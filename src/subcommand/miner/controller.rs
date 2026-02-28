use super::*;

enum Action {
    Shutdown,
    Reconnect,
}

pub(crate) struct Controller {
    client: Client,
    cpu_cores: usize,
    enonce1: Extranonce,
    enonce2: Arc<Mutex<Extranonce>>,
    hasher_cancel: Option<CancellationToken>,
    hashers: JoinSet<()>,
    metrics: Arc<Metrics>,
    notify_tx: watch::Sender<Option<(Notify, CancellationToken)>>,
    notify_rx: watch::Receiver<Option<(Notify, CancellationToken)>>,
    mode: Mode,
    pool_difficulty: Arc<Mutex<Difficulty>>,
    cancel: CancellationToken,
    share_tx: mpsc::Sender<(JobId, Header, Extranonce, Option<Version>)>,
    share_rx: mpsc::Receiver<(JobId, Header, Extranonce, Option<Version>)>,
    shares: Vec<Share>,
    throttle: f64,
    username: Username,
    version_mask: Option<Version>,
}

impl Controller {
    pub(crate) async fn run(
        client: Client,
        username: Username,
        cpu_cores: usize,
        throttle: Option<HashRate>,
        mode: Mode,
        disable_version_rolling: bool,
        cancel_token: CancellationToken,
    ) -> Result<Vec<Share>> {
        let (share_tx, share_rx) = mpsc::channel(256);
        let (notify_tx, notify_rx) = watch::channel(None);

        let throttle = throttle
            .map(|hashrate| hashrate.0 / cpu_cores as f64)
            .unwrap_or(f64::MAX);

        let mut controller = Self {
            client,
            cpu_cores,
            enonce1: Extranonce::zeros(0),
            enonce2: Arc::new(Mutex::new(Extranonce::zeros(0))),
            hasher_cancel: None,
            hashers: JoinSet::new(),
            metrics: Arc::new(Metrics::new()),
            notify_rx,
            notify_tx,
            mode,
            pool_difficulty: Arc::new(Mutex::new(Difficulty::default())),
            cancel: cancel_token.clone(),
            share_rx,
            share_tx,
            shares: Vec::new(),
            throttle,
            username,
            version_mask: None,
        };

        let mut events = controller.connect(disable_version_rolling).await?;

        info!("Controller initialized with {} CPU cores", cpu_cores);

        controller.spawn_hashers();
        controller.maybe_spawn_throbber(&cancel_token);

        let mut backoff = Duration::from_secs(1);

        loop {
            match controller.event_loop(events, cancel_token.clone()).await? {
                Action::Shutdown => break,
                Action::Reconnect => {
                    controller.cancel_hashers();
                    controller.notify_tx.send_replace(None);
                    controller.hashers.abort_all();
                    while controller.hashers.join_next().await.is_some() {}
                    tokio::select! {
                        _ = controller.client.disconnect() => {}
                        _ = cancel_token.cancelled() => {
                            return Ok(controller.shares);
                        }
                    }

                    let mut max_backoff_attempts = 0u32;

                    events = loop {
                        info!("Reconnecting in {}s...", backoff.as_secs());

                        tokio::select! {
                            _ = sleep(backoff) => {}
                            _ = cancel_token.cancelled() => {
                                return Ok(controller.shares);
                            }
                        }

                        backoff = (backoff * 2).min(Duration::from_secs(60));

                        if backoff >= Duration::from_secs(60) {
                            max_backoff_attempts += 1;
                            if max_backoff_attempts >= 3 {
                                bail!(
                                    "Upstream unreachable after {max_backoff_attempts} attempts at max backoff"
                                );
                            }
                        }

                        tokio::select! {
                            result = controller.connect(disable_version_rolling) => {
                                match result {
                                    Ok(new_events) => break new_events,
                                    Err(err) => {
                                        warn!("Reconnect failed: {err}");
                                        controller.client.disconnect().await;
                                    }
                                }
                            }
                            _ = cancel_token.cancelled() => {
                                return Ok(controller.shares);
                            }
                        }
                    };

                    backoff = Duration::from_secs(1);
                    controller.spawn_hashers();
                    controller.maybe_spawn_throbber(&cancel_token);
                }
            }
        }

        controller.cancel.cancel();
        drop(controller.notify_tx);
        while controller.hashers.join_next().await.is_some() {}
        controller.client.disconnect().await;

        Ok(controller.shares)
    }

    async fn connect(
        &mut self,
        disable_version_rolling: bool,
    ) -> Result<stratum::client::EventReceiver> {
        let events = self
            .client
            .connect()
            .await
            .context("failed to connect to stratum server")?;

        self.version_mask = if disable_version_rolling {
            info!("Version rolling disabled");
            None
        } else {
            match self
                .client
                .configure(
                    vec!["version-rolling".to_string()],
                    Some(Version::from_str("ffffffff")?),
                )
                .await
            {
                Ok((response, _, _)) => {
                    if response.version_rolling {
                        info!(
                            "Version rolling enabled: mask={:?}",
                            response.version_rolling_mask
                        );
                        response.version_rolling_mask
                    } else {
                        info!("Server does not support version rolling");
                        None
                    }
                }
                Err(err) => {
                    warn!("Failed to configure version rolling: {err}");
                    None
                }
            }
        };

        let (subscribe, _, _) = self
            .client
            .subscribe()
            .await
            .context("stratum mining.subscribe failed")?;

        self.client
            .authorize()
            .await
            .context("stratum mining.authorize failed")?;

        info!(
            "Authorized: enonce1={}, enonce2_size={}",
            subscribe.enonce1, subscribe.enonce2_size
        );

        self.enonce1 = subscribe.enonce1;
        self.enonce2 = Arc::new(Mutex::new(Extranonce::zeros(subscribe.enonce2_size)));

        Ok(events)
    }

    async fn event_loop(
        &mut self,
        mut events: stratum::client::EventReceiver,
        cancel_token: CancellationToken,
    ) -> Result<Action> {
        loop {
            tokio::select! {
                biased;
                _ = cancel_token.cancelled() => {
                    info!("Shutting down stratum client and hasher");
                    return Ok(Action::Shutdown);
                },
                event = events.recv() => {
                    match event {
                        Ok(stratum::client::Event::Notify(notify)) => {
                            self.handle_notify(notify).await?;
                        }
                        Ok(stratum::client::Event::SetDifficulty(difficulty)) => {
                            self.handle_set_difficulty(difficulty);
                        }
                        Ok(stratum::client::Event::Reconnect(_)) => {
                            info!("Received client.reconnect from server");
                            self.cancel_hashers();
                            return Ok(Action::Reconnect);
                        }
                        Ok(stratum::client::Event::Disconnected) => {
                            info!("Disconnected from stratum server");
                            self.cancel_hashers();
                            return Ok(Action::Reconnect);
                        }
                        Err(stratum::client::ClientError::EventsLagged { count }) => {
                            warn!("Event loop lagged, missed {count} messages");
                        }
                        Err(stratum::client::ClientError::EventChannelClosed) => {
                            info!("Client event channel closed, shutting down");
                            return Ok(Action::Shutdown);
                        }
                        Err(e) => {
                            warn!("Unexpected event error: {e}");
                        }
                    }
                },
                maybe = self.share_rx.recv() => match maybe {
                    Some((job_id, header, enonce2, version_bits)) => {
                        info!(
                            "Valid share found with difficulty={} version_bits={:?}",
                            Difficulty::from(header.block_hash()),
                            version_bits
                        );

                        let share = Share {
                            enonce1: self.enonce1.clone(),
                            enonce2: enonce2.clone(),
                            job_id,
                            nonce: header.nonce.into(),
                            ntime: header.time.into(),
                            username: self.username.clone(),
                            version_bits,
                        };

                        self.shares.push(share);

                        match self.client.submit(job_id, enonce2, header.time.into(), header.nonce.into(), version_bits).await {
                            Err(err) => warn!("Failed to submit share for job {job_id}: {err}"),
                            Ok(_) => info!("Share for job {job_id} submitted successfully"),
                        }

                        match self.mode {
                            Mode::ShareFound => {
                                info!("Share found, exiting");
                                return Ok(Action::Shutdown);
                            },
                            Mode::BlockFound => {
                                if header.validate_pow(header.bits.into()).is_ok() {
                                    info!("Block found, exiting");
                                    return Ok(Action::Shutdown);
                                }
                            }
                            Mode::Continuous => continue,
                        }
                    }
                    None => {
                        info!("Share channel closed");
                        return Ok(Action::Shutdown);
                    }
                }
            }
        }
    }

    fn spawn_hashers(&mut self) {
        for core_id in 0..self.cpu_cores {
            let mut notify_rx = self.notify_rx.clone();
            let share_tx = self.share_tx.clone();
            let enonce1 = self.enonce1.clone();
            let enonce2 = self.enonce2.clone();
            let pool_difficulty = self.pool_difficulty.clone();
            let metrics = self.metrics.clone();
            let throttle = self.throttle;
            let version_mask = self.version_mask;

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

                        let enonce2 = {
                            let mut guard = enonce2.lock();
                            let enonce2 = guard.clone();
                            guard.increment_wrapping();
                            enonce2
                        };

                        let merkle_root = stratum::merkle_root(
                            &notify.coinb1,
                            &notify.coinb2,
                            &enonce1,
                            &enonce2,
                            &notify.merkle_branches,
                        )
                        .expect("merkle root should calculate");

                        let header = Header {
                            version: notify.version.into(),
                            prev_blockhash: notify.prevhash.clone().into(),
                            merkle_root: merkle_root.into(),
                            time: notify.ntime.into(),
                            bits: notify.nbits.into(),
                            nonce: 0,
                        };

                        let pool_target = { pool_difficulty.lock().to_target() };

                        let mut hasher = Hasher {
                            version: notify.version,
                            header,
                            pool_target,
                            enonce2: enonce2.clone(),
                            job_id: notify.job_id,
                            version_mask,
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

    fn handle_set_difficulty(&mut self, difficulty: Difficulty) {
        *self.pool_difficulty.lock() = difficulty;
        info!("Updated pool difficulty: {difficulty}");
        info!(
            "Updated pool target:\t{}",
            target_as_block_hash(difficulty.to_target())
        );
    }

    fn maybe_spawn_throbber(&mut self, cancel_token: &CancellationToken) {
        if !integration_test() && !logs_enabled() {
            spawn_throbber(
                self.metrics.clone(),
                cancel_token.clone(),
                &mut self.hashers,
            );
        }
    }

    fn cancel_hashers(&mut self) -> CancellationToken {
        if let Some(cancel) = &self.hasher_cancel {
            cancel.cancel();
        }
        let cancel = self.cancel.child_token();
        self.hasher_cancel = Some(cancel.clone());
        cancel
    }
}
