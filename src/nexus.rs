use {
    super::*,
    stratum::{Client, ClientConfig, Event, EventReceiver, SubmitOutcome},
    tokio::sync::RwLock,
};

pub(crate) struct Nexus {
    client: Client,
    enonce1: Extranonce,
    enonce2_size: usize,
    connected: Arc<AtomicBool>,
    upstream: String,
    upstream_diff: Arc<RwLock<Difficulty>>,
    upstream_accepted: Arc<AtomicU64>,
    upstream_rejected: Arc<AtomicU64>,
}

impl Nexus {
    pub(crate) async fn connect(settings: Arc<Settings>) -> Result<(Self, EventReceiver)> {
        let username = settings.upstream_username()?;
        let upstream = settings.upstream()?;
        let upstream_addr = resolve_stratum_endpoint(upstream)
            .await
            .with_context(|| format!("failed to resolve upstream endpoint `{upstream}`"))?;

        info!(
            "Connecting to upstream {} ({}) as {}",
            upstream, upstream_addr, username
        );

        let client = Client::new(ClientConfig {
            address: upstream_addr.to_string(),
            username: username.clone(),
            user_agent: USER_AGENT.into(),
            password: settings.upstream_password(),
            timeout: settings.timeout(),
        });

        let events = client
            .connect()
            .await
            .context("failed to connect to upstream")?;

        let (subscribe_result, _, _) = client
            .subscribe()
            .await
            .context("failed to subscribe to upstream")?;

        info!(
            "Subscribed to upstream: enonce1={}, enonce2_size={}",
            subscribe_result.enonce1, subscribe_result.enonce2_size
        );

        Ok((
            Self {
                client,
                enonce1: subscribe_result.enonce1,
                enonce2_size: subscribe_result.enonce2_size,
                connected: Arc::new(AtomicBool::new(false)),
                upstream: upstream.to_string(),
                upstream_diff: Arc::new(RwLock::new(Difficulty::from(1))),
                upstream_accepted: Arc::new(AtomicU64::new(0)),
                upstream_rejected: Arc::new(AtomicU64::new(0)),
            },
            events,
        ))
    }

    pub(crate) async fn spawn(
        self: Arc<Self>,
        mut events: EventReceiver,
        cancel: CancellationToken,
        tasks: &mut JoinSet<()>,
    ) -> Result<(watch::Receiver<Arc<Notify>>, mpsc::Sender<Share>)> {
        self.client
            .authorize()
            .await
            .context("failed to authorize with upstream")?;

        info!(
            "Authorized with upstream as {}",
            self.client.config.username
        );

        self.connected.store(true, Ordering::SeqCst);

        let mut initial_difficulty: Option<Difficulty> = None;
        let mut first_notify: Option<Notify> = None;

        loop {
            match events.recv().await {
                Ok(Event::SetDifficulty(diff)) => {
                    info!("Received initial difficulty: {}", diff);
                    *self.upstream_diff.write().await = diff;
                    initial_difficulty = Some(diff);
                }
                Ok(Event::Notify(notify)) => {
                    info!(
                        "Received job: job_id={}, clean_jobs={}",
                        notify.job_id, notify.clean_jobs
                    );
                    first_notify = Some(notify);
                }
                Ok(Event::Disconnected) => {
                    self.connected.store(false, Ordering::SeqCst);
                    bail!("Disconnected from upstream before initialization complete");
                }
                Err(e) => {
                    self.connected.store(false, Ordering::SeqCst);
                    bail!("Upstream error during initialization: {e}");
                }
            }

            if initial_difficulty.is_some() && first_notify.is_some() {
                break;
            }
        }

        let first_notify = first_notify.expect("checked above");

        let (workbase_tx, workbase_rx) = watch::channel(Arc::new(first_notify));
        let (share_tx, mut share_rx) = mpsc::channel::<Share>(SHARE_CHANNEL_CAPACITY);

        let connected = self.connected.clone();
        let upstream_difficulty = self.upstream_diff.clone();

        let nexus = self;
        tasks.spawn(async move {
            loop {
                tokio::select! {
                    biased;

                    _ = cancel.cancelled() => {
                        info!("Shutting down nexus, draining {} pending shares", share_rx.len());

                        while let Ok(share) = share_rx.try_recv() {
                            nexus.submit_share(share).await;
                        }

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
                                *upstream_difficulty.write().await = diff;
                            }
                            Ok(Event::Disconnected) => {
                                warn!("Disconnected from upstream");
                                connected.store(false, Ordering::SeqCst);
                                break;
                            }
                            Err(e) => {
                                error!("Upstream event error: {}", e);
                                connected.store(false, Ordering::SeqCst);
                                break;
                            }
                        }
                    }

                    Some(share) = share_rx.recv() => {
                        nexus.submit_share(share).await;
                    }
                }
            }
        });

        Ok((workbase_rx, share_tx))
    }

    async fn submit_share(&self, share: Share) {
        if !share.result {
            return;
        }

        let upstream_diff = *self.upstream_diff.read().await;
        if share.share_diff < upstream_diff {
            debug!(
                "Share below upstream difficulty: share_diff={} < upstream_diff={}",
                share.share_diff, upstream_diff
            );
            return;
        }

        debug!(
            "Submitting share to upstream: job_id={}, share_diff={}, upstream_diff={}",
            share.job_id, share.share_diff, upstream_diff
        );

        let client = self.client.clone();
        let job_id = share.job_id;
        let ntime = share.ntime;
        let nonce = share.nonce;
        let version_bits = share.version_bits;
        let accepted = self.upstream_accepted.clone();
        let rejected = self.upstream_rejected.clone();

        tokio::spawn(async move {
            match client
                .submit_async(job_id, share.enonce2, ntime, nonce, version_bits)
                .await
            {
                Ok(handle) => match handle.wait().await {
                    Ok(SubmitOutcome::Accepted) => {
                        accepted.fetch_add(1, Ordering::Relaxed);
                        info!("Upstream accepted share");
                    }
                    Ok(SubmitOutcome::Rejected { reason }) => {
                        rejected.fetch_add(1, Ordering::Relaxed);

                        warn!(
                            "Upstream rejected share: {}",
                            reason.as_deref().unwrap_or("unknown")
                        );
                    }
                    Err(e) => warn!("Upstream submit error: {e}"),
                },
                Err(e) => {
                    warn!("Failed to submit share to upstream: {e}");
                }
            }
        });
    }

    pub(crate) fn enonce1(&self) -> &Extranonce {
        &self.enonce1
    }

    pub(crate) fn enonce2_size(&self) -> usize {
        self.enonce2_size
    }

    pub(crate) async fn upstream_difficulty(&self) -> Difficulty {
        *self.upstream_diff.read().await
    }

    pub(crate) fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    pub(crate) fn upstream(&self) -> &str {
        &self.upstream
    }

    pub(crate) fn username(&self) -> &Username {
        &self.client.config.username
    }

    pub(crate) fn upstream_accepted(&self) -> u64 {
        self.upstream_accepted.load(Ordering::Relaxed)
    }

    pub(crate) fn upstream_rejected(&self) -> u64 {
        self.upstream_rejected.load(Ordering::Relaxed)
    }
}
