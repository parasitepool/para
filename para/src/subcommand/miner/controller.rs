use super::*;

pub(crate) struct Controller {
    client: Client,
    job: Arc<Mutex<Option<Notify>>>,
    difficulty: Arc<Mutex<Difficulty>>,
    extranonce1: String,
    extranonce2_size: u32,
    share_rx: mpsc::Receiver<(Header, String)>,
    share_tx: mpsc::Sender<(Header, String)>,
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
            extranonce2_size: subscribe.extranonce2_size,
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
                Some((header, extranonce2)) = self.share_rx.recv() => {
                    info!("Valid header found: {:?}", header);
                    let notify = self.job.lock().await.clone().unwrap();
                    if let Err(e) = self.client.submit(notify.job_id, extranonce2, header.time.into(), header.nonce.into()).await {
                        warn!("Failed to submit share: {e}");
                    }
                }
                _ = ctrl_c() => {
                    info!("Shutting down client and hasher");
                    self.client.shutdown();
                    self.cancel_hasher();
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

                    let extranonce2 = {
                        let mut bytes = vec![0u8; self.extranonce2_size.try_into().unwrap()];
                        rand::rng().fill(&mut bytes[..]);
                        hex::encode(bytes)
                    };

                    let share_tx = self.share_tx.clone();

                    self.cancel_hasher();
                    let cancel = self.hasher_cancel.clone();

                    let pool_diff = self.difficulty.lock().await;
                    let pool_target = pool_diff.to_target();
                    let network_nbits: CompactTarget = notify.nbits.into();
                    let network_target: Target = network_nbits.into();

                    // info!("Pool diff: {}", pool_diff);
                    // info!("Pool target: {}", pool_target);
                    info!("Network target: {}", target_as_block_hash(network_target));

                    let mut hasher = Hasher {
                        header: Header {
                            version: notify.version.clone().into(),
                            prev_blockhash: notify.prevhash.clone().into(),
                            merkle_root: self.build_merkle_root(&notify, &extranonce2)?,
                            time: notify.ntime.into(),
                            bits: notify.nbits.into(),
                            nonce: 0,
                        },
                        pool_target,
                        extranonce2: extranonce2.to_string(),
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

    fn build_merkle_root(&self, notify: &Notify, extranonce2: &str) -> Result<TxMerkleNode> {
        let coinbase_hex = format!(
            "{}{}{}{}",
            notify.coinb1, self.extranonce1, extranonce2, notify.coinb2
        );

        let coinbase_bin = hex::decode(&coinbase_hex)?;

        let mut cursor = bitcoin::io::Cursor::new(&coinbase_bin);
        let coinbase_tx = bitcoin::Transaction::consensus_decode_from_finite_reader(&mut cursor)?;

        info!(
            "Building merkle root with coinbase txid {:?}",
            coinbase_tx.compute_txid()
        );

        let coinbase_hash = sha256d::Hash::hash(&coinbase_bin);

        let mut merkle_root = coinbase_hash;

        for branch in &notify.merkle_branch {
            let mut concat = Vec::with_capacity(64);
            concat.extend_from_slice(&merkle_root[..]);
            concat.extend_from_slice(branch.as_byte_array());
            merkle_root = sha256d::Hash::hash(&concat);
        }

        Ok(TxMerkleNode::from_raw_hash(merkle_root))
    }

    fn cancel_hasher(&mut self) {
        self.hasher_cancel.cancel();
        self.hasher_cancel = CancellationToken::new();
    }
}
