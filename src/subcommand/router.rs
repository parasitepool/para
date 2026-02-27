use {
    super::*,
    crate::{api, http_server},
};

#[derive(Parser, Debug)]
pub(crate) struct RouterCli {
    #[command(flatten)]
    pub(crate) options: RouterOptions,
}

impl RouterCli {
    pub(crate) async fn run(
        &self,
        cancel_token: CancellationToken,
        logs: Arc<logs::Logs>,
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

        let router = Router::connect(
            settings.upstream_targets(),
            timeout,
            enonce1_extension_size,
            &endpoint,
            &cancel_token,
            &mut tasks,
        )
        .await?;

        router.spawn(cancel_token.clone(), &mut tasks);

        http_server::spawn(
            &settings,
            api::router::router(router.clone(), bitcoin_client, settings.chain(), logs),
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

            let settings = settings.clone();
            let cancel_token = slot.cancel_token.child_token();

            debug!("Spawning stratifier task for {addr}");

            tasks.spawn(async move {
                let mut stratifier: Stratifier<Notify> = Stratifier::new(
                    addr,
                    settings,
                    slot.metatron.clone(),
                    Some(slot.upstream.clone()),
                    stream,
                    slot.upstream.workbase_rx(),
                    cancel_token,
                    None,
                    start_diff,
                );

                if let Err(err) = stratifier.serve().await {
                    error!("Stratifier error for {addr}: {err}");
                }
            });
        }
    }
}
