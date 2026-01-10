use {
    super::*,
    stratum::{Client, ClientConfig, Event, EventReceiver, SubmitOutcome},
    tokio::sync::RwLock,
};

pub(crate) struct Nexus {
    client: Client,
    upstream: String,
    enonce1: Extranonce,
    enonce2_size: usize,
    connected: Arc<AtomicBool>,
    upstream_difficulty: Arc<RwLock<Difficulty>>,
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
                upstream: upstream.to_string(),
                enonce1: subscribe_result.enonce1,
                enonce2_size: subscribe_result.enonce2_size,
                connected: Arc::new(AtomicBool::new(false)),
                upstream_difficulty: Arc::new(RwLock::new(Difficulty::from(1))),
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
                    *self.upstream_difficulty.write().await = diff;
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
        let upstream_difficulty = self.upstream_difficulty.clone();

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

        let upstream_diff = *self.upstream_difficulty.read().await;
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

        // Fire and forget - spawn task to submit and log result
        // TODO: this should track rejected and also notify downstream miners if smth wrong
        tokio::spawn(async move {
            match client
                .submit_async(job_id, share.enonce2, ntime, nonce, version_bits)
                .await
            {
                Ok(handle) => match handle.wait().await {
                    Ok(SubmitOutcome::Accepted) => info!("Upstream accepted share"),
                    Ok(SubmitOutcome::Rejected { reason }) => {
                        let reason_str = reason.as_deref().unwrap_or("unknown");
                        warn!("Upstream rejected share: {}", reason_str);
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
        *self.upstream_difficulty.read().await
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
}
