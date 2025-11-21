use crate::USER_AGENT;
use {
    super::*,
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

mod error;

pub type Result<T = (), E = ClientError> = std::result::Result<T, E>;

#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub address: String,
    pub username: String,
    pub password: Option<String>,
    pub timeout: Duration,
}

#[derive(Clone)]
pub struct Client {
    config: Arc<ClientConfig>,
    id_counter: Arc<AtomicU64>,
    tx: mpsc::Sender<ActorMessage>,
    pub events: tokio::sync::broadcast::Sender<StratumEvent>,
}

enum ActorMessage {
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
        let (tx, _) = mpsc::channel(32); // Buffer for outgoing requests
        let (events, _) = tokio::sync::broadcast::channel(32);

        Self {
            config: Arc::new(config),
            id_counter: Arc::new(AtomicU64::new(0)),
            tx,
            events,
        }
    }

    pub async fn connect(&mut self) -> Result<()> {
        // If there's an existing connection, we might want to ensure it's dead or just spawn a new one.
        // The `tx` held by `Client` points to the old channel if we don't update it.
        // So we need to create a new channel and spawn a new actor.

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

        // Perform handshake
        let (_subscribe, _, _) = self.subscribe(USER_AGENT.into()).await?;
        self.authorize().await?;

        Ok(())
    }

    pub async fn disconnect(&self) -> Result<()> {
        let _ = self.tx.send(ActorMessage::Disconnect).await;
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
            .send(ActorMessage::Request {
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

    // API Methods (delegating to actor)

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

    pub async fn subscribe(
        &self,
        user_agent: String,
    ) -> Result<(SubscribeResult, Duration, usize)> {
        let (rx, instant) = self
            .send_request(
                "mining.subscribe",
                serde_json::to_value(Subscribe {
                    user_agent,
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

struct Connection {
    config: Arc<ClientConfig>,
    rx: mpsc::Receiver<ActorMessage>,
    events: tokio::sync::broadcast::Sender<StratumEvent>,
    pending: BTreeMap<Id, oneshot::Sender<Result<(Message, usize)>>>,
}

impl Connection {
    fn new(
        config: Arc<ClientConfig>,
        rx: mpsc::Receiver<ActorMessage>,
        events: tokio::sync::broadcast::Sender<StratumEvent>,
    ) -> Self {
        Self {
            config,
            rx,
            events,
            pending: BTreeMap::new(),
        }
    }

    async fn run(mut self) -> Result<()> {
        let stream = tokio::time::timeout(
            self.config.timeout,
            TcpStream::connect(&self.config.address),
        )
        .await
        .context(error::TimeoutSnafu)?
        .context(error::IoSnafu)?;

        let (reader, writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let mut writer = BufWriter::new(writer);

        let mut line = String::new();

        loop {
            line.clear();

            tokio::select! {
                // Handle outgoing requests from Client
                msg = self.rx.recv() => {
                    match msg {
                        Some(ActorMessage::Request { id, method, params, tx }) => {
                            let msg = Message::Request {
                                id: id.clone(),
                                method,
                                params,
                            };
                            let frame = match serde_json::to_string(&msg) {
                                Ok(f) => f + "\n",
                                Err(e) => {
                                    let _ = tx.send(Err(ClientError::Serialization { source: e }));
                                    continue;
                                }
                            };

                            if let Err(e) = writer.write_all(frame.as_bytes()).await {
                                let _ = tx.send(Err(ClientError::Io { source: e }));
                                // Connection dead
                                break;
                            }
                            if let Err(e) = writer.flush().await {
                                let _ = tx.send(Err(ClientError::Io { source: e }));
                                break;
                            }

                            self.pending.insert(id, tx);
                        }
                        Some(ActorMessage::Disconnect) => {
                            break;
                        }
                        None => {
                            // Client dropped
                            break;
                        }
                    }
                }

                // Handle incoming messages from TCP
                read_result = reader.read_line(&mut line) => {
                    let bytes_read = match read_result {
                        Ok(0) => break, // EOF
                        Ok(n) => n,
                        Err(e) => {
                            error!("Read error: {e}");
                            break;
                        }
                    };

                    let msg: Message = match serde_json::from_str(&line) {
                        Ok(msg) => msg,
                        Err(e) => {
                            warn!("Invalid JSON message: {line:?} - {e}");
                            continue;
                        }
                    };

                    match msg {
                        Message::Response { id, result, error, reject_reason } => {
                            if let Some(tx) = self.pending.remove(&id) {
                                let _ = tx.send(Ok((
                                    Message::Response { id, result, error, reject_reason },
                                    bytes_read
                                )));
                            } else {
                                warn!("Unmatched response ID={id}: {line}");
                            }
                        }
                        Message::Notification { method, params } => {
                            self.handle_notification(method, params).await;
                        }
                        _ => {
                             warn!("Unexpected message type: {:?}", msg);
                        }
                    }
                }
            }
        }

        // Cleanup: notify pending requests
        // We iterate by removing all items to avoid instability issues with drain
        let pending = std::mem::take(&mut self.pending);
        for (_, tx) in pending {
            let _ = tx.send(Err(ClientError::NotConnected));
        }

        // Notify disconnection
        let _ = self.events.send(StratumEvent::Disconnected);

        Ok(())
    }

    async fn handle_notification(&self, method: String, params: Value) {
        match method.as_str() {
            "mining.notify" => match serde_json::from_value::<Notify>(params) {
                Ok(notify) => {
                    let _ = self.events.send(StratumEvent::Notify(notify));
                }
                Err(e) => warn!("Failed to parse mining.notify: {}", e),
            },
            "mining.set_difficulty" => match serde_json::from_value::<SetDifficulty>(params) {
                Ok(set_diff) => {
                    let _ = self
                        .events
                        .send(StratumEvent::SetDifficulty(set_diff.difficulty()));
                }
                Err(e) => warn!("Failed to parse mining.set_difficulty: {}", e),
            },
            _ => warn!("Unhandled notification: {}", method),
        }
    }
}
