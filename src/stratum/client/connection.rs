use super::*;

type PendingRequests =
    Arc<tokio::sync::Mutex<BTreeMap<Id, oneshot::Sender<Result<(Message, usize)>>>>>;

pub(super) struct Connection {
    config: Arc<ClientConfig>,
    rx: mpsc::Receiver<ClientMessage>,
    events: tokio::sync::broadcast::Sender<Event>,
    pending: PendingRequests,
}

impl Connection {
    pub(super) fn new(
        config: Arc<ClientConfig>,
        rx: mpsc::Receiver<ClientMessage>,
        events: tokio::sync::broadcast::Sender<Event>,
    ) -> Self {
        Self {
            config,
            rx,
            events,
            pending: Arc::new(tokio::sync::Mutex::new(BTreeMap::new())),
        }
    }

    pub(super) async fn run(mut self) -> Result<()> {
        let stream = tokio::time::timeout(
            self.config.timeout,
            TcpStream::connect(&self.config.address),
        )
        .await
        .context(error::TimeoutSnafu)?
        .context(error::IoSnafu)?;

        let (reader, writer) = stream.into_split();
        let reader = BufReader::new(reader);
        let mut writer = BufWriter::new(writer);

        // Spawn dedicated reader task that never gets cancelled
        let pending_clone = self.pending.clone();
        let events_clone = self.events.clone();
        let reader_handle =
            tokio::spawn(async move { Self::read_loop(reader, pending_clone, events_clone).await });

        // Main loop handles outgoing requests
        loop {
            match self.rx.recv().await {
                Some(ClientMessage::Request {
                    id,
                    method,
                    params,
                    tx,
                }) => {
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
                        break;
                    }
                    if let Err(e) = writer.flush().await {
                        let _ = tx.send(Err(ClientError::Io { source: e }));
                        break;
                    }

                    self.pending.lock().await.insert(id, tx);
                }
                Some(ClientMessage::Disconnect) => {
                    break;
                }
                None => {
                    break;
                }
            }
        }

        // Abort the reader task when we're done
        reader_handle.abort();

        // Cleanup: notify pending requests
        let pending = std::mem::take(&mut *self.pending.lock().await);
        for (_, tx) in pending {
            let _ = tx.send(Err(ClientError::NotConnected));
        }

        // Notify disconnection
        let _ = self.events.send(Event::Disconnected);

        Ok(())
    }

    async fn read_loop(
        mut reader: BufReader<tokio::net::tcp::OwnedReadHalf>,
        pending: PendingRequests,
        events: tokio::sync::broadcast::Sender<Event>,
    ) {
        let mut line = String::new();

        loop {
            line.clear();

            let bytes_read = match reader.read_line(&mut line).await {
                Ok(0) => {
                    // EOF - notify disconnection
                    let _ = events.send(Event::Disconnected);
                    break;
                }
                Ok(n) => n,
                Err(e) => {
                    error!("Read error: {e}");
                    let _ = events.send(Event::Disconnected);
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
                    if let Some(tx) = pending.lock().await.remove(&id) {
                        let _ = tx.send(Ok((
                            Message::Response {
                                id,
                                result,
                                error,
                                reject_reason,
                            },
                            bytes_read,
                        )));
                    } else {
                        warn!("Unmatched response ID={id}: {line}");
                    }
                }
                Message::Notification { method, params } => {
                    Self::handle_notification(&events, method, params);
                }
                _ => {
                    warn!("Unexpected message type: {:?}", msg);
                }
            }
        }
    }

    fn handle_notification(
        events: &tokio::sync::broadcast::Sender<Event>,
        method: String,
        params: Value,
    ) {
        match method.as_str() {
            "mining.notify" => match serde_json::from_value::<Notify>(params) {
                Ok(notify) => {
                    let _ = events.send(Event::Notify(notify));
                }
                Err(e) => warn!("Failed to parse mining.notify: {}", e),
            },
            "mining.set_difficulty" => match serde_json::from_value::<SetDifficulty>(params) {
                Ok(set_diff) => {
                    let _ = events.send(Event::SetDifficulty(set_diff.difficulty()));
                }
                Err(e) => warn!("Failed to parse mining.set_difficulty: {}", e),
            },
            _ => warn!("Unhandled notification: {}", method),
        }
    }
}
