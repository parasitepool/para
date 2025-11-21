use super::*;

struct ConnectionState {
    writer: BufWriter<tokio::net::tcp::OwnedWriteHalf>,
    reader_handle: tokio::task::JoinHandle<()>,
}

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

pub(super) enum ClientMessage {
    Connect {
        respond_to: oneshot::Sender<Result>,
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

pub(super) struct ClientActor {
    config: ClientConfig,
    rx: mpsc::Receiver<ClientMessage>,
    events: tokio::sync::broadcast::Sender<Event>,
    id_counter: u64,
    pending: BTreeMap<Id, oneshot::Sender<Result<(Message, usize)>>>,
    connection: Option<ConnectionState>,
}

impl ClientActor {
    pub(super) fn new(
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

    pub(super) async fn run(mut self) {
        let (incoming_tx, mut incoming_rx) = mpsc::channel::<IncomingMessage>(32);

        loop {
            tokio::select! {
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

    async fn handle_connect(&mut self, incoming_tx: mpsc::Sender<IncomingMessage>) -> Result {
        if self.connection.is_some() {
            self.handle_disconnect().await;
        }

        let stream = tokio::time::timeout(
            self.config.timeout,
            TcpStream::connect(&self.config.address),
        )
        .await
        .map_err(|source| ClientError::Timeout { source })?
        .map_err(|source| ClientError::Io { source })?;

        let (reader, writer) = stream.into_split();
        let writer = BufWriter::new(writer);

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

    async fn handle_request(&mut self, id: Id, method: String, params: Value) -> Result {
        let connection = self.connection.as_mut().ok_or(ClientError::NotConnected)?;

        // Request is already complete - just forward it!
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
