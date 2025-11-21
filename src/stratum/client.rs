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

        let _ = rx.await.map_err(|_| ClientError::NotConnected)?;

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
                "mining.subscribe".to_string(),
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
                "mining.authorize".to_string(),
                serde_json::to_value(Authorize {
                    username: self.config.username.clone(),
                    password: self.config.password.clone().or(Some("x".to_string())),
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
                "mining.submit".to_string(),
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
