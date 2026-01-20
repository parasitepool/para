use {
    super::*,
    actor::{ClientActor, ClientMessage},
    futures::StreamExt,
    std::{
        collections::HashMap,
        time::{Duration, Instant},
    },
    tokio::{
        io::{AsyncWriteExt, BufWriter},
        net::TcpStream,
        sync::{broadcast, mpsc, oneshot},
    },
    tokio_util::codec::{FramedRead, LinesCodec},
    tracing::{debug, error, warn},
};

pub use error::ClientError;

mod actor;
mod error;

pub type Result<T = (), E = ClientError> = std::result::Result<T, E>;

const CHANNEL_BUFFER_SIZE: usize = 256;

#[derive(Debug)]
pub struct EventReceiver {
    rx: broadcast::Receiver<Event>,
}

impl EventReceiver {
    pub async fn recv(&mut self) -> Result<Event> {
        match self.rx.recv().await {
            Ok(event) => Ok(event),
            Err(broadcast::error::RecvError::Closed) => Err(ClientError::EventChannelClosed),
            Err(broadcast::error::RecvError::Lagged(count)) => {
                Err(ClientError::EventsLagged { count })
            }
        }
    }

    pub fn try_recv(&mut self) -> Option<Result<Event>> {
        match self.rx.try_recv() {
            Ok(event) => Some(Ok(event)),
            Err(broadcast::error::TryRecvError::Empty) => None,
            Err(broadcast::error::TryRecvError::Closed) => {
                Some(Err(ClientError::EventChannelClosed))
            }
            Err(broadcast::error::TryRecvError::Lagged(count)) => {
                Some(Err(ClientError::EventsLagged { count }))
            }
        }
    }
}

#[derive(Clone)]
pub struct Client {
    #[allow(dead_code)]
    address: String,
    pub username: Username,
    password: Option<String>,
    user_agent: String,
    timeout: Duration,
    tx: mpsc::Sender<ClientMessage>,
    events: broadcast::Sender<Event>,
}

impl Client {
    #[must_use]
    pub fn new(
        address: String,
        username: Username,
        password: Option<String>,
        user_agent: String,
        timeout: Duration,
    ) -> Self {
        let (tx, rx) = mpsc::channel(CHANNEL_BUFFER_SIZE);
        let (event_tx, _event_rx) = broadcast::channel(CHANNEL_BUFFER_SIZE);

        let actor = ClientActor::new(address.clone(), timeout, rx, event_tx.clone());

        tokio::spawn(async move {
            actor.run().await;
        });

        Self {
            address,
            username,
            password,
            user_agent,
            timeout,
            tx,
            events: event_tx,
        }
    }

    pub async fn connect(&self) -> Result<EventReceiver> {
        let (respond_to, rx) = oneshot::channel();

        self.tx
            .send(ClientMessage::Connect { respond_to })
            .await
            .map_err(|_| ClientError::NotConnected)?;

        rx.await.map_err(|_| ClientError::NotConnected)??;

        Ok(EventReceiver {
            rx: self.events.subscribe(),
        })
    }

    pub async fn disconnect(&self) {
        let (respond_to, rx) = oneshot::channel();

        if self
            .tx
            .send(ClientMessage::Disconnect { respond_to })
            .await
            .is_err()
        {
            debug!("Disconnect send failed: actor already shut down");
            return;
        }

        if rx.await.is_err() {
            debug!("Disconnect response failed: actor shut down during disconnect");
        }
    }

    async fn send_request(
        &self,
        method: &'static str,
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
        let (message, bytes_read) = tokio::time::timeout(self.timeout, rx)
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
            } => Err(ClientError::Stratum { response: err }),
            Message::Response {
                reject_reason: Some(reason),
                ..
            } => Err(ClientError::Rejected {
                method: method.to_owned(),
                reason,
            }),
            _ => Err(ClientError::UnhandledResponse {
                method: method.to_owned(),
            }),
        }
    }

    pub async fn configure(
        &self,
        extensions: Vec<String>,
        version_rolling_mask: Option<Version>,
    ) -> Result<(ConfigureResponse, Duration, usize)> {
        let (rx, instant) = self
            .send_request(
                "mining.configure",
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

        let response: ConfigureResponse =
            serde_json::from_value(result).context(error::SerializationSnafu)?;

        Ok((response, duration, bytes_read))
    }

    pub async fn subscribe(&self) -> Result<(SubscribeResult, Duration, usize)> {
        self.subscribe_with_enonce1(None).await
    }

    pub async fn subscribe_with_enonce1(
        &self,
        enonce1: Option<Extranonce>,
    ) -> Result<(SubscribeResult, Duration, usize)> {
        let (rx, instant) = self
            .send_request(
                "mining.subscribe",
                serde_json::to_value(Subscribe {
                    user_agent: self.user_agent.clone(),
                    enonce1,
                })
                .context(error::SerializationSnafu)?,
            )
            .await?;

        let (message, bytes_read, duration) = self.await_response(rx, instant).await?;
        let result = self.handle_response(message, "mining.subscribe")?;

        let subscribe_result: SubscribeResult =
            serde_json::from_value(result).context(error::SerializationSnafu)?;

        Ok((subscribe_result, duration, bytes_read))
    }

    pub async fn authorize(&self) -> Result<(Duration, usize)> {
        let (rx, instant) = self
            .send_request(
                "mining.authorize",
                serde_json::to_value(Authorize {
                    username: self.username.clone(),
                    password: self.password.clone().or(Some("x".to_string())),
                })
                .context(error::SerializationSnafu)?,
            )
            .await?;

        let (message, bytes_read, duration) = self.await_response(rx, instant).await?;
        let result = self.handle_response(message, "mining.authorize")?;

        if serde_json::from_value(result).context(error::SerializationSnafu)? {
            Ok((duration, bytes_read))
        } else {
            Err(ClientError::Stratum {
                response: StratumError::Unauthorized.into_response(None),
            })
        }
    }

    pub async fn submit(
        &self,
        job_id: JobId,
        enonce2: Extranonce,
        ntime: Ntime,
        nonce: Nonce,
        version_bits: Option<Version>,
    ) -> Result<Submit> {
        self.submit_with_username(
            self.username.clone(),
            job_id,
            enonce2,
            ntime,
            nonce,
            version_bits,
        )
        .await
    }

    pub async fn submit_with_username(
        &self,
        username: Username,
        job_id: JobId,
        enonce2: Extranonce,
        ntime: Ntime,
        nonce: Nonce,
        version_bits: Option<Version>,
    ) -> Result<Submit> {
        let submit = Submit {
            username,
            job_id,
            enonce2,
            ntime,
            nonce,
            version_bits,
        };

        let (rx, instant) = self
            .send_request(
                "mining.submit",
                serde_json::to_value(&submit).context(error::SerializationSnafu)?,
            )
            .await?;

        let (message, _, _) = self.await_response(rx, instant).await?;
        let result = self.handle_response(message, "mining.submit")?;

        if !serde_json::from_value::<bool>(result).context(error::SerializationSnafu)? {
            return Err(ClientError::SubmitFalse);
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

        let client = Client::new(
            addr.to_string(),
            "test".into(),
            None,
            "test".into(),
            Duration::from_millis(200),
        );
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
        let client = Client::new(
            "10.255.255.1:9999".into(),
            "test".into(),
            None,
            "test".into(),
            Duration::from_millis(200),
        );
        let err = client.connect().await.unwrap_err();
        assert!(
            matches!(err, ClientError::Timeout { .. }),
            "Expected Timeout error, got: {:?}",
            err
        );
    }

    #[tokio::test]
    async fn request_fails_fast() {
        let client = Client::new(
            "127.0.0.1:9999".into(),
            "test".into(),
            None,
            "test".into(),
            Duration::from_secs(1),
        );

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

        let client = Client::new(
            addr.to_string(),
            "test".into(),
            None,
            "test".into(),
            Duration::from_secs(5),
        );
        client.connect().await.unwrap();

        let err = client.subscribe().await.unwrap_err();
        assert!(
            matches!(err, ClientError::NotConnected),
            "Expected NotConnected, got: {:?}",
            err
        );
    }
}
