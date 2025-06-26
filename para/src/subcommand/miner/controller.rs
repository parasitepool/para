use super::*;

pub(crate) struct Controller {
    client: Client,
    job: Arc<Mutex<Option<Notify>>>,
    difficulty: Arc<Mutex<Difficulty>>,
    extranonce1: String,
    share_rx: mpsc::Receiver<Header>,
    share_tx: mpsc::Sender<Header>,
    hasher_cancel: CancellationToken,
}

impl Controller {
    pub(crate) async fn new(mut client: Client) -> Result<Self> {
        let subscribe = client.subscribe().await?;
        client.authorize().await?;

        info!(
            "Authorized: extranonce1={}, extranonce2_size={}",
            subscribe.extranonce1, subscribe.extranonce2_size
        );

        let (share_tx, share_rx) = mpsc::channel(8);

        Ok(Self {
            client,
            job: Arc::new(Mutex::new(None)),
            difficulty: Arc::new(Mutex::new(Difficulty::default())),
            extranonce1: subscribe.extranonce1,
            share_rx,
            share_tx,
            hasher_cancel: CancellationToken::new(),
        })
    }

    pub(crate) async fn run(mut self) -> Result {
        loop {
            tokio::select! {
                Some(msg) = self.client.notifications.recv() => self.handle_notification(msg).await?,
                Some(msg) = self.client.requests.recv() => self.handle_request(msg).await?,
                Some(header) = self.share_rx.recv() => {
                    info!("Valid header found: {:?}", header);
                    let notify = self.job.lock().await.clone().unwrap();
                    if let Err(e) = self.client.submit(notify.job_id, "".into(), format!("{:08x}", header.time), header.nonce).await {
                        warn!("Failed to submit share: {e}");
                    }
                }
                _ = ctrl_c() => {
                    info!("Shutting down client and hasher");
                    self.client.shutdown();
                    self.cancel_hasher().await;
                    break;
                }
            }
        }

        Ok(())
    }

    async fn handle_notification(&mut self, message: Message) -> Result {
        if let Message::Notification { method, params } = message {
            match method.as_str() {
                "mining.notify" => {
                    let notify = serde_json::from_value::<Notify>(params)?;

                    self.job.lock().await.replace(notify.clone());

                    // let job_id = notify.job_id.clone();
                    // let extranonce2 = self.next_extranonce2();

                    let share_tx = self.share_tx.clone();

                    self.cancel_hasher().await;
                    let cancel = self.hasher_cancel.clone();

                    let mut hasher = Hasher {
                        header: Header {
                            version: Version::TWO,
                            prev_blockhash: notify.prevhash,
                            merkle_root: TxMerkleNode::from_raw_hash(
                                BlockHash::all_zeros().to_raw_hash(),
                            ),
                            time: u32::from_str_radix(&notify.ntime, 16).unwrap_or_default(),
                            bits: CompactTarget::from_unprefixed_hex(&notify.nbits)
                                .unwrap_or_default(),
                            nonce: 0,
                        },
                        pool_target: self.difficulty.lock().await.to_target(),
                    };

                    // CPU-heavy task so spawning in it's own thread pool
                    tokio::spawn(async move {
                        let result = tokio::task::spawn_blocking(move || hasher.hash(cancel)).await;

                        if let Ok(Ok(header)) = result {
                            if let Err(e) = share_tx.send(header).await {
                                error!("Failed to send found share: {e:#}");
                            }
                        } else if let Err(e) = result {
                            error!("Join error on hasher: {e:#}");
                        }
                    });
                }
                "mining.set_difficulty" => {
                    let difficulty = serde_json::from_value::<SetDifficulty>(params)?.difficulty();
                    *self.difficulty.lock().await = difficulty;
                    info!("New difficulty: {difficulty}");
                }

                _ => warn!("Unhandled notification: {}", method),
            }
        }

        Ok(())
    }

    async fn handle_request(&self, message: Message) -> Result {
        if let Message::Request { method, params, id } = message {
            info!("Got request method={method} with id={id} with params={params}");
        }

        Ok(())
    }

    async fn cancel_hasher(&mut self) {
        self.hasher_cancel.cancel();
        self.hasher_cancel = CancellationToken::new();
    }
}
