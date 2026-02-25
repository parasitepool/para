use {super::*, crate::event_sink::build_event_sink, tokio::sync::RwLock as TokioRwLock};

#[derive(Parser, Debug)]
pub(crate) struct Router {
    #[command(flatten)]
    pub(crate) options: RouterOptions,
}

struct UpstreamSlot {
    target: UpstreamTarget,
    metatron: Arc<Metatron>,
    state: TokioRwLock<Option<ActiveUpstream>>,
}

struct ActiveUpstream {
    upstream: Arc<Upstream>,
    workbase_rx: watch::Receiver<Arc<Notify>>,
}

impl Router {
    pub(crate) async fn run(
        &self,
        cancel_token: CancellationToken,
        _logs: Arc<logs::Logs>,
    ) -> Result {
        let mut tasks = JoinSet::new();

        let settings = Arc::new(
            Settings::from_router_options(self.options.clone())
                .context("failed to create settings")?,
        );

        let address = settings.address();
        let port = settings.port();
        let listener = TcpListener::bind((address, port))
            .await
            .with_context(|| format!("failed to bind to {address}:{port}"))?;

        info!("Router listening for downstream miners on {address}:{port}");

        let high_diff_listener = if let Some(high_diff_port) = settings.high_diff_port() {
            let listener = TcpListener::bind((address, high_diff_port))
                .await
                .with_context(|| {
                    format!("failed to bind high diff listener to {address}:{high_diff_port}")
                })?;

            info!("High diff stratum server listening on {address}:{high_diff_port}");

            Some(listener)
        } else {
            None
        };

        let timeout = settings.timeout();
        let enonce1_extension_size = settings.enonce1_extension_size();
        let endpoint = format!("{}:{}", settings.address(), settings.port());

        let slots: Vec<Arc<UpstreamSlot>> = self
            .options
            .upstream
            .iter()
            .map(|target| {
                let extranonces =
                    Extranonces::Pool(PoolExtranonces::new(ENONCE1_SIZE, MAX_ENONCE_SIZE).unwrap());
                let metatron = Arc::new(Metatron::new(extranonces, endpoint.clone()));
                metatron.clone().spawn(cancel_token.clone(), &mut tasks);
                Arc::new(UpstreamSlot {
                    target: target.clone(),
                    metatron,
                    state: TokioRwLock::new(None),
                })
            })
            .collect();

        let slot_connected = Arc::new(tokio::sync::Notify::new());

        for slot in &slots {
            let slot = slot.clone();
            let cancel_token = cancel_token.clone();
            let slot_connected = slot_connected.clone();

            tasks.spawn(async move {
                slot_connect_loop(
                    slot,
                    enonce1_extension_size,
                    timeout,
                    cancel_token,
                    slot_connected,
                )
                .await;
            });
        }

        let event_tx = build_event_sink(&settings, cancel_token.clone(), &mut tasks)
            .await
            .context("failed to build event sink")?;

        let counter = AtomicU64::new(0);

        loop {
            let (stream, addr, start_diff) = tokio::select! {
                accept = listener.accept() => {
                    let (stream, addr) = match accept {
                        Ok((stream, addr)) => (stream, addr),
                        Err(err) => {
                            error!("Accept error: {err}");
                            continue;
                        }
                    };
                    (stream, addr, settings.start_diff())
                }
                Some(accept) = async {
                    match &high_diff_listener {
                        Some(listener) => Some(listener.accept().await),
                        None => None,
                    }
                } => {
                    let (stream, addr) = match accept {
                        Ok((stream, addr)) => (stream, addr),
                        Err(err) => {
                            error!("High diff accept error: {err}");
                            continue;
                        }
                    };
                    (stream, addr, settings.high_diff_start())
                }
                _ = cancel_token.cancelled() => {
                    info!("Shutting down router");
                    while tasks.join_next().await.is_some() {}
                    info!("All router tasks stopped");
                    return Ok(());
                }
            };

            let idx = counter.fetch_add(1, Ordering::Relaxed) as usize;

            let assigned = assign_slot(&slots, idx).await;

            let Some((metatron, upstream, workbase_rx)) = assigned else {
                info!("All upstreams disconnected, waiting for reconnect...");
                tokio::select! {
                    _ = slot_connected.notified() => {}
                    _ = cancel_token.cancelled() => {
                        info!("Shutting down router");
                        while tasks.join_next().await.is_some() {}
                        return Ok(());
                    }
                }

                let Some((metatron, upstream, workbase_rx)) = assign_slot(&slots, idx).await else {
                    warn!("No upstream available after notify, dropping connection from {addr}");
                    continue;
                };

                spawn_stratifier(
                    addr,
                    &settings,
                    metatron,
                    upstream,
                    stream,
                    workbase_rx,
                    &cancel_token,
                    &event_tx,
                    start_diff,
                    &mut tasks,
                );
                continue;
            };

            spawn_stratifier(
                addr,
                &settings,
                metatron,
                upstream,
                stream,
                workbase_rx,
                &cancel_token,
                &event_tx,
                start_diff,
                &mut tasks,
            );
        }
    }
}

async fn assign_slot(
    slots: &[Arc<UpstreamSlot>],
    idx: usize,
) -> Option<(Arc<Metatron>, Arc<Upstream>, watch::Receiver<Arc<Notify>>)> {
    for i in 0..slots.len() {
        let slot = &slots[(idx + i) % slots.len()];
        let state = slot.state.read().await;
        if let Some(active) = &*state {
            return Some((
                slot.metatron.clone(),
                active.upstream.clone(),
                active.workbase_rx.clone(),
            ));
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn spawn_stratifier(
    addr: SocketAddr,
    settings: &Arc<Settings>,
    metatron: Arc<Metatron>,
    upstream: Arc<Upstream>,
    stream: TcpStream,
    workbase_rx: watch::Receiver<Arc<Notify>>,
    cancel_token: &CancellationToken,
    event_tx: &Option<mpsc::Sender<crate::event_sink::Event>>,
    start_diff: Difficulty,
    tasks: &mut JoinSet<()>,
) {
    debug!("Spawning stratifier task for {addr}");

    let settings = settings.clone();
    let conn_cancel_token = cancel_token.child_token();
    let event_tx = event_tx.clone();

    tasks.spawn(async move {
        let mut stratifier: Stratifier<Notify> = Stratifier::new(
            addr,
            settings,
            metatron,
            Some(upstream),
            stream,
            workbase_rx,
            conn_cancel_token,
            event_tx,
            start_diff,
        );

        if let Err(err) = stratifier.serve().await {
            error!("Stratifier error for {addr}: {err}");
        }
    });
}

async fn slot_connect_loop(
    slot: Arc<UpstreamSlot>,
    enonce1_extension_size: usize,
    timeout: Duration,
    cancel_token: CancellationToken,
    slot_connected: Arc<tokio::sync::Notify>,
) {
    let mut backoff = Duration::from_secs(1);

    loop {
        let result = Upstream::connect(
            &slot.target.endpoint,
            slot.target.username.clone(),
            slot.target.password.clone(),
            timeout,
        )
        .await;

        match result {
            Ok((upstream, events)) => {
                let upstream = Arc::new(upstream);
                let mut slot_tasks = JoinSet::new();

                match upstream
                    .clone()
                    .spawn(events, cancel_token.clone(), &mut slot_tasks)
                    .await
                {
                    Ok(workbase_rx) => {
                        backoff = Duration::from_secs(1);

                        let extranonces = Extranonces::Proxy(
                            match ProxyExtranonces::new(
                                upstream.enonce1().clone(),
                                upstream.enonce2_size(),
                                enonce1_extension_size,
                            ) {
                                Ok(e) => e,
                                Err(e) => {
                                    error!(
                                        "Extranonce config incompatible for {}: {e}",
                                        slot.target
                                    );
                                    continue;
                                }
                            },
                        );

                        slot.metatron.update_extranonces(extranonces);

                        {
                            let mut state = slot.state.write().await;
                            *state = Some(ActiveUpstream {
                                upstream: upstream.clone(),
                                workbase_rx,
                            });
                        }

                        info!("Upstream {} connected", slot.target);
                        slot_connected.notify_waiters();

                        upstream.disconnected().await;

                        warn!("Upstream {} disconnected", slot.target);

                        {
                            let mut state = slot.state.write().await;
                            *state = None;
                        }

                        while slot_tasks.join_next().await.is_some() {}
                        continue;
                    }
                    Err(e) => {
                        warn!(
                            "Failed to start upstream event loop for {}: {e}",
                            slot.target
                        );
                    }
                }
            }
            Err(e) => {
                warn!("Failed to connect to upstream {}: {e}", slot.target);
            }
        }

        warn!("Retrying {} in {}s...", slot.target, backoff.as_secs());

        tokio::select! {
            _ = sleep(backoff) => {}
            _ = cancel_token.cancelled() => return
        }

        backoff = (backoff * 2).min(Duration::from_secs(60));
    }
}
