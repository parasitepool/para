use {
    super::*,
    actor::{ClientActor, ClientMessage},
    error::ClientError,
    std::{
        collections::BTreeMap,
        sync::Arc,
        time::{Duration, Instant},
    },
    tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter},
        net::TcpStream,
        sync::{broadcast, mpsc, oneshot},
    },
    tracing::{debug, error, warn},
};

mod actor;
mod error;

pub type Result<T = (), E = ClientError> = std::result::Result<T, E>;

const CHANNEL_BUFFER_SIZE: usize = 32;

#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub address: String,
    pub username: String,
    pub user_agent: String,
    pub password: Option<String>,
    pub timeout: Duration,
}

#[derive(Clone)]
pub struct Client {
    config: Arc<ClientConfig>,
    tx: mpsc::Sender<ClientMessage>,
    events: broadcast::Sender<Event>,
}

impl Client {
    pub fn new(config: ClientConfig) -> Self {
        let (tx, rx) = mpsc::channel(CHANNEL_BUFFER_SIZE);
        let (event_tx, _event_rx) = broadcast::channel(CHANNEL_BUFFER_SIZE);

        let config = Arc::new(config);
        let actor = ClientActor::new(config.clone(), rx, event_tx.clone());

        tokio::spawn(async move {
            actor.run().await;
        });

        Self {
            config,
            tx,
            events: event_tx,
        }
    }

    pub async fn connect(&self) -> Result<broadcast::Receiver<Event>> {
        let (respond_to, rx) = oneshot::channel();

        self.tx
            .send(ClientMessage::Connect { respond_to })
            .await
            .map_err(|_| ClientError::NotConnected)?;

        rx.await.map_err(|_| ClientError::NotConnected)??;

        Ok(self.events.subscribe())
    }

    pub async fn disconnect(&self) -> Result {
        let (respond_to, rx) = oneshot::channel();

        let _ = self.tx.send(ClientMessage::Disconnect { respond_to }).await;

        let _ = rx.await;

        Ok(())
    }

    async fn send_request(
        &self,
        method: String,
        params: Value,
    ) -> Result<(oneshot::Receiver<Result<(Message, usize)>>, Instant)> {
        let (respond_to, rx) = oneshot::channel();
        let instant = Instant::now();

        self.tx
            .send(ClientMessage::Request {
                method,
                params,
                respond_to,
            })
            .await
            .map_err(|_| ClientError::NotConnected)?;

        Ok((rx, instant))
    }

    async fn await_response(
        &self,
        rx: oneshot::Receiver<Result<(Message, usize)>>,
        instant: Instant,
    ) -> Result<(Message, usize, Duration)> {
        let (message, bytes_read) = tokio::time::timeout(self.config.timeout, rx)
            .await
            .map_err(|source| ClientError::Timeout { source })?
            .map_err(|e| ClientError::ChannelRecv { source: e })??;

        Ok((message, bytes_read, instant.elapsed()))
    }

    fn handle_response(&self, message: Message, method: &str) -> Result<Value> {
        match message {
            Message::Response {
                result: Some(result),
                error: None,
                reject_reason: None,
                ..
            } => Ok(result),
            Message::Response {
                error: Some(err), ..
            } => Err(ClientError::Protocol {
                message: format!("{method} error: {err}"),
            }),
            Message::Response {
                reject_reason: Some(reason),
                ..
            } => Err(ClientError::Protocol {
                message: format!("{method} rejected: {reason}"),
            }),
            _ => Err(ClientError::Protocol {
                message: format!("Unhandled {method} response"),
            }),
        }
    }

    pub async fn configure(
        &self,
        extensions: Vec<String>,
        version_rolling_mask: Option<Version>,
    ) -> Result<(Value, Duration, usize)> {
        let (rx, instant) = self
            .send_request(
                "mining.configure".to_string(),
                serde_json::to_value(Configure {
                    extensions,
                    minimum_difficulty_value: None,
                    version_rolling_mask,
                    version_rolling_min_bit_count: None,
                })
                .context(error::SerializationSnafu)?,
            )
            .await?;

        let (message, bytes_read, duration) = self.await_response(rx, instant).await?;
        let result = self.handle_response(message, "mining.configure")?;

        Ok((result, duration, bytes_read))
    }

    pub async fn subscribe(&self) -> Result<(SubscribeResult, Duration, usize)> {
        let (rx, instant) = self
            .send_request(
                "mining.subscribe".to_string(),
                serde_json::to_value(Subscribe {
                    user_agent: self.config.user_agent.clone(),
                    extranonce1: None,
                })
                .context(error::SerializationSnafu)?,
            )
            .await?;

        let (message, bytes_read, duration) = self.await_response(rx, instant).await?;
        let result = self.handle_response(message, "mining.subscribe")?;

        Ok((
            serde_json::from_value(result).context(error::SerializationSnafu)?,
            duration,
            bytes_read,
        ))
    }

    pub async fn authorize(&self) -> Result<(Duration, usize)> {
        let (rx, instant) = self
            .send_request(
                "mining.authorize".to_string(),
                serde_json::to_value(Authorize {
                    username: self.config.username.clone(),
                    password: self.config.password.clone().or(Some("x".to_string())),
                })
                .context(error::SerializationSnafu)?,
            )
            .await?;

        let (message, bytes_read, duration) = self.await_response(rx, instant).await?;
        let result = self.handle_response(message, "mining.authorize")?;

        if serde_json::from_value(result).context(error::SerializationSnafu)? {
            Ok((duration, bytes_read))
        } else {
            Err(ClientError::Protocol {
                message: "Unauthorized".to_string(),
            })
        }
    }

    pub async fn submit(
        &self,
        job_id: JobId,
        extranonce2: Extranonce,
        ntime: Ntime,
        nonce: Nonce,
    ) -> Result<Submit> {
        let submit = Submit {
            username: self.config.username.clone(),
            job_id,
            extranonce2,
            ntime,
            nonce,
            version_bits: None,
        };

        let (rx, instant) = self
            .send_request(
                "mining.submit".to_string(),
                serde_json::to_value(&submit).context(error::SerializationSnafu)?,
            )
            .await?;

        let (message, _, _) = self.await_response(rx, instant).await?;
        let result = self.handle_response(message, "mining.submit")?;

        if !serde_json::from_value::<bool>(result).context(error::SerializationSnafu)? {
            return Err(ClientError::Protocol {
                message: "Server returned false for submit".to_string(),
            });
        }

        Ok(submit)
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        std::net::SocketAddr,
        tokio::{
            io::{AsyncReadExt, BufReader},
            net::TcpListener,
        },
    };

    async fn mock_server(drop_after_read: bool) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut reader = BufReader::new(socket);
            let mut buf = [0u8; 1024];

            if drop_after_read {
                let _ = reader.read(&mut buf).await;
            } else {
                loop {
                    match reader.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(_) => continue,
                        Err(_) => break,
                    }
                }
            }
        });

        addr
    }

    #[tokio::test]
    async fn request_timeout() {
        let addr = mock_server(false).await;

        let config = ClientConfig {
            address: addr.to_string(),
            username: "test".into(),
            user_agent: "test".into(),
            password: None,
            timeout: Duration::from_millis(200),
        };

        let client = Client::new(config);
        client.connect().await.unwrap();

        let err = client.subscribe().await.unwrap_err();
        assert!(
            matches!(err, ClientError::Timeout { .. }),
            "Expected Timeout error, got: {:?}",
            err
        );
    }

    #[tokio::test]
    async fn connection_timeout() {
        let config = ClientConfig {
            address: "10.255.255.1:9999".into(),
            username: "test".into(),
            user_agent: "test".into(),
            password: None,
            timeout: Duration::from_millis(200),
        };

        let client = Client::new(config);
        let err = client.connect().await.unwrap_err();
        assert!(
            matches!(err, ClientError::Timeout { .. }),
            "Expected Timeout error, got: {:?}",
            err
        );
    }

    #[tokio::test]
    async fn request_fails_fast() {
        let config = ClientConfig {
            address: "127.0.0.1:9999".into(),
            username: "test".into(),
            user_agent: "test".into(),
            password: None,
            timeout: Duration::from_secs(1),
        };

        let client = Client::new(config);

        let err = client.subscribe().await.unwrap_err();
        assert!(
            matches!(err, ClientError::NotConnected),
            "Expected NotConnected error, got: {:?}",
            err
        );
    }

    #[tokio::test]
    async fn detect_connection_drop() {
        let addr = mock_server(true).await;

        let config = ClientConfig {
            address: addr.to_string(),
            username: "test".into(),
            user_agent: "test".into(),
            password: None,
            timeout: Duration::from_secs(5),
        };

        let client = Client::new(config);
        client.connect().await.unwrap();

        let err = client.subscribe().await.unwrap_err();
        assert!(
            matches!(err, ClientError::NotConnected),
            "Expected NotConnected, got: {:?}",
            err
        );
    }
}
