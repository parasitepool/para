use {
    super::*,
    connection::Connection,
    error::ClientError,
    std::{
        collections::BTreeMap,
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        },
        time::{Duration, Instant},
    },
    tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter},
        net::TcpStream,
        sync::{mpsc, oneshot},
    },
    tracing::{error, warn},
};

mod connection;
mod error;

pub type Result<T = (), E = ClientError> = std::result::Result<T, E>;

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
    id_counter: Arc<AtomicU64>,
    tx: mpsc::Sender<ClientMessage>,
    pub events: tokio::sync::broadcast::Sender<Event>,
}

enum ClientMessage {
    Request {
        id: Id,
        method: String,
        params: Value,
        tx: oneshot::Sender<Result<(Message, usize)>>,
    },
    Disconnect,
}

impl Client {
    pub fn new(config: ClientConfig) -> Self {
        let (tx, _) = mpsc::channel(32);
        let (events, _) = tokio::sync::broadcast::channel(32);

        Self {
            config: Arc::new(config),
            id_counter: Arc::new(AtomicU64::new(0)),
            tx,
            events,
        }
    }

    pub async fn connect(&mut self) -> Result<()> {
        let (tx, rx) = mpsc::channel(32);
        self.tx = tx;

        let connection = Connection::new(self.config.clone(), rx, self.events.clone());

        tokio::spawn(async move {
            if let Err(e) = connection.run().await {
                error!("Connection actor failed: {}", e);
            }
        });

        Ok(())
    }

    pub async fn reconnect(&mut self) -> Result<()> {
        self.disconnect().await?;
        self.connect().await?;

        let (_subscribe, _, _) = self.subscribe().await?;
        self.authorize().await?;

        Ok(())
    }

    pub async fn disconnect(&self) -> Result<()> {
        let _ = self.tx.send(ClientMessage::Disconnect).await;
        Ok(())
    }

    async fn send_request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<(oneshot::Receiver<Result<(Message, usize)>>, Instant)> {
        let id = self.next_id();
        let (tx, rx) = oneshot::channel();

        let instant = Instant::now();

        self.tx
            .send(ClientMessage::Request {
                id,
                method: method.to_string(),
                params,
                tx,
            })
            .await
            .map_err(|_| ClientError::NotConnected)?;

        Ok((rx, instant))
    }

    fn next_id(&self) -> Id {
        Id::Number(self.id_counter.fetch_add(1, Ordering::Relaxed))
    }

    pub async fn configure(
        &self,
        extensions: Vec<String>,
        version_rolling_mask: Option<Version>,
    ) -> Result<(Value, Duration, usize)> {
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

        let (message, bytes_read) = rx
            .await
            .map_err(|e| ClientError::ChannelRecv { source: e })??;

        let duration = instant.elapsed();

        match message {
            Message::Response {
                result: Some(result),
                error: None,
                ..
            } => Ok((result, duration, bytes_read)),
            Message::Response {
                error: Some(err), ..
            } => Err(ClientError::Protocol {
                message: format!("mining.configure error: {}", err),
            }),
            _ => Err(ClientError::Protocol {
                message: "Unhandled error in mining.configure".to_string(),
            }),
        }
    }

    pub async fn subscribe(&self) -> Result<(SubscribeResult, Duration, usize)> {
        let (rx, instant) = self
            .send_request(
                "mining.subscribe",
                serde_json::to_value(Subscribe {
                    user_agent: self.config.user_agent.clone(),
                    extranonce1: None,
                })
                .context(error::SerializationSnafu)?,
            )
            .await?;

        let (message, bytes_read) = rx
            .await
            .map_err(|e| ClientError::ChannelRecv { source: e })??;

        let duration = instant.elapsed();

        match message {
            Message::Response {
                result: Some(result),
                error: None,
                ..
            } => Ok((
                serde_json::from_value(result).context(error::SerializationSnafu)?,
                duration,
                bytes_read,
            )),
            Message::Response {
                error: Some(err), ..
            } => Err(ClientError::Protocol {
                message: format!("mining.subscribe error: {}", err),
            }),
            _ => Err(ClientError::Protocol {
                message: "Unknown mining.subscribe error".to_string(),
            }),
        }
    }

    pub async fn authorize(&self) -> Result<(Duration, usize)> {
        let (rx, instant) = self
            .send_request(
                "mining.authorize",
                serde_json::to_value(Authorize {
                    username: self.config.username.clone(),
                    password: Some(
                        self.config
                            .password
                            .clone()
                            .unwrap_or_else(|| "x".to_string()),
                    ),
                })
                .context(error::SerializationSnafu)?,
            )
            .await?;

        let (message, bytes_read) = rx
            .await
            .map_err(|e| ClientError::ChannelRecv { source: e })??;

        let duration = instant.elapsed();

        match message {
            Message::Response {
                result: Some(result),
                error: None,
                ..
            } => {
                if serde_json::from_value(result).context(error::SerializationSnafu)? {
                    Ok((duration, bytes_read))
                } else {
                    Err(ClientError::Protocol {
                        message: "Unauthorized".to_string(),
                    })
                }
            }
            Message::Response {
                error: Some(err), ..
            } => Err(ClientError::Protocol {
                message: format!("mining.authorize error: {}", err),
            }),
            _ => Err(ClientError::Protocol {
                message: "Unknown mining.authorize error".to_string(),
            }),
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

        let (rx, _) = self
            .send_request(
                "mining.submit",
                serde_json::to_value(&submit).context(error::SerializationSnafu)?,
            )
            .await?;

        let (message, _) = rx
            .await
            .map_err(|e| ClientError::ChannelRecv { source: e })??;

        match message {
            Message::Response {
                result: Some(result),
                error: None,
                reject_reason: None,
                ..
            } => {
                if let Err(err) = serde_json::from_value::<Value>(result) {
                    return Err(ClientError::Protocol {
                        message: format!("Failed to submit: {err}"),
                    });
                }
            }
            Message::Response {
                error: Some(err), ..
            } => {
                return Err(ClientError::Protocol {
                    message: format!("mining.submit error: {}", err),
                });
            }
            Message::Response {
                reject_reason: Some(reason),
                ..
            } => {
                return Err(ClientError::Protocol {
                    message: format!("share rejected: {}", reason),
                });
            }
            _ => {
                return Err(ClientError::Protocol {
                    message: "Unhandled error in mining.submit".to_string(),
                });
            }
        }

        Ok(submit)
    }
}
