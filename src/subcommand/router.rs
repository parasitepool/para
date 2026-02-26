use {
    super::*,
    crate::{api, http_server, router::Slot},
};

#[derive(Parser, Debug)]
pub(crate) struct Router {
    #[command(flatten)]
    pub(crate) options: RouterOptions,
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

        let bitcoin_client = Arc::new(settings.bitcoin_rpc_client().await?);

        let address = settings.address();
        let port = settings.port();
        let listener = TcpListener::bind((address, port))
            .await
            .with_context(|| format!("failed to bind to {address}:{port}"))?;

        info!("Stratum router listening for downstream miners on {address}:{port}");

        let timeout = settings.timeout();
        let enonce1_extension_size = settings.enonce1_extension_size();
        let endpoint = format!("{}:{}", settings.address(), settings.port());

        let mut slots = Vec::new();

        for target in &self.options.upstream {
            match connect_upstream(
                target,
                timeout,
                enonce1_extension_size,
                &endpoint,
                &cancel_token,
                &mut tasks,
            )
            .await
            {
                Ok(slot) => slots.push(slot),
                Err(err) => {
                    warn!("Skipping upstream {target}: {err}");
                }
            }
        }

        let router = Arc::new(crate::router::Router::new(slots));

        for slot in &router.slots() {
            let slot = slot.clone();
            let router = router.clone();
            tasks.spawn(async move {
                slot.upstream.disconnected().await;
                warn!(
                    "Upstream {} disconnected, removing slot",
                    slot.upstream.endpoint()
                );
                slot.cancel_token.cancel();
                router.remove_slot(&slot);
            });
        }

        http_server::spawn(
            &settings,
            api::router::router(router.clone(), bitcoin_client, settings.chain()),
            cancel_token.clone(),
            &mut tasks,
        )?;

        if !integration_test() && !logs_enabled() {
            spawn_throbber(router.clone(), cancel_token.clone(), &mut tasks);
        }

        let start_diff = settings.start_diff();

        loop {
            let (stream, addr) = tokio::select! {
                accept = listener.accept() => {
                    match accept {
                        Ok((stream, addr)) => (stream, addr),
                        Err(err) => {
                            error!("Accept error: {err}");
                            continue;
                        }
                    }
                }
                _ = cancel_token.cancelled() => {
                    info!("Shutting down router");
                    while tasks.join_next().await.is_some() {}
                    info!("All router tasks stopped");
                    return Ok(());
                }
            };

            let Some(slot) = router.assign_to_slot() else {
                warn!("No upstream available, dropping connection from {addr}");
                continue;
            };

            spawn_stratifier(addr, &settings, slot, stream, start_diff, &mut tasks);
        }
    }
}

async fn connect_upstream(
    target: &UpstreamTarget,
    timeout: Duration,
    enonce1_extension_size: usize,
    endpoint: &str,
    cancel_token: &CancellationToken,
    tasks: &mut JoinSet<()>,
) -> Result<Arc<Slot>> {
    let (upstream, events) = Upstream::connect(target.clone(), timeout).await?;
    let upstream = Arc::new(upstream);

    let slot_cancel = cancel_token.child_token();

    let workbase_rx = upstream
        .clone()
        .spawn(events, slot_cancel.clone(), tasks)
        .await?;

    let proxy_extranonces = ProxyExtranonces::new(
        upstream.enonce1().clone(),
        upstream.enonce2_size(),
        enonce1_extension_size,
    )?;

    let metatron = Arc::new(Metatron::new(
        Extranonces::Proxy(proxy_extranonces),
        endpoint.to_string(),
    ));

    metatron.clone().spawn(cancel_token.clone(), tasks);

    info!("Upstream {target} connected");

    Ok(Arc::new(Slot {
        upstream,
        metatron,
        workbase_rx,
        cancel_token: slot_cancel,
    }))
}

fn spawn_stratifier(
    addr: SocketAddr,
    settings: &Arc<Settings>,
    slot: Arc<Slot>,
    stream: TcpStream,
    start_diff: Difficulty,
    tasks: &mut JoinSet<()>,
) {
    debug!("Spawning stratifier task for {addr}");

    let settings = settings.clone();
    let conn_cancel_token = slot.cancel_token.child_token();

    tasks.spawn(async move {
        let mut stratifier: Stratifier<Notify> = Stratifier::new(
            addr,
            settings,
            slot.metatron.clone(),
            Some(slot.upstream.clone()),
            stream,
            slot.workbase_rx.clone(),
            conn_cancel_token,
            None,
            start_diff,
        );

        if let Err(err) = stratifier.serve().await {
            error!("Stratifier error for {addr}: {err}");
        }
    });
}
