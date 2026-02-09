use {
    super::*,
    crate::{api, event_sink::build_event_sink, http_server},
};

#[derive(Parser, Debug)]
pub(crate) struct Proxy {
    #[command(flatten)]
    pub(crate) options: ProxyOptions,
}

impl Proxy {
    pub(crate) async fn run(
        &self,
        cancel_token: CancellationToken,
        logs: Arc<logs::Logs>,
    ) -> Result {
        let mut tasks = JoinSet::new();

        let settings = Arc::new(
            Settings::from_proxy_options(self.options.clone())
                .context("failed to create settings")?,
        );

        let bitcoin_client = Arc::new(settings.bitcoin_rpc_client().await?);

        let (upstream, events) = Upstream::connect(settings.clone()).await?;

        let upstream = Arc::new(upstream);

        let workbase_rx = upstream
            .clone()
            .spawn(events, cancel_token.clone(), &mut tasks)
            .await
            .context("failed to start upstream event loop")?;

        let extranonces = Extranonces::Proxy(
            ProxyExtranonces::new(
                upstream.enonce1().clone(),
                upstream.enonce2_size(),
                settings.enonce1_extension_size(),
            )
            .context("upstream extranonce configuration incompatible with proxy mode")?,
        );
        let metatron = Arc::new(Metatron::new(
            extranonces,
            format!("{}:{}", settings.address(), settings.port()),
        ));
        metatron.clone().spawn(cancel_token.clone(), &mut tasks);

        let metrics = Arc::new(Metrics {
            upstream: upstream.clone(),
            metatron: metatron.clone(),
        });

        http_server::spawn(
            &settings,
            api::proxy::router(metrics.clone(), bitcoin_client, settings.chain(), logs),
            cancel_token.clone(),
            &mut tasks,
        )?;

        let event_tx = build_event_sink(&settings, cancel_token.clone(), &mut tasks)
            .await
            .context("failed to build record sink")?;

        let address = settings.address();
        let port = settings.port();
        let listener = TcpListener::bind((address, port))
            .await
            .with_context(|| format!("failed to bind to {address}:{port}"))?;

        info!("Stratum server listening for downstream miners on {address}:{port}");

        let high_diff_listener = if let Some(high_diff_port) = settings.high_diff_port() {
            let listener = TcpListener::bind((address, high_diff_port))
                .await
                .with_context(|| {
                    format!("failed to bind high-diff listener to {address}:{high_diff_port}")
                })?;
            info!("High-diff stratum server listening on {address}:{high_diff_port}");
            Some(listener)
        } else {
            None
        };

        if !integration_test() && !logs_enabled() {
            spawn_throbber(metrics, cancel_token.clone(), &mut tasks);
        }

        loop {
            let (stream, addr, start_diff) = tokio::select! {
                Ok((stream, addr)) = listener.accept() => {
                    (stream, addr, settings.start_diff())
                }
                Ok((stream, addr)) = async {
                    match &high_diff_listener {
                        Some(l) => l.accept().await,
                        None => std::future::pending().await,
                    }
                } => {
                    (stream, addr, Difficulty::from(1_000_000))
                }
                _ = async {
                    while upstream.is_connected() {
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                } => {
                    warn!("Upstream connection lost, shutting down");
                    cancel_token.cancel();
                    break;
                }
                _ = cancel_token.cancelled() => {
                    info!("Shutting down proxy");
                    break;
                }
            };

            info!("Spawning stratifier task for {addr}");

            let workbase_rx = workbase_rx.clone();
            let settings = settings.clone();
            let metatron = metatron.clone();
            let upstream = upstream.clone();
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

        info!("Waiting for {} tasks to complete...", tasks.len());
        while tasks.join_next().await.is_some() {}
        info!("All proxy tasks stopped");

        Ok(())
    }
}
