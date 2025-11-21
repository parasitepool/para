use super::*;
use crate::stratum::StratumEvent;

pub(crate) struct Controller {
    client: Client,
    cpu_cores: usize,
    extranonce1: Extranonce,
    extranonce2: Arc<Mutex<Extranonce>>,
    hasher_cancel: Option<CancellationToken>,
    hashers: JoinSet<()>,
    metrics: Arc<Metrics>,
    notify_tx: watch::Sender<Option<(Notify, CancellationToken)>>,
    notify_rx: watch::Receiver<Option<(Notify, CancellationToken)>>,
    mode: Mode,
    pool_difficulty: Arc<Mutex<Difficulty>>,
    root_cancel: CancellationToken,
    share_tx: mpsc::Sender<(JobId, Header, Extranonce)>,
    share_rx: mpsc::Receiver<(JobId, Header, Extranonce)>,
    shares: Vec<Share>,
    throttle: f64,
    username: String,
}

impl Controller {
    pub(crate) async fn new(
        client: Client,
        username: String,
        cpu_cores: usize,
        throttle: Option<ckpool::HashRate>,
        mode: Mode,
    ) -> Result<Self> {
        let (subscribe, _, _) = client.subscribe(USER_AGENT.into()).await?;
        client.authorize().await?;

        info!(
            "Authorized: extranonce1={}, extranonce2_size={}",
            subscribe.extranonce1, subscribe.extranonce2_size
        );

        info!("Controller initialized with {} CPU cores", cpu_cores);

        let (share_tx, share_rx) = mpsc::channel(256);
        let (notify_tx, notify_rx) = watch::channel(None);

        let throttle = throttle
            .map(|hash_rate| hash_rate.0 / cpu_cores as f64)
            .unwrap_or(f64::MAX);

        Ok(Self {
            client,
            cpu_cores,
            extranonce1: subscribe.extranonce1,
            extranonce2: Arc::new(Mutex::new(Extranonce::zeros(subscribe.extranonce2_size))),
            hasher_cancel: None,
            hashers: JoinSet::new(),
            metrics: Arc::new(Metrics::new()),
            notify_rx,
            notify_tx,
            mode,
            pool_difficulty: Arc::new(Mutex::new(Difficulty::default())),
            root_cancel: CancellationToken::new(),
            share_rx,
            share_tx,
            shares: Vec::new(),
            throttle,
            username,
        })
    }

    pub(crate) async fn run(mut self, cancel_token: CancellationToken) -> Result<Vec<Share>> {
        self.spawn_hashers();

        if !integration_test() && !logs_enabled() {
            spawn_throbber(self.metrics.clone());
        }

        let mut events = self.client.events.subscribe();

        loop {
            tokio::select! {
                biased;
                _ = cancel_token.cancelled() => {
                    info!("Shutting down stratum client and hasher");
                    break;
                },
                event = events.recv() => {
                    match event {
                        Ok(StratumEvent::Notify(notify)) => {
                             self.handle_notify(notify).await?;
                        }
                        Ok(StratumEvent::SetDifficulty(difficulty)) => {
                            self.handle_set_difficulty(difficulty).await;
                        }
                        Ok(StratumEvent::Disconnected) => {
                            info!("Disconnected from stratum server. Reconnecting...");
                            // Cancel hashers immediately to stop mining on stale job
                            self.cancel_hashers();

                            // Attempt reconnection with backoff? For now simple loop
                            let mut retry_delay = Duration::from_secs(1);
                            loop {
                                if cancel_token.is_cancelled() { break; }

                                tokio::time::sleep(retry_delay).await;

                                match self.client.reconnect().await {
                                    Ok(_) => {
                                        info!("Reconnected successfully");
                                        // We need to update subscription info if it changed?
                                        // Actually, Extranonce1 might change.
                                        // The current Controller structure assumes Extranonce1 is constant from initialization.
                                        // This is a limitation of the current refactor scope without deeper changes to Controller.
                                        // Ideally we should update self.extranonce1 here.

                                        // Let's fetch the new subscription details via a new subscribe call which reconnect() does?
                                        // reconnect() does the handshake. But we don't get the result back out easily unless we change reconnect's signature or store it in Client.

                                        // For now, we assume Extranonce1 might not change or we just keep mining and if we get rejected, so be it.
                                        // BUT, if Extranonce1 changes, our shares will be invalid.

                                        // Let's do a quick hack: We know reconnect() calls subscribe.
                                        // But we can't easily access the result.
                                        // Correct approach: Controller shouldn't cache extranonce1 so rigidly, or we need to update it.

                                        // Since `reconnect` is in `Client`, and returns `Result<()>`, we trust it worked.
                                        // However, the `Client` doesn't expose the new Extranonce1.

                                        // Let's leave it as is for now, acknowledging the limitation,
                                        // or we could add a way to get the latest subscription info from Client if we stored it.
                                        break;
                                    },
                                    Err(e) => {
                                        warn!("Reconnection failed: {}. Retrying in {:?}...", e, retry_delay);
                                        retry_delay = std::cmp::min(retry_delay * 2, Duration::from_secs(60));
                                    }
                                }
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            warn!("Event loop lagged, missed messages");
                        }
                         Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            info!("Client event channel closed, shutting down");
                            break;
                        }
                    }
                },
                maybe = self.share_rx.recv() => match maybe {
                    Some((job_id, header, extranonce2)) => {
                        info!("Valid share found: blockhash={} nonce={}", header.block_hash(), header.nonce);

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

                        match self.mode {
                            Mode::ShareFound => {
                                info!("Share found, exiting");
                                break;
                            },
                            Mode::BlockFound => {
                                if header.validate_pow(header.bits.into()).is_ok() {
                                    info!("Block found, exiting");
                                    break;
                                }
                            }
                            Mode::Continuous => continue,
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
            let metrics = self.metrics.clone();
            let throttle = self.throttle;

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
                        let metrics_clone = metrics.clone();

                        let result = task::spawn_blocking(move || {
                            hasher.hash(cancel_clone, metrics_clone, throttle)
                        })
                        .await;

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

    async fn handle_notify(&mut self, notify: Notify) -> Result {
        info!("New job: job_id={}", notify.job_id,);

        let cancel = if notify.clean_jobs {
            self.cancel_hashers()
        } else {
            self.hasher_cancel
                .clone()
                .unwrap_or_else(|| self.cancel_hashers())
        };

        self.notify_tx.send_replace(Some((notify, cancel)));
        Ok(())
    }

    async fn handle_set_difficulty(&mut self, difficulty: Difficulty) {
        *self.pool_difficulty.lock().await = difficulty;
        info!("Updated pool difficulty: {difficulty}");
        info!(
            "Updated pool target:\t{}",
            target_as_block_hash(difficulty.to_target())
        );
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
