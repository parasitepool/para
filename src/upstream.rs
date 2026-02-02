use {
    super::*,
    stratum::{Client, ClientError, Event, EventReceiver},
    tokio::sync::RwLock,
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
    client: Client,
    enonce1: Extranonce,
    enonce2_size: usize,
    connected: Arc<AtomicBool>,
    endpoint: String,
    difficulty: Arc<RwLock<Difficulty>>,
    accepted: Arc<AtomicU64>,
    rejected: Arc<AtomicU64>,
    filtered: Arc<AtomicU64>,
    version_mask: Option<Version>,
}

impl Upstream {
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

        let client = Client::new(
            upstream_addr.to_string(),
            username.clone(),
            settings.upstream_password(),
            USER_AGENT.into(),
            settings.timeout(),
        );

        let events = client
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
            Ok((response, _, _)) if response.version_rolling => {
                if let Some(mask) = response.version_rolling_mask {
                    info!("Upstream supports version rolling: mask={mask}",);
                    Some(mask)
                } else {
                    None
                }
            }
            Ok(_) => {
                info!("Upstream does not support version rolling");
                None
            }
            Err(e) => {
                warn!("Failed to negotiate version rolling with upstream: {e}");
                None
            }
        };

        let (subscribe, _, _) = client
            .subscribe()
            .await
            .context("failed to subscribe to upstream")?;

        info!(
            "Subscribed to upstream: enonce1={}, enonce2_size={}",
            subscribe.enonce1, subscribe.enonce2_size
        );

        Ok((
            Self {
                client,
                enonce1: subscribe.enonce1,
                enonce2_size: subscribe.enonce2_size,
                connected: Arc::new(AtomicBool::new(false)),
                endpoint: upstream.to_string(),
                difficulty: Arc::new(RwLock::new(Difficulty::from(1))),
                accepted: Arc::new(AtomicU64::new(0)),
                rejected: Arc::new(AtomicU64::new(0)),
                filtered: Arc::new(AtomicU64::new(0)),
                version_mask,
            },
            events,
        ))
    }

    pub(crate) async fn spawn(
        self: Arc<Self>,
        mut events: EventReceiver,
        cancel: CancellationToken,
        tasks: &mut JoinSet<()>,
    ) -> Result<watch::Receiver<Arc<Notify>>> {
        self.client
            .authorize()
            .await
            .context("failed to authorize with upstream")?;

        info!(
            "Authorized to upstream {} with {}",
            self.client.address(),
            self.client.username()
        );

        self.connected.store(true, Ordering::SeqCst);

        let mut initial_difficulty: Option<Difficulty> = None;
        let mut first_notify: Option<Notify> = None;

        loop {
            match events.recv().await {
                Ok(Event::SetDifficulty(diff)) => {
                    info!("Received initial difficulty: {}", diff);
                    *self.difficulty.write().await = diff;
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

        let connected = self.connected.clone();
        let upstream_difficulty = self.difficulty.clone();

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
                                *upstream_difficulty.write().await = diff;
                            }
                            Ok(Event::Disconnected) => {
                                warn!("Disconnected from upstream");
                                connected.store(false, Ordering::SeqCst);
                                break;
                            }
                            Err(err) => {
                                error!("Upstream event error: {}", err);
                                connected.store(false, Ordering::SeqCst);
                                break;
                            }
                        }
                    }
                }
            }
        });

        Ok(workbase_rx)
    }

    pub(crate) async fn submit_share(&self, submit: UpstreamSubmit) {
        let upstream_diff = *self.difficulty.read().await;
        if submit.share_diff < upstream_diff {
            debug!(
                "Share below upstream difficulty: share_diff={} < upstream_diff={}",
                submit.share_diff, upstream_diff
            );
            self.filtered.fetch_add(1, Ordering::Relaxed);
            return;
        }

        debug!(
            "Submitting share to upstream: job_id={}, share_diff={}, upstream_diff={}",
            submit.job_id, submit.share_diff, upstream_diff
        );

        let client = self.client.clone();
        let accepted = self.accepted.clone();
        let rejected = self.rejected.clone();

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
                Ok(_) => {
                    accepted.fetch_add(1, Ordering::Relaxed);
                    info!("Upstream accepted share");
                }
                Err(ClientError::SubmitFalse) => {
                    rejected.fetch_add(1, Ordering::Relaxed);
                    warn!("Upstream rejected share: submit=false");
                }
                Err(ClientError::Rejected { reason, .. }) => {
                    rejected.fetch_add(1, Ordering::Relaxed);
                    warn!("Upstream rejected share: {}", reason);
                }
                Err(e) => {
                    warn!("Upstream submit error: {e}");
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

    pub(crate) async fn difficulty(&self) -> Difficulty {
        *self.difficulty.read().await
    }

    pub(crate) fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
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

    pub(crate) fn filtered(&self) -> u64 {
        self.filtered.load(Ordering::Relaxed)
    }

    pub(crate) fn version_mask(&self) -> Option<Version> {
        self.version_mask
    }
}
