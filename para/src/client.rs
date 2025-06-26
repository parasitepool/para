use super::*;

// Handles all the stratum protocol messages. Holds all the client information and updates the
// hasher with new work/templates. Has a couple channels to the Miner for communication and
// listens/talks to upstream mining pool
pub struct Client {
    user: String,
    password: String,
    pub notifications: mpsc::Receiver<Message>,
    pub requests: mpsc::Receiver<Message>,
    pending: Arc<Mutex<BTreeMap<Id, oneshot::Sender<Message>>>>,
    listener: JoinHandle<()>,
    tcp_writer: BufWriter<OwnedWriteHalf>,
    id_counter: AtomicU64,
}

impl Client {
    pub async fn connect(host: &str, port: u16, user: &str, password: &str) -> Result<Self> {
        info!("Connecting to {host}:{port} with user {user}");

        let stream = TcpStream::connect((host, port)).await?;

        let (tcp_reader, tcp_writer) = {
            let (rx, tx) = stream.into_split();
            (BufReader::new(rx), BufWriter::new(tx))
        };

        let (request_sender, request_receiver) = mpsc::channel(32);
        let (notification_sender, notification_receiver) = mpsc::channel(32);

        let pending: Arc<Mutex<BTreeMap<Id, oneshot::Sender<Message>>>> =
            Arc::new(Mutex::new(BTreeMap::new()));

        let listener = {
            let request_sender = request_sender.clone();
            let notification_sender = notification_sender.clone();
            let pending = pending.clone();
            tokio::spawn(async {
                Self::listener(tcp_reader, request_sender, notification_sender, pending).await
            })
        };

        Ok(Self {
            tcp_writer,
            requests: request_receiver,
            notifications: notification_receiver,
            listener,
            pending: pending.clone(),
            user: user.to_string(),
            password: password.to_string(),
            id_counter: AtomicU64::new(1),
        })
    }

    async fn listener<R>(
        mut tcp_reader: BufReader<R>,
        requests: mpsc::Sender<Message>,
        notifications: mpsc::Sender<Message>,
        pending: Arc<Mutex<BTreeMap<Id, oneshot::Sender<Message>>>>,
    ) where
        R: AsyncRead + Unpin,
    {
        let mut line = String::new();

        loop {
            line.clear();

            match tcp_reader.read_line(&mut line).await {
                Ok(0) => break,
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
                Message::Response { id, result, error } => {
                    let tx = {
                        let mut map = pending.lock().await;
                        map.remove(&id)
                    };

                    if let Some(tx) = tx {
                        if tx
                            .send(Message::Response {
                                id: id.clone(),
                                result,
                                error,
                            })
                            .is_err()
                        {
                            debug!("Dropped response for id={id} — receiver went away");
                        }
                    } else {
                        warn!("Unmatched response ID={id}: {line}");
                    }
                }

                Message::Notification { .. } => {
                    if let Err(e) = notifications.send(msg).await {
                        error!("Failed to forward notification: {e}");
                        break;
                    }
                }

                Message::Request { .. } => {
                    if let Err(e) = requests.send(msg).await {
                        error!("Failed to forward request: {e}");
                        break;
                    }
                }
            }
        }
    }

    pub async fn subscribe(&mut self) -> Result<SubscribeResult> {
        let rx = self
            .send_request(
                "mining.subscribe",
                serde_json::to_value(Subscribe {
                    user_agent: "user ParaMiner/0.0.1".into(),
                    extranonce1: None,
                })?,
            )
            .await?;

        match rx.await? {
            Message::Response {
                result: Some(result),
                error: None,
                ..
            } => Ok(serde_json::from_value(result)?),
            Message::Response {
                error: Some(err), ..
            } => Err(anyhow!("mining.subscribe error: {}", err)),
            _ => Err(anyhow!("Unknown mining.subscribe error")),
        }
    }

    pub async fn authorize(&mut self) -> Result {
        let rx = self
            .send_request(
                "mining.authorize",
                serde_json::to_value(Authorize {
                    worker_name: self.user.clone(),
                    password: self.password.clone(),
                })?,
            )
            .await?;

        match rx.await? {
            Message::Response {
                result: Some(result),
                error: None,
                ..
            } => {
                if serde_json::from_value(result)? {
                    Ok(())
                } else {
                    Err(anyhow!("Unauthorized"))
                }
            }
            Message::Response {
                error: Some(err), ..
            } => Err(anyhow!("mining.authorize error: {}", err)),
            _ => Err(anyhow!("Unknown mining.authorize error")),
        }
    }

    pub async fn submit(
        &mut self,
        job_id: String,
        extranonce2: String,
        ntime: String,
        nonce: u32,
    ) -> Result {
        let rx = self
            .send_request(
                "mining.submit",
                serde_json::to_value(Submit {
                    worker_name: self.user.clone(),
                    job_id,
                    extranonce2,
                    ntime,
                    nonce,
                })?,
            )
            .await?;

        match rx.await? {
            Message::Response {
                result: Some(result),
                error: None,
                ..
            } => {
                if serde_json::from_value(result)? {
                    Ok(())
                } else {
                    Err(anyhow!("Failed to submit"))
                }
            }
            Message::Response {
                error: Some(err), ..
            } => Err(anyhow!("mining.submit error: {}", err)),
            _ => Err(anyhow!("Unknown mining.submit error")),
        }
    }

    async fn send_request(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<oneshot::Receiver<Message>> {
        let id = self.next_id();

        let msg = Message::Request {
            id: id.clone(),
            method: method.to_string(),
            params,
        };

        let (tx, rx) = oneshot::channel();

        self.pending.lock().await.insert(id.clone(), tx);

        self.send(&msg).await?;

        Ok(rx)
    }

    async fn send(&mut self, message: &Message) -> Result {
        let frame = serde_json::to_string(message)? + "\n";
        self.tcp_writer.write_all(frame.as_bytes()).await?;
        self.tcp_writer.flush().await?;
        Ok(())
    }

    fn next_id(&mut self) -> Id {
        Id::Number(self.id_counter.fetch_add(1, Ordering::Relaxed))
    }

    pub fn shutdown(&self) {
        self.listener.abort()
    }
}
