use {super::*, crate::MAX_MESSAGE_SIZE, std::time::Instant};

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

pub(super) enum ClientMessage {
    Connect {
        respond_to: oneshot::Sender<Result>,
    },
    Request {
        method: &'static str,
        params: Value,
        respond_to: oneshot::Sender<Result<(Message, usize)>>,
    },
    Disconnect {
        respond_to: oneshot::Sender<()>,
    },
}

pub(super) struct ClientActor {
    inner: Arc<Config>,
    rx: mpsc::Receiver<ClientMessage>,
    events: broadcast::Sender<Event>,
    id_counter: u64,
    pending: HashMap<Id, PendingRequest>,
    connection: Option<ConnectionState>,
}

impl ClientActor {
    pub(super) fn new(
        inner: Arc<Config>,
        rx: mpsc::Receiver<ClientMessage>,
        events: broadcast::Sender<Event>,
    ) -> Self {
        Self {
            inner,
            rx,
            events,
            id_counter: 0,
            pending: HashMap::new(),
            connection: None,
        }
    }

    pub(super) async fn run(mut self) {
        let (incoming_tx, mut incoming_rx) = mpsc::channel::<IncomingMessage>(CHANNEL_BUFFER_SIZE);

        loop {
            tokio::select! {
                biased;

                Some(msg) = incoming_rx.recv() => {
                    self.handle_incoming(msg).await;
                }
                Some(msg) = self.rx.recv() => {
                    match msg {
                        ClientMessage::Connect { respond_to } => {
                            let result = self.handle_connect(incoming_tx.clone()).await;
                            if respond_to.send(result).is_err() {
                                debug!("Connect response dropped: caller gave up");
                            }
                        }
                        ClientMessage::Request { method, params, respond_to } => {
                            self.evict_expired_pending();

                            if self.pending.len() >= MAX_PENDING_REQUESTS {
                                if respond_to.send(Err(ClientError::TooManyPendingRequests)).is_err() {
                                    debug!("TooManyPendingRequests response dropped: caller gave up");
                                }
                                continue;
                            }

                            let id = self.next_id();
                            let deadline = Instant::now() + self.inner.timeout;

                            match self.handle_request(id.clone(), method, params).await {
                                Ok(_) => {
                                    self.pending.insert(id, (respond_to, deadline));
                                }
                                Err(err) => {
                                    if respond_to.send(Err(err)).is_err() {
                                        debug!("Request error response dropped: caller gave up");
                                    }
                                }
                            }
                        }
                        ClientMessage::Disconnect { respond_to } => {
                            self.handle_disconnect().await;
                            if respond_to.send(()).is_err() {
                                debug!("Disconnect response dropped: caller gave up");
                            }
                        }
                    }
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

        let expired_ids = self
            .pending
            .iter()
            .filter(|(_, (_, deadline))| now > *deadline)
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();

        for id in expired_ids {
            if let Some((tx, _)) = self.pending.remove(&id)
                && tx.send(Err(ClientError::RequestExpired)).is_err()
            {
                debug!("RequestExpired response dropped: caller gave up");
            }
        }
    }

    async fn handle_connect(&mut self, incoming_tx: mpsc::Sender<IncomingMessage>) -> Result {
        if self.connection.is_some() {
            self.handle_disconnect().await;
        }

        let stream =
            tokio::time::timeout(self.inner.timeout, TcpStream::connect(&self.inner.address))
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

        debug!("Connected to {}", self.inner.address);
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

    async fn handle_request(&mut self, id: Id, method: &'static str, params: Value) -> Result {
        let msg = Message::Request {
            id,
            method: method.to_owned(),
            params,
        };
        self.send_message(&msg).await
    }

    async fn handle_disconnect(&mut self) {
        if let Some(connection) = self.connection.take() {
            connection.reader_handle.abort();
            debug!("Disconnected");
        }

        let pending = std::mem::take(&mut self.pending);
        for (_, (tx, _)) in pending {
            if tx.send(Err(ClientError::NotConnected)).is_err() {
                debug!("NotConnected response dropped: caller gave up");
            }
        }

        if self.events.send(Event::Disconnected).is_err() {
            debug!("Disconnected event dropped: no subscribers");
        }
    }

    async fn handle_incoming(&mut self, msg: IncomingMessage) {
        match msg {
            IncomingMessage::Response {
                id,
                message,
                bytes_read,
            } => {
                if let Some((tx, _)) = self.pending.remove(&id) {
                    if tx.send(Ok((message, bytes_read))).is_err() {
                        debug!("Response dropped: caller gave up");
                    }
                } else {
                    warn!("Unmatched response ID={:?}", id);
                }
            }
            IncomingMessage::Notification { method, params } => match method.as_str() {
                "mining.notify" => match serde_json::from_value::<Notify>(params) {
                    Ok(notify) => {
                        if self.events.send(Event::Notify(notify)).is_err() {
                            debug!("Notify event dropped: no subscribers");
                        }
                    }
                    Err(e) => warn!("Failed to parse mining.notify: {}", e),
                },
                "mining.set_difficulty" => match serde_json::from_value::<SetDifficulty>(params) {
                    Ok(set_diff) => {
                        if self
                            .events
                            .send(Event::SetDifficulty(set_diff.difficulty()))
                            .is_err()
                        {
                            debug!("SetDifficulty event dropped: no subscribers");
                        }
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
                    if incoming_tx
                        .send(IncomingMessage::Error(ClientError::Io {
                            source: std::io::Error::new(std::io::ErrorKind::InvalidData, e),
                        }))
                        .await
                        .is_err()
                    {
                        debug!("Error notification dropped: actor shutting down");
                    }
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
                    if incoming_tx
                        .send(IncomingMessage::Response {
                            id: id.clone(),
                            message: msg,
                            bytes_read,
                        })
                        .await
                        .is_err()
                    {
                        debug!("Response forwarding dropped: actor shutting down");
                        break;
                    }
                }
                Message::Notification { method, params } => {
                    if incoming_tx
                        .send(IncomingMessage::Notification {
                            method: method.clone(),
                            params: params.clone(),
                        })
                        .await
                        .is_err()
                    {
                        debug!("Notification forwarding dropped: actor shutting down");
                        break;
                    }
                }
                _ => {
                    warn!("Unexpected message type: {:?}", msg);
                }
            }
        }

        if incoming_tx
            .send(IncomingMessage::Disconnected)
            .await
            .is_err()
        {
            debug!("Disconnected notification dropped: actor already shut down");
        }
    }
}
