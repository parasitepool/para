use {
    super::*,
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
        net::{
            TcpStream,
            tcp::{OwnedReadHalf, OwnedWriteHalf},
        },
        sync::{Mutex, mpsc, oneshot},
    },
    tracing::{debug, error, warn},
};

mod error;

pub use error::{ClientError, DisconnectReason};

pub type Result<T = (), E = ClientError> = std::result::Result<T, E>;

type Pending = Arc<Mutex<BTreeMap<Id, oneshot::Sender<(Message, usize, Duration)>>>>;

/// Client handle for sending commands to the connection
#[derive(Clone)]
pub struct Client {
    command_tx: mpsc::Sender<Command>,
    id_counter: Arc<AtomicU64>,
    username: String,
    password: String,
}

/// Connection that drives the event loop
pub struct Connection {
    command_rx: mpsc::Receiver<Command>,
    event_tx: mpsc::Sender<Event>,
    tcp_reader: BufReader<OwnedReadHalf>,
    tcp_writer: BufWriter<OwnedWriteHalf>,
    pending: Pending,
}

/// Events from the stratum connection
#[derive(Debug)]
pub enum Event {
    /// Notification from server (mining.notify, mining.set_difficulty, etc)
    Notification {
        method: String,
        params: serde_json::Value,
    },
    /// Connection was disconnected
    Disconnected { reason: DisconnectReason },
}

/// Commands to send to the connection
enum Command {
    SendRequest {
        id: Id,
        method: String,
        params: serde_json::Value,
        response_tx: oneshot::Sender<(Message, usize, Duration)>,
    },
    Disconnect,
}

impl Client {
    /// Connect to a stratum server
    /// Returns (Client, Connection, EventReceiver)
    pub async fn connect(
        address: impl tokio::net::ToSocketAddrs,
        username: String,
        password: Option<String>,
        timeout: Duration,
    ) -> Result<(Self, Connection, mpsc::Receiver<Event>)> {
        let stream = tokio::time::timeout(timeout, TcpStream::connect(address))
            .await
            .map_err(|e| ClientError::Timeout { source: e })?
            .map_err(|e| ClientError::Io { source: e })?;

        let (tcp_reader, tcp_writer) = {
            let (rx, tx) = stream.into_split();
            (BufReader::new(rx), BufWriter::new(tx))
        };

        let (command_tx, command_rx) = mpsc::channel(32);
        let (event_tx, event_rx) = mpsc::channel(32);

        let pending: Pending = Arc::new(Mutex::new(BTreeMap::new()));

        let connection = Connection {
            command_rx,
            event_tx,
            tcp_reader,
            tcp_writer,
            pending: pending.clone(),
        };

        let client = Self {
            command_tx,
            id_counter: Arc::new(AtomicU64::new(0)),
            username,
            password: password.unwrap_or_else(|| "x".to_string()),
        };

        Ok((client, connection, event_rx))
    }

    /// Configure mining extensions
    pub async fn configure(
        &mut self,
        extensions: Vec<String>,
        version_rolling_mask: Option<Version>,
    ) -> Result<(serde_json::Value, Duration, usize)> {
        let (response, bytes_read, duration) = self
            .send_request(
                "mining.configure",
                serde_json::to_value(Configure {
                    extensions,
                    minimum_difficulty_value: None,
                    version_rolling_mask,
                    version_rolling_min_bit_count: None,
                })
                .map_err(|e| ClientError::Serialization { source: e })?,
            )
            .await?;

        match response {
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

    /// Subscribe to mining notifications
    pub async fn subscribe(
        &mut self,
        user_agent: String,
    ) -> Result<(SubscribeResult, Duration, usize)> {
        let (response, bytes_read, duration) = self
            .send_request(
                "mining.subscribe",
                serde_json::to_value(Subscribe {
                    user_agent,
                    extranonce1: None,
                })
                .map_err(|e| ClientError::Serialization { source: e })?,
            )
            .await?;

        match response {
            Message::Response {
                result: Some(result),
                error: None,
                ..
            } => Ok((
                serde_json::from_value(result)
                    .map_err(|e| ClientError::Serialization { source: e })?,
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

    /// Authorize with username and password
    pub async fn authorize(&mut self) -> Result<(Duration, usize)> {
        let (response, bytes_read, duration) = self
            .send_request(
                "mining.authorize",
                serde_json::to_value(Authorize {
                    username: self.username.clone(),
                    password: Some(self.password.clone()),
                })
                .map_err(|e| ClientError::Serialization { source: e })?,
            )
            .await?;

        match response {
            Message::Response {
                result: Some(result),
                error: None,
                ..
            } => {
                if serde_json::from_value(result)
                    .map_err(|e| ClientError::Serialization { source: e })?
                {
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

    /// Submit a share
    pub async fn submit(
        &mut self,
        job_id: JobId,
        extranonce2: Extranonce,
        ntime: Ntime,
        nonce: Nonce,
    ) -> Result<Submit> {
        let submit = Submit {
            username: self.username.clone(),
            job_id,
            extranonce2,
            ntime,
            nonce,
            version_bits: None,
        };

        let (response, _, _) = self
            .send_request(
                "mining.submit",
                serde_json::to_value(&submit)
                    .map_err(|e| ClientError::Serialization { source: e })?,
            )
            .await?;

        match response {
            Message::Response {
                result: Some(result),
                error: None,
                reject_reason: None,
                ..
            } => {
                if let Err(err) = serde_json::from_value::<serde_json::Value>(result) {
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

    /// Request graceful disconnect
    pub async fn disconnect(&self) -> Result {
        self.command_tx
            .send(Command::Disconnect)
            .await
            .map_err(|_| ClientError::ChannelSend)
    }

    async fn send_request(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<(Message, usize, Duration)> {
        let id = self.next_id();

        let (tx, rx) = oneshot::channel();

        self.command_tx
            .send(Command::SendRequest {
                id,
                method: method.to_string(),
                params,
                response_tx: tx,
            })
            .await
            .map_err(|_| ClientError::ChannelSend)?;

        rx.await.map_err(|e| ClientError::ChannelRecv { source: e })
    }

    fn next_id(&mut self) -> Id {
        Id::Number(self.id_counter.fetch_add(1, Ordering::Relaxed))
    }
}

impl Connection {
    /// Run the connection event loop until disconnection or error
    pub async fn run(mut self) -> Result<()> {
        let mut line = String::new();

        loop {
            tokio::select! {
                // Handle incoming data from server
                result = self.tcp_reader.read_line(&mut line) => {
                    match result {
                        Ok(0) => {
                            // Server closed connection
                            let _ = self.event_tx.send(Event::Disconnected {
                                reason: DisconnectReason::ServerClosed,
                            }).await;
                            return Err(ClientError::Disconnected {
                                reason: DisconnectReason::ServerClosed,
                            });
                        }
                        Ok(n) => {
                            if let Err(e) = self.handle_message(&line, n).await {
                                error!("Failed to handle message: {}", e);
                            }
                            line.clear();
                        }
                        Err(e) => {
                            let reason = DisconnectReason::ReadError(e.to_string());
                            let _ = self.event_tx.send(Event::Disconnected {
                                reason: reason.clone(),
                            }).await;
                            return Err(ClientError::Disconnected { reason });
                        }
                    }
                }

                // Handle commands from client
                Some(cmd) = self.command_rx.recv() => {
                    match cmd {
                        Command::SendRequest { id, method, params, response_tx } => {
                            if let Err(e) = self.handle_send_request(id, method, params, response_tx).await {
                                error!("Failed to send request: {}", e);
                                return Err(e);
                            }
                        }
                        Command::Disconnect => {
                            debug!("Disconnect requested");
                            let _ = self.event_tx.send(Event::Disconnected {
                                reason: DisconnectReason::UserRequested,
                            }).await;
                            return Ok(());
                        }
                    }
                }

                // Command channel closed (all clients dropped)
                else => {
                    debug!("All clients dropped, disconnecting");
                    return Ok(());
                }
            }
        }
    }

    async fn handle_send_request(
        &mut self,
        id: Id,
        method: String,
        params: serde_json::Value,
        response_tx: oneshot::Sender<(Message, usize, Duration)>,
    ) -> Result<()> {
        let msg = Message::Request {
            id: id.clone(),
            method,
            params,
        };

        self.pending.lock().await.insert(id, response_tx);

        let _instant = self.send(&msg).await?;

        Ok(())
    }

    async fn handle_message(&mut self, line: &str, bytes_read: usize) -> Result<()> {
        let msg: Message = match serde_json::from_str(line) {
            Ok(msg) => msg,
            Err(e) => {
                warn!("Invalid JSON message: {line:?} - {e}");
                return Ok(());
            }
        };

        match msg {
            Message::Response {
                id,
                result,
                error,
                reject_reason,
            } => {
                let tx = {
                    let mut map = self.pending.lock().await;
                    map.remove(&id)
                };

                if let Some(tx) = tx {
                    // Calculate duration here - in real impl you'd track send time
                    let duration = Duration::from_millis(0);
                    if tx
                        .send((
                            Message::Response {
                                id: id.clone(),
                                result,
                                error,
                                reject_reason,
                            },
                            bytes_read,
                            duration,
                        ))
                        .is_err()
                    {
                        debug!("Dropped response for id={id}: receiver went away");
                    }
                } else {
                    warn!("Unmatched response ID={id}: {line}");
                }
            }

            Message::Notification { method, params } => {
                if let Err(e) = self
                    .event_tx
                    .send(Event::Notification { method, params })
                    .await
                {
                    error!("Failed to forward notification: {e}");
                    return Err(ClientError::ChannelSend);
                }
            }

            _ => {
                warn!("Unexpected message type: {:?}", msg);
            }
        }

        Ok(())
    }

    async fn send(&mut self, message: &Message) -> Result<Instant> {
        let frame = serde_json::to_string(message)
            .map_err(|e| ClientError::Serialization { source: e })?
            + "\n";
        self.tcp_writer
            .write_all(frame.as_bytes())
            .await
            .map_err(|e| ClientError::Io { source: e })?;
        let instant = Instant::now();
        self.tcp_writer
            .flush()
            .await
            .map_err(|e| ClientError::Io { source: e })?;
        Ok(instant)
    }
}
