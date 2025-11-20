use {super::*, error::ClientError};

mod error;

pub type Result<T = (), E = ClientError> = std::result::Result<T, E>;

type Pending = Arc<Mutex<BTreeMap<Id, oneshot::Sender<(Message, usize)>>>>;

pub struct Client {
    pub incoming: mpsc::Receiver<Message>,
    id_counter: AtomicU64,
    listener: JoinHandle<()>,
    password: String,
    pending: Pending,
    tcp_writer: BufWriter<OwnedWriteHalf>,
    username: String,
}

impl Client {
    pub async fn connect(
        address: impl tokio::net::ToSocketAddrs,
        username: String,
        password: Option<String>,
        timeout: Duration,
    ) -> Result<Self> {
        let stream = tokio::time::timeout(timeout, TcpStream::connect(address))
            .await
            .context(error::TimeoutSnafu)?
            .context(error::IoSnafu)?;

        let (tcp_reader, tcp_writer) = {
            let (rx, tx) = stream.into_split();
            (BufReader::new(rx), BufWriter::new(tx))
        };

        let (incoming_tx, incoming_rx) = mpsc::channel(32);

        let pending: Pending = Arc::new(Mutex::new(BTreeMap::new()));

        let listener = {
            let incoming_tx = incoming_tx.clone();
            let pending = pending.clone();
            tokio::spawn(async move { Self::listener(tcp_reader, incoming_tx, pending).await })
        };

        Ok(Self {
            tcp_writer,
            incoming: incoming_rx,
            listener,
            pending: pending.clone(),
            username,
            password: password.unwrap_or("x".to_string()),
            id_counter: AtomicU64::new(0),
        })
    }

    pub async fn disconnect(&mut self) -> Result {
        self.tcp_writer.shutdown().await.context(error::IoSnafu)?;
        self.listener.abort();
        Ok(())
    }

    async fn listener<R>(
        mut tcp_reader: BufReader<R>,
        incoming: mpsc::Sender<Message>,
        pending: Pending,
    ) where
        R: AsyncRead + Unpin,
    {
        let mut line = String::new();

        loop {
            line.clear();

            let bytes_read = match tcp_reader.read_line(&mut line).await {
                Ok(0) => {
                    error!("Stratum server disconnected");
                    break;
                }
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
                Message::Response {
                    id,
                    result,
                    error,
                    reject_reason,
                } => {
                    let tx = {
                        let mut map = pending.lock().await;
                        map.remove(&id)
                    };

                    if let Some(tx) = tx {
                        if tx
                            .send((
                                Message::Response {
                                    id: id.clone(),
                                    result,
                                    error,
                                    reject_reason,
                                },
                                bytes_read,
                            ))
                            .is_err()
                        {
                            debug!("Dropped response for id={id}: receiver went away");
                        }
                    } else {
                        warn!("Unmatched response ID={id}: {line}");
                    }
                }

                _ => {
                    if let Err(e) = incoming.send(msg).await {
                        error!("Failed to forward incoming notification/request: {e}");
                        break;
                    }
                }
            }
        }

        drop(incoming);
    }

    pub async fn configure(
        &mut self,
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

        let (message, bytes_read) = rx.await.context(error::ChannelRecvSnafu)?;

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
        &mut self,
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

        let (message, bytes_read) = rx.await.context(error::ChannelRecvSnafu)?;

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

    pub async fn authorize(&mut self) -> Result<(Duration, usize)> {
        let (rx, instant) = self
            .send_request(
                "mining.authorize",
                serde_json::to_value(Authorize {
                    username: self.username.clone(),
                    password: Some(self.password.clone()),
                })
                .context(error::SerializationSnafu)?,
            )
            .await?;

        let (message, bytes_read) = rx.await.context(error::ChannelRecvSnafu)?;

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

        let (rx, _) = self
            .send_request(
                "mining.submit",
                serde_json::to_value(&submit).context(error::SerializationSnafu)?,
            )
            .await?;

        let (message, _) = rx.await.context(error::ChannelRecvSnafu)?;

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

    async fn send_request(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<(oneshot::Receiver<(Message, usize)>, Instant)> {
        let id = self.next_id();

        let msg = Message::Request {
            id: id.clone(),
            method: method.to_string(),
            params,
        };

        let (tx, rx) = oneshot::channel();

        self.pending.lock().await.insert(id.clone(), tx);

        let instant = self.send(&msg).await?;

        Ok((rx, instant))
    }

    async fn send(&mut self, message: &Message) -> Result<Instant> {
        let frame = serde_json::to_string(message).context(error::SerializationSnafu)? + "\n";
        self.tcp_writer
            .write_all(frame.as_bytes())
            .await
            .context(error::IoSnafu)?;
        let instant = Instant::now();
        self.tcp_writer.flush().await.context(error::IoSnafu)?;
        Ok(instant)
    }

    fn next_id(&mut self) -> Id {
        Id::Number(self.id_counter.fetch_add(1, Ordering::Relaxed))
    }
}
