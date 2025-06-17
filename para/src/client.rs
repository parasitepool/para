use super::*;

// Handles all the stratum protocol messages. Holds all the client information and updates the
// hasher with new work/templates. Has a couple channels to the Miner for communication and
// listens/talks to upstream mining pool
pub struct Client {
    pub user: String,
    pub password: String,
    pub notifications: mpsc::Receiver<Message>,
    pub requests: mpsc::Receiver<Message>,
    pending: Arc<Mutex<BTreeMap<u64, oneshot::Sender<Message>>>>,
    listener: JoinHandle<()>,
    tcp_writer: BufWriter<OwnedWriteHalf>,
    id_counter: u64,
}

impl Client {
    pub async fn connect(host: &str, port: u16, user: &str, password: &str) -> Result<Self> {
        log::info!("Connecting to {host}:{port} with user {user}");

        let stream = TcpStream::connect((host, port)).await?;

        let (tcp_reader, tcp_writer) = {
            let (rx, tx) = stream.into_split();
            (BufReader::new(rx), BufWriter::new(tx))
        };

        let (request_sender, request_receiver) = mpsc::channel(32);
        let (notification_sender, notification_receiver) = mpsc::channel(32);

        let pending: Arc<Mutex<BTreeMap<u64, oneshot::Sender<Message>>>> =
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
            id_counter: 1,
        })
    }

    async fn listener<R>(
        mut tcp_reader: BufReader<R>,
        requests: mpsc::Sender<Message>,
        notifications: mpsc::Sender<Message>,
        pending: Arc<Mutex<BTreeMap<u64, oneshot::Sender<Message>>>>,
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
                    log::error!("Read error: {e}");
                    break;
                }
            };

            let msg: Message = match serde_json::from_str(&line) {
                Ok(msg) => msg,
                Err(e) => {
                    log::warn!("Invalid JSON message: {line:?} - {e}");
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
                        if tx.send(Message::Response { id, result, error }).is_err() {
                            log::debug!("Dropped response for id={id} — receiver went away");
                        }
                    } else {
                        log::warn!("Unmatched response ID={id}: {line}");
                    }
                }

                Message::Notification { .. } => {
                    if let Err(e) = notifications.send(msg).await {
                        log::error!("Failed to forward notification: {e}");
                        break;
                    }
                }

                Message::Request { .. } => {
                    if let Err(e) = requests.send(msg).await {
                        log::error!("Failed to forward request: {e}");
                        break;
                    }
                }
            }
        }
    }

    pub async fn send(&mut self, message: &Message) -> Result<()> {
        let frame = serde_json::to_string(message)? + "\n";
        self.tcp_writer.write_all(frame.as_bytes()).await?;
        self.tcp_writer.flush().await?;
        Ok(())
    }

    fn next_id(&mut self) -> u64 {
        let id = self.id_counter;
        self.id_counter += 1;
        id
    }

    pub async fn send_request(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<oneshot::Receiver<Message>> {
        let id = self.next_id();

        let msg = Message::Request {
            id,
            method: method.to_string(),
            params,
        };

        let (tx, rx) = oneshot::channel();

        self.pending.lock().await.insert(id, tx);

        self.send(&msg).await?;

        Ok(rx)
    }

    pub async fn subscribe(&mut self) -> Result<SubscribeResult> {
        let rx = self.send_request("mining.subscribe", json!([])).await?;

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
            .send_request("mining.authorize", json!([self.user, self.password]))
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

    pub fn shutdown(&self) {
        self.listener.abort()
    }
}

//struct Client {
//    client_id: u32,
//    extranonce1: Option<Extranonce<'static>>,
//    extranonce2_size: Option<usize>,
//    version_rolling_mask: Option<HexU32Be>,
//    version_rolling_min_bit: Option<HexU32Be>,
//    miner: Miner,
//}

// Comese from the mining.notify message
//struct Job {
//    job_id: u32,
//    prev_hash: [u8; 32],
//    coinbase_1: Vec<u32>,
//    coinbase_2: Vec<u32>,
//    merkle_brances: Vec<[u8; 32]>,
//    merkle_root: [u8; 32],
//    version: u32,
//    nbits: u32,
//    _ntime: u32,       // not needed?
//    _clean_jobs: bool, // not needed
//}
//
