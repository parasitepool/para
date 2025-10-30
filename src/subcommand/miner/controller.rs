use super::*;

pub(crate) struct Controller {
    client: Client,
    pool_difficulty: Arc<Mutex<Difficulty>>,
    extranonce1: Extranonce,
    extranonce2: Arc<Mutex<Extranonce>>,
    share_tx: mpsc::Sender<(JobId, Header, Extranonce, ckpool::HashRate)>,
    share_rx: mpsc::Receiver<(JobId, Header, Extranonce, ckpool::HashRate)>,
    notify_tx: watch::Sender<Option<(Notify, CancellationToken)>>,
    notify_rx: watch::Receiver<Option<(Notify, CancellationToken)>>,
    root_cancel: CancellationToken,
    hasher_cancel: Option<CancellationToken>,
    hashers: JoinSet<()>,
    cpu_cores: usize,
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
        let (notify_tx, notify_rx) = watch::channel(None);

        Ok(Self {
            client,
            pool_difficulty: Arc::new(Mutex::new(Difficulty::default())),
            extranonce1: subscribe.extranonce1,
            extranonce2: Arc::new(Mutex::new(Extranonce::zeros(subscribe.extranonce2_size))),
            share_tx,
            share_rx,
            notify_tx,
            notify_rx,
            root_cancel: CancellationToken::new(),
            hasher_cancel: None,
            hashers: JoinSet::new(),
            cpu_cores,
            once,
            username,
            shares: Vec::new(),
        })
    }

    pub(crate) async fn run(mut self) -> Result<Vec<Share>> {
        self.spawn_hashers();

        loop {
            tokio::select! {
                biased;
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
                            Err(err) => warn!("Failed to submit share for job {job_id}: {err}"),
                            Ok(_) => info!("Share for job {job_id} submitted successfully"),
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

        self.root_cancel.cancel();
        drop(self.notify_tx);
        while self.hashers.join_next().await.is_some() {}
        self.client.disconnect().await?;

        Ok(self.shares)
    }

    fn spawn_hashers(&mut self) {
        for core_id in 0..self.cpu_cores {
            let mut notify_rx = self.notify_rx.clone();
            let share_tx = self.share_tx.clone();
            let extranonce1 = self.extranonce1.clone();
            let extranonce2 = self.extranonce2.clone();
            let pool_difficulty = self.pool_difficulty.clone();

            info!("Starting hasher for core {core_id}",);
            self.hashers.spawn(async move {
                loop {
                    if notify_rx.changed().await.is_err() {
                        break;
                    }

                    let Some((notify, cancel)) = notify_rx.borrow().clone() else {
                        continue;
                    };

                    loop {
                        if cancel.is_cancelled() {
                            break;
                        }

                        let extranonce2 = {
                            let mut guard = extranonce2.lock().await;
                            let extranonce2 = guard.clone();
                            guard.increment_wrapping();
                            extranonce2
                        };

                        let merkle = stratum::merkle_root(
                            &notify.coinb1,
                            &notify.coinb2,
                            &extranonce1,
                            &extranonce2,
                            &notify.merkle_branches,
                        )
                        .expect("merkle");

                        let header = Header {
                            version: notify.version.into(),
                            prev_blockhash: notify.prevhash.clone().into(),
                            merkle_root: merkle.into(),
                            time: notify.ntime.into(),
                            bits: notify.nbits.into(),
                            nonce: 0,
                        };

                        let pool_target = { pool_difficulty.lock().await.to_target() };

                        let mut hasher = Hasher {
                            header,
                            pool_target,
                            extranonce2: extranonce2.clone(),
                            job_id: notify.job_id,
                        };

                        let cancel_clone = cancel.clone();
                        let result = task::spawn_blocking(move || hasher.hash(cancel_clone)).await;

                        match result {
                            Ok(Ok(share)) => {
                                let _ = share_tx.send(share).await;
                            }
                            Ok(Err(err)) => {
                                warn!("Hasher failed on core {core_id}: {err}");
                                if cancel.is_cancelled() {
                                    break;
                                }
                                continue;
                            }
                            Err(_) => break,
                        }
                    }
                }
            });
        }
    }

    async fn handle_notification(&mut self, method: String, params: Value) -> Result {
        match method.as_str() {
            "mining.notify" => {
                let notify = serde_json::from_value::<Notify>(params)?;

                info!("New job: job_id={}", notify.job_id,);

                let cancel = if notify.clean_jobs {
                    self.cancel_hashers()
                } else {
                    self.hasher_cancel
                        .clone()
                        .unwrap_or_else(|| self.cancel_hashers())
                };

                self.notify_tx.send_replace(Some((notify, cancel)));
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

    fn cancel_hashers(&mut self) -> CancellationToken {
        if let Some(cancel) = &self.hasher_cancel {
            cancel.cancel();
        }
        let cancel = self.root_cancel.child_token();
        self.hasher_cancel = Some(cancel.clone());
        cancel
    }
}
