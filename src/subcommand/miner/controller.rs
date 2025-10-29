use super::*;

pub(crate) struct Controller {
    client: Client,
    pool_difficulty: Arc<Mutex<Difficulty>>,
    extranonce1: Extranonce,
    share_rx: mpsc::Receiver<(JobId, Header, Extranonce, ckpool::HashRate)>,
    share_tx: mpsc::Sender<(JobId, Header, Extranonce, ckpool::HashRate)>,
    cancel: CancellationToken,
    cpu_cores: usize,
    next_extranonce2: Extranonce,
    once: bool,
    username: String,
    shares: Vec<Share>,
}

impl Controller {
    pub(crate) async fn new(
        mut client: Client,
        cpu_cores: usize,
        once: bool,
        username: String,
    ) -> Result<Self> {
        let (subscribe, _, _) = client.subscribe().await?;
        client.authorize().await?;

        info!(
            "Authorized: extranonce1={}, extranonce2_size={}",
            subscribe.extranonce1, subscribe.extranonce2_size
        );

        info!("Controller initialized with {} CPU cores", cpu_cores);

        let (share_tx, share_rx) = mpsc::channel(256);

        Ok(Self {
            client,
            pool_difficulty: Arc::new(Mutex::new(Difficulty::default())),
            extranonce1: subscribe.extranonce1,
            share_rx,
            share_tx,
            cancel: CancellationToken::new(),
            cpu_cores,
            next_extranonce2: Extranonce::zeros(subscribe.extranonce2_size),
            once,
            username,
            shares: Vec::new(),
        })
    }

    pub(crate) async fn run(mut self) -> Result<Vec<Share>> {
        loop {
            tokio::select! {
                _ = ctrl_c() => {
                    info!("Shutting down stratum client and hasher");
                    break;
                },
                maybe = self.client.incoming.recv() => match maybe {
                    Some(msg) => {
                        match msg {
                            Message::Notification { method, params } => {
                                self.handle_notification(method, params).await?;
                            }
                            _ => warn!("Unexpected message on incoming: {:?}", msg)
                        }
                    }
                    None => {
                        info!("Incoming closed, shutting down");
                        break;
                    }
                },
                maybe = self.share_rx.recv() => match maybe {
                    Some((job_id, header, extranonce2, hash_rate)) => {
                        info!("Valid share found: blockhash={} nonce={}", header.block_hash(), header.nonce);
                        info!("Hash rate: {hash_rate}");

                        let share = Share {
                            extranonce1: self.extranonce1.clone(),
                            extranonce2: extranonce2.clone(),
                            job_id,
                            nonce: header.nonce.into(),
                            ntime: header.time.into(),
                            username: self.username.clone(),
                            version_bits: None,
                        };

                        self.shares.push(share);

                        match self.client.submit(job_id, extranonce2, header.time.into(), header.nonce.into()).await {
                            Err(err) => warn!("Failed to submit share: {err}"),
                            Ok(_) => info!("Share submitted successfully"),
                        }

                        if self.once {
                            info!("Share found, exiting");
                            break;
                        }
                    }
                    None => {
                        info!("Share channel closed");
                        break;
                    }
                }
            }
        }

        self.cancel();
        self.client.disconnect().await?;

        Ok(self.shares)
    }

    async fn handle_notification(&mut self, method: String, params: Value) -> Result {
        match method.as_str() {
            "mining.notify" => {
                let notify = serde_json::from_value::<Notify>(params)?;

                if notify.clean_jobs {
                    self.cancel();
                }

                let network_nbits: CompactTarget = notify.nbits.into();
                let network_target: Target = network_nbits.into();
                let pool_target = self.pool_difficulty.lock().await.to_target();

                info!("New job received: {}", notify.job_id);
                info!("Network target:\t{}", target_as_block_hash(network_target));
                info!("Pool target:\t\t{}", target_as_block_hash(pool_target));

                let share_tx = self.share_tx.clone();

                for core_id in 0..self.cpu_cores {
                    let extranonce2 = self.next_extranonce2();

                    let mut hasher = Hasher {
                        header: Header {
                            version: notify.version.into(),
                            prev_blockhash: notify.prevhash.clone().into(),
                            merkle_root: stratum::merkle_root(
                                &notify.coinb1,
                                &notify.coinb2,
                                &self.extranonce1,
                                &extranonce2,
                                &notify.merkle_branches,
                            )?
                            .into(),
                            time: notify.ntime.into(),
                            bits: notify.nbits.into(),
                            nonce: 0,
                        },
                        pool_target,
                        extranonce2: extranonce2.clone(),
                        job_id: notify.job_id,
                    };

                    let share_tx_clone = share_tx.clone();
                    let mining_cancel = self.cancel.clone();

                    info!(
                        "Starting hasher for core {}: extranonce2={}",
                        core_id, extranonce2
                    );

                    tokio::spawn(async move {
                        match hasher.hash(mining_cancel) {
                            Ok(share) => {
                                let _ = share_tx_clone.send(share).await;
                            }
                            Err(err) => warn!("Hasher failed on core {core_id}: {err}"),
                        }
                    });
                }
            }
            "mining.set_difficulty" => {
                // TODO: if current diff is different from new one then cancel all running hashers
                let difficulty = serde_json::from_value::<SetDifficulty>(params)?.difficulty();
                *self.pool_difficulty.lock().await = difficulty;
                info!("Updated pool difficulty: {difficulty}");
                info!(
                    "Updated pool target:\t{}",
                    target_as_block_hash(difficulty.to_target())
                );
            }
            _ => warn!("Unhandled notification: {}", method),
        }

        Ok(())
    }

    fn cancel(&mut self) {
        self.cancel.cancel();
        self.cancel = CancellationToken::new();
    }

    fn next_extranonce2(&mut self) -> Extranonce {
        let extranonce2 = self.next_extranonce2.clone();
        self.next_extranonce2.increment_wrapping();

        extranonce2
    }
}
