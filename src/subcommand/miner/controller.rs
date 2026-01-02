use super::hasher::HashResult;
use super::*;

#[derive(Debug, Clone, Copy)]
pub struct VersionRollingConfig {
    pub enabled: bool,
    pub mask: u32,
}

impl Default for VersionRollingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            mask: BIP320_VERSION_MASK,
        }
    }
}

impl VersionRollingConfig {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            mask: 0,
        }
    }

    pub fn with_mask(mask: u32) -> Self {
        Self {
            enabled: mask != 0,
            mask,
        }
    }
}

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
    share_tx: mpsc::Sender<HashResult>,
    share_rx: mpsc::Receiver<HashResult>,
    shares: Vec<Share>,
    throttle: f64,
    username: Username,
    version_rolling: VersionRollingConfig,
}

impl Controller {
    pub(crate) async fn run_with_version_rolling(
        client: Client,
        username: Username,
        cpu_cores: usize,
        throttle: Option<HashRate>,
        mode: Mode,
        cancel_token: CancellationToken,
        version_rolling: VersionRollingConfig,
    ) -> Result<Vec<Share>> {
        let events = client
            .connect()
            .await
            .context("failed to connect to stratum server")?;

        let (subscribe, _, _) = client
            .subscribe()
            .await
            .context("stratum mining.subscribe failed")?;

        client
            .authorize()
            .await
            .context("stratum mining.authorize failed")?;

        info!(
            "Authorized: extranonce1={}, extranonce2_size={}",
            subscribe.extranonce1, subscribe.extranonce2_size
        );

        info!("Controller initialized with {} CPU cores", cpu_cores);
        if version_rolling.enabled {
            info!(
                "Version rolling enabled with mask: {:#010x} ({} bits)",
                version_rolling.mask,
                version_rolling.mask.count_ones()
            );
        } else {
            info!("Version rolling disabled");
        }

        let (share_tx, share_rx) = mpsc::channel(256);
        let (notify_tx, notify_rx) = watch::channel(None);

        let throttle = throttle
            .map(|hash_rate| hash_rate.0 / cpu_cores as f64)
            .unwrap_or(f64::MAX);

        let mut controller = Self {
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
            version_rolling,
        };

        controller.spawn_hashers();

        if !integration_test() && !logs_enabled() {
            spawn_throbber(controller.metrics.clone());
        }

        controller.event_loop(events, cancel_token).await?;

        controller.root_cancel.cancel();
        drop(controller.notify_tx);
        while controller.hashers.join_next().await.is_some() {}
        controller.client.disconnect().await?;

        Ok(controller.shares)
    }

    async fn event_loop(
        &mut self,
        mut events: broadcast::Receiver<stratum::Event>,
        cancel_token: CancellationToken,
    ) -> Result {
        loop {
            tokio::select! {
                biased;
                _ = cancel_token.cancelled() => {
                    info!("Shutting down stratum client and hasher");
                    break;
                },
                event = events.recv() => {
                    match event {
                        Ok(stratum::Event::Notify(notify)) => {
                            self.handle_notify(notify).await?;
                        }
                        Ok(stratum::Event::SetDifficulty(difficulty)) => {
                            self.handle_set_difficulty(difficulty).await;
                        }
                        Ok(stratum::Event::Disconnected) => {
                            info!("Disconnected from stratum server. Shutting down...");
                            self.cancel_hashers();
                            break;
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => {
                            warn!("Event loop lagged, missed messages");
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            info!("Client event channel closed, shutting down");
                            break;
                        }
                    }
                },
                maybe = self.share_rx.recv() => match maybe {
                    Some(hash_result) => {
                        self.handle_share_found(hash_result).await?;
                    }
                    None => {
                        info!("Share channel closed");
                        break;
                    }
                }
            }
        }

        Ok(())
    }

    async fn handle_share_found(&mut self, hash_result: HashResult) -> Result<()> {
        let header = &hash_result.header;

        info!(
            "Valid share found: blockhash={} nonce={} version_bits={:?}",
            header.block_hash(),
            header.nonce,
            hash_result.version_bits
        );

        let version_bits = hash_result
            .version_bits
            .map(|bits| Version::from(bits as i32));

        let share = Share {
            extranonce1: self.extranonce1.clone(),
            extranonce2: hash_result.extranonce2.clone(),
            job_id: hash_result.job_id,
            nonce: header.nonce.into(),
            ntime: header.time.into(),
            username: self.username.clone(),
            version_bits,
        };

        self.shares.push(share);

        match self
            .client
            .submit(
                hash_result.job_id,
                hash_result.extranonce2,
                header.time.into(),
                header.nonce.into(),
            )
            .await
        {
            Err(err) => warn!(
                "Failed to submit share for job {}: {err}",
                hash_result.job_id
            ),
            Ok(_) => info!(
                "Share for job {} submitted successfully",
                hash_result.job_id
            ),
        }

        match self.mode {
            Mode::ShareFound => {
                info!("Share found, exiting");
                return Ok(());
            }
            Mode::BlockFound => {
                if header.validate_pow(header.bits.into()).is_ok() {
                    info!("Block found, exiting");
                    return Ok(());
                }
            }
            Mode::Continuous => {}
        }

        Ok(())
    }

    fn spawn_hashers(&mut self) {
        let version_rolling_config = self.version_rolling;

        for core_id in 0..self.cpu_cores {
            let mut notify_rx = self.notify_rx.clone();
            let share_tx = self.share_tx.clone();
            let extranonce1 = self.extranonce1.clone();
            let extranonce2 = self.extranonce2.clone();
            let pool_difficulty = self.pool_difficulty.clone();
            let metrics = self.metrics.clone();
            let throttle = self.throttle;

            info!("Starting hasher for core {core_id}");
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

                        let mut hasher = if !version_rolling_config.enabled {
                            Hasher::without_version_rolling(
                                header,
                                pool_target,
                                extranonce2.clone(),
                                notify.job_id,
                            )
                        } else if version_rolling_config.mask == BIP320_VERSION_MASK {
                            Hasher::new(header, pool_target, extranonce2.clone(), notify.job_id)
                        } else {
                            Hasher::with_version_mask(
                                header,
                                pool_target,
                                extranonce2.clone(),
                                notify.job_id,
                                version_rolling_config.mask,
                            )
                        };

                        let cancel_clone = cancel.clone();
                        let metrics_clone = metrics.clone();

                        let result = task::spawn_blocking(move || {
                            hasher.hash(cancel_clone, metrics_clone, throttle)
                        })
                        .await;

                        match result {
                            Ok(Ok(hash_result)) => {
                                let _ = share_tx.send(hash_result).await;
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
        info!("New job: job_id={}", notify.job_id);

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
