use {super::*, crate::MAX_MESSAGE_SIZE, parking_lot::RwLock, std::time::Instant};

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

const MAX_PENDING_REQUESTS: usize = 1024;

type PendingRequest = (oneshot::Sender<Result<(Message, usize)>>, Instant);
type PendingSubmit = (oneshot::Sender<Result<bool>>, Instant);

pub(super) enum ClientMessage {
    Connect {
        respond_to: oneshot::Sender<Result>,
    },
    Request {
        method: String,
        params: Value,
        respond_to: oneshot::Sender<Result<(Message, usize)>>,
    },
    SubmitAsync {
        submit: Submit,
        respond_to: oneshot::Sender<Result<bool>>,
    },
    Disconnect {
        respond_to: oneshot::Sender<()>,
    },
}

pub(super) struct ClientActor {
    config: Arc<ClientConfig>,
    rx: mpsc::Receiver<ClientMessage>,
    events: broadcast::Sender<Event>,
    state: Arc<RwLock<ClientState>>,
    id_counter: u64,
    pending: HashMap<Id, PendingRequest>,
    pending_submits: HashMap<Id, PendingSubmit>,
    connection: Option<ConnectionState>,
}

impl ClientActor {
    pub(super) fn new(
        config: Arc<ClientConfig>,
        rx: mpsc::Receiver<ClientMessage>,
        events: broadcast::Sender<Event>,
        state: Arc<RwLock<ClientState>>,
    ) -> Self {
        Self {
            config,
            rx,
            events,
            state,
            id_counter: 0,
            pending: HashMap::new(),
            pending_submits: HashMap::new(),
            connection: None,
        }
    }

    pub(super) async fn run(mut self) {
        let (incoming_tx, mut incoming_rx) = mpsc::channel::<IncomingMessage>(CHANNEL_BUFFER_SIZE);

        loop {
            tokio::select! {
                Some(msg) = self.rx.recv() => {
                    match msg {
                        ClientMessage::Connect { respond_to } => {
                            let result = self.handle_connect(incoming_tx.clone()).await;
                            respond_to.send(result).ok();
                        }
                        ClientMessage::Request { method, params, respond_to } => {
                            self.evict_expired_pending();

                            if self.pending.len() >= MAX_PENDING_REQUESTS {
                                respond_to.send(Err(ClientError::TooManyPendingRequests)).ok();
                                continue;
                            }

                            let id = self.next_id();
                            let deadline = Instant::now() + self.config.timeout;

                            match self.handle_request(id.clone(), method, params).await {
                                Ok(_) => {
                                    self.pending.insert(id, (respond_to, deadline));
                                }
                                Err(err) => {
                                    respond_to.send(Err(err)).ok();
                                }
                            }
                        }
                        ClientMessage::SubmitAsync { submit, respond_to } => {
                            self.evict_expired_pending();

                            if self.pending_submits.len() >= MAX_PENDING_REQUESTS {
                                respond_to.send(Err(ClientError::TooManyPendingRequests)).ok();
                                continue;
                            }

                            let id = self.next_id();
                            let deadline = Instant::now() + self.config.timeout;

                            match self.handle_submit_request(id.clone(), &submit).await {
                                Ok(_) => {
                                    self.pending_submits.insert(id, (respond_to, deadline));
                                }
                                Err(err) => {
                                    respond_to.send(Err(err)).ok();
                                }
                            }
                        }
                        ClientMessage::Disconnect { respond_to } => {
                            self.handle_disconnect().await;
                            respond_to.send(()).ok();
                        }
                    }
                }
                Some(msg) = incoming_rx.recv() => {
                    self.handle_incoming(msg).await;
                }
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

    fn evict_expired_pending(&mut self) {
        let now = Instant::now();

        let expired_ids: Vec<_> = self
            .pending
            .iter()
            .filter(|(_, (_, deadline))| now > *deadline)
            .map(|(id, _)| id.clone())
            .collect();

        for id in expired_ids {
            if let Some((tx, _)) = self.pending.remove(&id) {
                tx.send(Err(ClientError::RequestExpired)).ok();
            }
        }

        let expired_submit_ids: Vec<_> = self
            .pending_submits
            .iter()
            .filter(|(_, (_, deadline))| now > *deadline)
            .map(|(id, _)| id.clone())
            .collect();

        for id in expired_submit_ids {
            if let Some((tx, _)) = self.pending_submits.remove(&id) {
                tx.send(Err(ClientError::RequestExpired)).ok();
            }
        }
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

        stream
            .set_nodelay(true)
            .map_err(|source| ClientError::Io { source })?;

        let (reader, writer) = stream.into_split();
        let writer = BufWriter::new(writer);
        let framed_reader =
            FramedRead::new(reader, LinesCodec::new_with_max_length(MAX_MESSAGE_SIZE));

        let reader_handle = tokio::spawn(async move {
            Self::reader_task(framed_reader, incoming_tx).await;
        });

        self.connection = Some(ConnectionState {
            writer,
            reader_handle,
        });

        debug!("Connected to {}", self.config.address);
        Ok(())
    }

    async fn send_message(&mut self, msg: &Message) -> Result {
        let connection = self.connection.as_mut().ok_or(ClientError::NotConnected)?;

        let frame = serde_json::to_string(msg)
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

    async fn handle_request(&mut self, id: Id, method: String, params: Value) -> Result {
        let msg = Message::Request { id, method, params };
        self.send_message(&msg).await
    }

    async fn handle_submit_request(&mut self, id: Id, submit: &Submit) -> Result {
        let msg = Message::Request {
            id,
            method: "mining.submit".to_owned(),
            params: serde_json::to_value(submit)
                .map_err(|e| ClientError::Serialization { source: e })?,
        };
        self.send_message(&msg).await
    }

    async fn handle_disconnect(&mut self) {
        if let Some(connection) = self.connection.take() {
            connection.reader_handle.abort();
            debug!("Disconnected");
        }

        self.state.write().clear();

        let pending = std::mem::take(&mut self.pending);
        for (_, (tx, _)) in pending {
            tx.send(Err(ClientError::NotConnected)).ok();
        }

        let pending_submits = std::mem::take(&mut self.pending_submits);
        for (_, (tx, _)) in pending_submits {
            tx.send(Err(ClientError::NotConnected)).ok();
        }

        self.events.send(Event::Disconnected).ok();
    }

    async fn handle_incoming(&mut self, msg: IncomingMessage) {
        match msg {
            IncomingMessage::Response {
                id,
                message,
                bytes_read,
            } => {
                if let Some((tx, _)) = self.pending_submits.remove(&id) {
                    let result = match &message {
                        Message::Response {
                            result: Some(val),
                            error: None,
                            ..
                        } => serde_json::from_value::<bool>(val.clone()).unwrap_or(false),
                        _ => false,
                    };
                    tx.send(Ok(result)).ok();
                } else if let Some((tx, _)) = self.pending.remove(&id) {
                    tx.send(Ok((message, bytes_read))).ok();
                } else {
                    warn!("Unmatched response ID={:?}", id);
                }
            }
            IncomingMessage::Notification { method, params } => match method.as_str() {
                "mining.notify" => match serde_json::from_value::<Notify>(params) {
                    Ok(notify) => {
                        self.events.send(Event::Notify(notify)).ok();
                    }
                    Err(e) => warn!("Failed to parse mining.notify: {}", e),
                },
                "mining.set_difficulty" => match serde_json::from_value::<SetDifficulty>(params) {
                    Ok(set_diff) => {
                        let difficulty = set_diff.difficulty();
                        self.state.write().difficulty = Some(difficulty);
                        self.events.send(Event::SetDifficulty(difficulty)).ok();
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
        mut reader: FramedRead<tokio::net::tcp::OwnedReadHalf, LinesCodec>,
        incoming_tx: mpsc::Sender<IncomingMessage>,
    ) {
        while let Some(result) = reader.next().await {
            let line = match result {
                Ok(line) => line,
                Err(e) => {
                    error!("Read error: {e}");
                    incoming_tx
                        .send(IncomingMessage::Error(ClientError::Io {
                            source: std::io::Error::new(std::io::ErrorKind::InvalidData, e),
                        }))
                        .await
                        .ok();
                    break;
                }
            };

            let bytes_read = line.len() + 1;

            let msg: Message = match serde_json::from_str(&line) {
                Ok(msg) => msg,
                Err(e) => {
                    warn!("Invalid JSON message: {line:?} - {e}");
                    continue;
                }
            };

            match &msg {
                Message::Response { id, .. } => {
                    incoming_tx
                        .send(IncomingMessage::Response {
                            id: id.clone(),
                            message: msg,
                            bytes_read,
                        })
                        .await
                        .ok();
                }
                Message::Notification { method, params } => {
                    incoming_tx
                        .send(IncomingMessage::Notification {
                            method: method.clone(),
                            params: params.clone(),
                        })
                        .await
                        .ok();
                }
                _ => {
                    warn!("Unexpected message type: {:?}", msg);
                }
            }
        }

        incoming_tx.send(IncomingMessage::Disconnected).await.ok();
    }
}
