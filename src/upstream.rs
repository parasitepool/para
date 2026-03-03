use {
    super::*,
    stratum::client::{Client, ClientError, Event},
};

pub(crate) struct UpstreamSubmit {
    pub(crate) job_id: JobId,
    pub(crate) enonce2: Extranonce,
    pub(crate) nonce: Nonce,
    pub(crate) ntime: Ntime,
    pub(crate) version_bits: Option<Version>,
    pub(crate) share_diff: Difficulty,
}

pub(crate) struct Upstream {
    id: u32,
    client: Client,
    endpoint: String,
    enonce1: Extranonce,
    enonce2_size: usize,
    connected: Arc<AtomicBool>,
    ping: Arc<RwLock<Duration>>,
    difficulty: Arc<RwLock<Difficulty>>,
    accepted: Arc<AtomicU64>,
    rejected: Arc<AtomicU64>,
    accepted_work: Arc<Mutex<TotalWork>>,
    rejected_work: Arc<Mutex<TotalWork>>,
    version_mask: Option<Version>,
    disconnect_notify: Arc<tokio::sync::Notify>,
    workbase_rx: watch::Receiver<Arc<Notify>>,
}

impl Upstream {
    pub(crate) async fn connect(
        id: u32,
        target: &UpstreamTarget,
        timeout: Duration,
        cancel: CancellationToken,
        tasks: &mut JoinSet<()>,
    ) -> Result<Arc<Self>> {
        let upstream_addr = resolve_stratum_endpoint(target.endpoint())
            .await
            .with_context(|| {
                format!(
                    "failed to resolve upstream endpoint `{}`",
                    target.endpoint()
                )
            })?;

        info!(
            "Connecting to upstream {} ({}) as {}",
            target.endpoint(),
            upstream_addr,
            target.username()
        );

        let client = Client::new(
            upstream_addr.to_string(),
            target.username().clone(),
            target.password().map(String::from),
            USER_AGENT.into(),
            timeout,
        );

        let mut events = client
            .connect()
            .await
            .context("failed to connect to upstream")?;

        let version_mask = match client
            .configure(
                vec!["version-rolling".to_string()],
                Some(Version::from_str("1fffe000").expect("valid hex")),
            )
            .await
        {
            Ok((response, ..)) => {
                if response.version_rolling {
                    if let Some(mask) = response.version_rolling_mask {
                        info!("Upstream supports version rolling: mask={mask}",);
                    }
                    response.version_rolling_mask
                } else {
                    info!("Upstream does not support version rolling");
                    None
                }
            }
            Err(e) => {
                warn!("Failed to negotiate version rolling with upstream: {e}");
                None
            }
        };

        let (subscribe, ..) = client
            .subscribe()
            .await
            .context("failed to subscribe to upstream")?;

        info!(
            "Subscribed to upstream: enonce1={}, enonce2_size={}",
            subscribe.enonce1, subscribe.enonce2_size
        );

        let (ping, _) = client
            .authorize()
            .await
            .context("failed to authorize with upstream")?;

        info!(
            "Authorized to upstream {} with {}",
            client.address(),
            client.username()
        );

        let mut initial_difficulty: Option<Difficulty> = None;
        let mut first_notify: Option<Notify> = None;

        let (initial_difficulty, first_notify) = loop {
            match events.recv().await {
                Ok(Event::SetDifficulty(diff)) => {
                    info!("Received initial difficulty: {}", diff);
                    initial_difficulty = Some(diff);
                }
                Ok(Event::Notify(notify)) => {
                    info!(
                        "Received job: job_id={}, clean_jobs={}",
                        notify.job_id, notify.clean_jobs
                    );
                    first_notify = Some(notify);
                }
                Ok(Event::Reconnect(_)) | Ok(Event::Disconnected) => {
                    bail!("Disconnected from upstream before initialization complete");
                }
                Err(e) => {
                    bail!("Upstream error during initialization: {e}");
                }
            }

            if let Some(diff) = initial_difficulty
                && let Some(notify) = first_notify.take()
            {
                break (diff, notify);
            }
        };

        let difficulty = Arc::new(RwLock::new(initial_difficulty));
        let connected = Arc::new(AtomicBool::new(true));
        let disconnect_notify = Arc::new(tokio::sync::Notify::new());
        let (workbase_tx, workbase_rx) = watch::channel(Arc::new(first_notify));

        let connected_clone = connected.clone();
        let difficulty_clone = difficulty.clone();
        let disconnect_clone = disconnect_notify.clone();

        tasks.spawn(async move {
            loop {
                tokio::select! {
                    biased;

                    _ = cancel.cancelled() => {
                        break;
                    }

                    event = events.recv() => {
                        match event {
                            Ok(Event::Notify(notify)) => {
                                info!(
                                    "Received notify: job_id={}, clean_jobs={}",
                                    notify.job_id, notify.clean_jobs
                                );
                                workbase_tx.send_replace(Arc::new(notify));
                            }
                            Ok(Event::SetDifficulty(diff)) => {
                                info!("Received set_difficulty: {}", diff);
                                *difficulty_clone.write() = diff;
                            }
                            Ok(Event::Reconnect(_)) | Ok(Event::Disconnected) => {
                                warn!("Disconnected from upstream");
                                connected_clone.store(false, Ordering::Relaxed);
                                disconnect_clone.notify_waiters();
                                break;
                            }
                            Err(err) => {
                                error!("Upstream event error: {}", err);
                                connected_clone.store(false, Ordering::Relaxed);
                                disconnect_clone.notify_waiters();
                                break;
                            }
                        }
                    }
                }
            }
        });

        Ok(Arc::new(Self {
            id,
            client,
            endpoint: target.endpoint().to_string(),
            enonce1: subscribe.enonce1,
            enonce2_size: subscribe.enonce2_size,
            connected,
            ping: Arc::new(RwLock::new(ping)),
            difficulty,
            accepted: Arc::new(AtomicU64::new(0)),
            rejected: Arc::new(AtomicU64::new(0)),
            accepted_work: Arc::new(Mutex::new(TotalWork::ZERO)),
            rejected_work: Arc::new(Mutex::new(TotalWork::ZERO)),
            version_mask,
            disconnect_notify,
            workbase_rx,
        }))
    }

    pub(crate) fn workbase_rx(&self) -> watch::Receiver<Arc<Notify>> {
        self.workbase_rx.clone()
    }

    pub(crate) async fn submit_share(&self, submit: UpstreamSubmit) {
        let upstream_diff = *self.difficulty.read();
        if submit.share_diff < upstream_diff {
            debug!(
                "Share below upstream difficulty: share_diff={} < upstream_diff={}",
                submit.share_diff, upstream_diff
            );
            return;
        }

        debug!(
            "Submitting share to upstream: job_id={}, share_diff={}, upstream_diff={}",
            submit.job_id, submit.share_diff, upstream_diff
        );

        let client = self.client.clone();
        let accepted = self.accepted.clone();
        let rejected = self.rejected.clone();
        let accepted_work = self.accepted_work.clone();
        let rejected_work = self.rejected_work.clone();
        let ping = self.ping.clone();

        tokio::spawn(async move {
            match client
                .submit(
                    submit.job_id,
                    submit.enonce2,
                    submit.ntime,
                    submit.nonce,
                    submit.version_bits,
                )
                .await
            {
                Ok(duration) => {
                    accepted.fetch_add(1, Ordering::Relaxed);
                    *accepted_work.lock() += TotalWork::from_difficulty(upstream_diff);
                    let mut ping = ping.write();
                    *ping = duration;

                    debug!("Upstream accepted share");
                }
                Err(ClientError::SubmitFalse) => {
                    rejected.fetch_add(1, Ordering::Relaxed);
                    *rejected_work.lock() += TotalWork::from_difficulty(upstream_diff);
                    warn!("Upstream rejected share");
                }
                Err(ClientError::Rejected { reason, .. }) => {
                    rejected.fetch_add(1, Ordering::Relaxed);
                    *rejected_work.lock() += TotalWork::from_difficulty(upstream_diff);
                    warn!("Upstream rejected share: {}", reason);
                }
                Err(e) => {
                    warn!("Upstream submit error: {e}");
                }
            }
        });
    }

    pub(crate) fn id(&self) -> u32 {
        self.id
    }

    pub(crate) fn enonce1(&self) -> &Extranonce {
        &self.enonce1
    }

    pub(crate) fn enonce2_size(&self) -> usize {
        self.enonce2_size
    }

    pub(crate) fn difficulty(&self) -> Difficulty {
        *self.difficulty.read()
    }

    pub(crate) fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    pub(crate) async fn disconnected(&self) {
        self.disconnect_notify.notified().await;
    }

    pub(crate) fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub(crate) fn username(&self) -> &Username {
        self.client.username()
    }

    pub(crate) fn accepted(&self) -> u64 {
        self.accepted.load(Ordering::Relaxed)
    }

    pub(crate) fn rejected(&self) -> u64 {
        self.rejected.load(Ordering::Relaxed)
    }

    pub(crate) fn accepted_work(&self) -> TotalWork {
        *self.accepted_work.lock()
    }

    pub(crate) fn rejected_work(&self) -> TotalWork {
        *self.rejected_work.lock()
    }

    pub(crate) fn version_mask(&self) -> Option<Version> {
        self.version_mask
    }

    pub(crate) fn ping_ms(&self) -> u128 {
        self.ping.read().as_millis()
    }
}
