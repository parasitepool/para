use super::*;

// Handles all the stratum protocol messages. Holds all the client information and updates the
// hasher with new work/templates. Has a couple channels to the Miner for communication and
// listens/talks to upstream mining pool
pub struct Client {
    pub user: String,
    pub password: String,
    pub message_receiver: mpsc::Receiver<Message>,
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

        let (message_sender, message_receiver) = mpsc::channel(32);

        let listener = {
            let sender = message_sender.clone();
            tokio::spawn(async { Self::listener(tcp_reader, sender).await })
        };

        Ok(Self {
            tcp_writer,
            message_receiver,
            listener,
            user: user.to_string(),
            password: password.to_string(),
            id_counter: 1,
        })
    }

    async fn listener<R>(mut tcp_reader: BufReader<R>, message_sender: mpsc::Sender<Message>)
    where
        R: AsyncRead + Unpin,
    {
        let mut line = String::new();
        loop {
            line.clear();
            match tcp_reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => match serde_json::from_str::<Message>(&line) {
                    Ok(msg) => {
                        if message_sender.send(msg).await.is_err() {
                            break;
                        }
                    }
                    Err(err) => {
                        log::info!("Invalid message: {line} - {err}");
                    }
                },
                Err(e) => {
                    log::error!("Read error: {e}");
                    break;
                }
            }
        }
    }

    pub async fn send(&mut self, message: &Message) -> Result<()> {
        let json = serde_json::to_string(message)? + "\n";
        self.tcp_writer.write_all(json.as_bytes()).await?;
        self.tcp_writer.flush().await?;
        Ok(())
    }

    fn next_id(&mut self) -> u64 {
        let id = self.id_counter;
        self.id_counter += 1;
        id
    }

    pub async fn send_request(&mut self, method: &str, params: serde_json::Value) -> Result {
        let id = self.next_id();
        let msg = Message::Request {
            id: json!(id),
            method: method.to_string(),
            params,
        };
        // let (_tx, rx) = oneshot::channel();
        // self.requests.lock().await.insert(id, tx);
        self.send(&msg).await?;
        Ok(())
    }

    pub async fn subscribe(&mut self) -> Result<()> {
        self.send_request("mining.subscribe", json!([])).await?;

        //    if let Message::Response {
        //        result: Some(val), ..
        //    } = rx.await?
        //    {
        //        let result: SubscribeResult = serde_json::from_value(val)?;
        //        log::info!(
        //            "Subscribed: extranonce1={}, extranonce2_size={}",
        //            result.1,
        //            result.2
        //        );
        //    }
        Ok(())
    }

    pub async fn authorize(&mut self) -> Result<()> {
        self.send_request("mining.authorize", json!([self.user, self.password]))
            .await?;

        //    if let Message::Response {
        //        result: Some(val), ..
        //    } = rx.await?
        //    {
        //        if val == json!(true) {
        //            log::info!("Authorized");
        //        }
        //    }
        Ok(())
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
