use {
    super::*,
    error::ClientError,
    std::{
        collections::BTreeMap,
        time::{Duration, Instant},
    },
    tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter},
        net::TcpStream,
        sync::{mpsc, oneshot},
    },
    tracing::{debug, error, warn},
};

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

// ============================================================================
// Handle - what the user holds and uses to talk to the actor
// ============================================================================

#[derive(Clone)]
pub struct Client {
    tx: mpsc::Sender<ClientMessage>,
    pub events: tokio::sync::broadcast::Sender<Event>,
}

// Messages sent from the handle to the actor
enum ClientMessage {
    Connect {
        respond_to: oneshot::Sender<Result<()>>,
    },
    Request {
        method: String,
        params: Value,
        respond_to: oneshot::Sender<Result<(Message, usize)>>,
    },
    Disconnect {
        respond_to: oneshot::Sender<()>,
    },
}

// ============================================================================
// Actor - the independently running task that owns all state
// ============================================================================

struct ClientActor {
    config: ClientConfig,
    rx: mpsc::Receiver<ClientMessage>,
    events: tokio::sync::broadcast::Sender<Event>,
    id_counter: u64,
    pending: BTreeMap<Id, oneshot::Sender<Result<(Message, usize)>>>,
    connection: Option<ConnectionState>,
}

struct ConnectionState {
    writer: BufWriter<tokio::net::tcp::OwnedWriteHalf>,
    reader_handle: tokio::task::JoinHandle<()>,
}

// Messages sent from the reader task to the actor
enum IncomingMessage {
    Response {
        id: Id,
        message: Message,
        bytes_read: usize,
    },
    Notification {
        method: String,
        params: Value,
    },
    Disconnected,
    Error(ClientError),
}

// ============================================================================
// Client Handle Implementation
// ============================================================================

impl Client {
    pub fn new(config: ClientConfig) -> Self {
        let (tx, rx) = mpsc::channel(32);
        let (event_tx, _event_rx) = tokio::sync::broadcast::channel(32);

        let actor = ClientActor::new(config, rx, event_tx.clone());

        // Spawn the actor immediately - it starts in disconnected state
        tokio::spawn(async move {
            actor.run().await;
        });

        Self {
            tx,
            events: event_tx,
        }
    }

    pub async fn connect(&self) -> Result<()> {
        let (respond_to, rx) = oneshot::channel();

        self.tx
            .send(ClientMessage::Connect { respond_to })
            .await
            .map_err(|_| ClientError::NotConnected)?;

        rx.await.map_err(|_| ClientError::NotConnected)?
    }

    pub async fn disconnect(&self) -> Result<()> {
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
                serde_json::json!(null), // Will be populated by actor from config
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
                serde_json::json!(null), // Will be populated by actor from config
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
            username: String::new(), // Will be populated by actor from config
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

// ============================================================================
// ClientActor Implementation
// ============================================================================

impl ClientActor {
    fn new(
        config: ClientConfig,
        rx: mpsc::Receiver<ClientMessage>,
        events: tokio::sync::broadcast::Sender<Event>,
    ) -> Self {
        Self {
            config,
            rx,
            events,
            id_counter: 0,
            pending: BTreeMap::new(),
            connection: None,
        }
    }

    async fn run(mut self) {
        // Main actor loop - processes messages from client handle and reader task
        let (incoming_tx, mut incoming_rx) = mpsc::channel::<IncomingMessage>(32);

        loop {
            tokio::select! {
                // Messages from the client handle
                Some(msg) = self.rx.recv() => {
                    match msg {
                        ClientMessage::Connect { respond_to } => {
                            let result = self.handle_connect(incoming_tx.clone()).await;
                            let _ = respond_to.send(result);
                        }
                        ClientMessage::Request { method, params, respond_to } => {
                            // Assign actual ID from counter
                            let actual_id = self.next_id();
                            self.pending.insert(actual_id.clone(), respond_to);

                            if let Err(e) = self.handle_request(actual_id.clone(), method, params).await {
                                // If sending fails, notify the pending request
                                if let Some(tx) = self.pending.remove(&actual_id) {
                                    let _ = tx.send(Err(e));
                                }
                            }
                        }
                        ClientMessage::Disconnect { respond_to } => {
                            self.handle_disconnect().await;
                            let _ = respond_to.send(());
                        }
                    }
                }
                // Messages from the reader task
                Some(msg) = incoming_rx.recv() => {
                    self.handle_incoming(msg).await;
                }
                // All senders dropped - shutdown
                else => {
                    debug!("Client actor shutting down");
                    self.handle_disconnect().await;
                    break;
                }
            }
        }
    }

    fn next_id(&mut self) -> Id {
        let id = self.id_counter;
        self.id_counter += 1;
        Id::Number(id)
    }

    async fn handle_connect(&mut self, incoming_tx: mpsc::Sender<IncomingMessage>) -> Result<()> {
        // Disconnect if already connected
        if self.connection.is_some() {
            self.handle_disconnect().await;
        }

        // Establish TCP connection
        let stream = tokio::time::timeout(
            self.config.timeout,
            TcpStream::connect(&self.config.address),
        )
        .await
        .map_err(|source| ClientError::Timeout { source })?
        .map_err(|source| ClientError::Io { source })?;

        let (reader, writer) = stream.into_split();
        let writer = BufWriter::new(writer);

        // Spawn reader task
        let events = self.events.clone();
        let reader_handle = tokio::spawn(async move {
            Self::reader_task(BufReader::new(reader), incoming_tx, events).await;
        });

        self.connection = Some(ConnectionState {
            writer,
            reader_handle,
        });

        debug!("Connected to {}", self.config.address);
        Ok(())
    }

    async fn handle_request(&mut self, id: Id, method: String, mut params: Value) -> Result<()> {
        let connection = self.connection.as_mut().ok_or(ClientError::NotConnected)?;

        // Inject config values for specific methods
        match method.as_str() {
            "mining.subscribe" => {
                params = serde_json::to_value(Subscribe {
                    user_agent: self.config.user_agent.clone(),
                    extranonce1: None,
                })
                .map_err(|e| ClientError::Serialization { source: e })?;
            }
            "mining.authorize" => {
                params = serde_json::to_value(Authorize {
                    username: self.config.username.clone(),
                    password: self.config.password.clone().or(Some("x".to_string())),
                })
                .map_err(|e| ClientError::Serialization { source: e })?;
            }
            "mining.submit" => {
                // Inject username into submit
                if let Ok(mut submit) = serde_json::from_value::<Submit>(params.clone()) {
                    submit.username = self.config.username.clone();
                    params = serde_json::to_value(&submit)
                        .map_err(|e| ClientError::Serialization { source: e })?;
                }
            }
            _ => {}
        }

        let msg = Message::Request { id, method, params };

        let frame = serde_json::to_string(&msg)
            .map_err(|e| ClientError::Serialization { source: e })?
            + "\n";

        connection
            .writer
            .write_all(frame.as_bytes())
            .await
            .map_err(|e| ClientError::Io { source: e })?;

        connection
            .writer
            .flush()
            .await
            .map_err(|e| ClientError::Io { source: e })?;

        Ok(())
    }

    async fn handle_disconnect(&mut self) {
        if let Some(connection) = self.connection.take() {
            connection.reader_handle.abort();
            debug!("Disconnected");
        }

        // Cleanup pending requests
        let pending = std::mem::take(&mut self.pending);
        for (_, tx) in pending {
            let _ = tx.send(Err(ClientError::NotConnected));
        }

        let _ = self.events.send(Event::Disconnected);
    }

    async fn handle_incoming(&mut self, msg: IncomingMessage) {
        match msg {
            IncomingMessage::Response {
                id,
                message,
                bytes_read,
            } => {
                if let Some(tx) = self.pending.remove(&id) {
                    let _ = tx.send(Ok((message, bytes_read)));
                } else {
                    warn!("Unmatched response ID={:?}", id);
                }
            }
            IncomingMessage::Notification { method, params } => match method.as_str() {
                "mining.notify" => match serde_json::from_value::<Notify>(params) {
                    Ok(notify) => {
                        let _ = self.events.send(Event::Notify(notify));
                    }
                    Err(e) => warn!("Failed to parse mining.notify: {}", e),
                },
                "mining.set_difficulty" => match serde_json::from_value::<SetDifficulty>(params) {
                    Ok(set_diff) => {
                        let _ = self
                            .events
                            .send(Event::SetDifficulty(set_diff.difficulty()));
                    }
                    Err(e) => warn!("Failed to parse mining.set_difficulty: {}", e),
                },
                _ => warn!("Unhandled notification: {}", method),
            },
            IncomingMessage::Disconnected => {
                self.handle_disconnect().await;
            }
            IncomingMessage::Error(err) => {
                error!("Reader error: {}", err);
                self.handle_disconnect().await;
            }
        }
    }

    async fn reader_task(
        mut reader: BufReader<tokio::net::tcp::OwnedReadHalf>,
        incoming_tx: mpsc::Sender<IncomingMessage>,
        events: tokio::sync::broadcast::Sender<Event>,
    ) {
        let mut line = String::new();

        loop {
            line.clear();

            let bytes_read = match reader.read_line(&mut line).await {
                Ok(0) => {
                    let _ = incoming_tx.send(IncomingMessage::Disconnected).await;
                    let _ = events.send(Event::Disconnected);
                    break;
                }
                Ok(n) => n,
                Err(e) => {
                    error!("Read error: {e}");
                    let _ = incoming_tx
                        .send(IncomingMessage::Error(ClientError::Io { source: e }))
                        .await;
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
                Message::Response {
                    id,
                    result,
                    error,
                    reject_reason,
                } => {
                    let _ = incoming_tx
                        .send(IncomingMessage::Response {
                            id,
                            message: Message::Response {
                                id: Id::Number(0), // Placeholder
                                result,
                                error,
                                reject_reason,
                            },
                            bytes_read,
                        })
                        .await;
                }
                Message::Notification { method, params } => {
                    let _ = incoming_tx
                        .send(IncomingMessage::Notification { method, params })
                        .await;
                }
                _ => {
                    warn!("Unexpected message type: {:?}", msg);
                }
            }
        }
    }
}
