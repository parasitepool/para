use super::*;

pub(super) struct Connection {
    config: Arc<ClientConfig>,
    rx: mpsc::Receiver<ClientMessage>,
    events: tokio::sync::broadcast::Sender<Event>,
    pending: BTreeMap<Id, oneshot::Sender<Result<(Message, usize)>>>,
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
            pending: BTreeMap::new(),
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
        let mut reader = BufReader::new(reader);
        let mut writer = BufWriter::new(writer);

        let mut line = String::new();

        loop {
            line.clear();

            tokio::select! {
                // Handle outgoing requests from Client
                msg = self.rx.recv() => {
                    match msg {
                        Some(ClientMessage::Request { id, method, params, tx }) => {
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
                        Some(ClientMessage::Disconnect) => {
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
        let _ = self.events.send(Event::Disconnected);

        Ok(())
    }

    async fn handle_notification(&self, method: String, params: Value) {
        match method.as_str() {
            "mining.notify" => match serde_json::from_value::<Notify>(params) {
                Ok(notify) => {
                    let _ = self.events.send(Event::Notify(notify));
                }
                Err(e) => warn!("Failed to parse mining.notify: {}", e),
            },
            "mining.set_difficulty" => match serde_json::from_value::<SetDifficulty>(params) {
                Ok(set_diff) => {
                    info!("Set difficulty");
                    let _ = self
                        .events
                        .send(Event::SetDifficulty(set_diff.difficulty()));
                }
                Err(e) => warn!("Failed to parse mining.set_difficulty: {}", e),
            },
            _ => warn!("Unhandled notification: {}", method),
        }
    }
}
