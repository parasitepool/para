use {
    super::*,
    crate::{api, http_server},
};

#[derive(Parser, Debug)]
pub(crate) struct RouterCommand {
    #[command(flatten)]
    pub(crate) options: RouterOptions,
}

impl RouterCommand {
    pub(crate) async fn run(
        &self,
        cancel_token: CancellationToken,
        logs: Arc<logs::Logs>,
    ) -> Result {
        let tasks = TaskTracker::new();

        let settings = Arc::new(
            Settings::from_router_options(self.options.clone())
                .context("failed to create settings")?,
        );

        let bitcoin_client = Arc::new(settings.bitcoin_rpc_client().await?);

        let rpc_url = format!("http://{}", settings.bitcoin_rpc_url());

        let wallet = Arc::new(Wallet::new(
            settings.descriptor().context("--descriptor is required")?,
            settings.change_descriptor(),
            settings.chain().network(),
            &rpc_url,
            settings.wallet_rpc_auth()?,
            settings.wallet_birthday(),
        )?);

        info!("Syncing wallet...");

        wallet.sync().context("initial wallet sync failed")?;
        wallet.spawn(settings.tick_interval(), cancel_token.clone(), &tasks);

        let address = settings.address();
        let port = settings.port();
        let listener = TcpListener::bind((address, port))
            .await
            .with_context(|| format!("failed to bind to {address}:{port}"))?;

        let metatron = Arc::new(Metatron::new());
        metatron.spawn(cancel_token.clone(), &tasks);

        let router = Arc::new(Router::new(
            metatron.clone(),
            settings.clone(),
            tasks.clone(),
            cancel_token.clone(),
            wallet,
        ));

        for target in settings.default_orders() {
            router.add_order(target.clone(), None);
        }

        router.spawn_rebalance_loop();

        http_server::spawn(
            &settings,
            api::router::router(router.clone(), bitcoin_client, settings.chain(), logs),
            cancel_token.clone(),
            &tasks,
        )?;

        if !integration_test() && !logs_enabled() {
            spawn_throbber(router.clone(), cancel_token.clone(), &tasks);
        }

        info!("Stratum router listening for downstream miners on {address}:{port}");

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
                    tasks.close();
                    tasks.wait().await;
                    info!("All router tasks stopped");
                    return Ok(());
                }
            };

            let Some(order) = router.next_order() else {
                warn!("No order to match with available, dropping connection from {addr}");
                continue;
            };

            let order_kind = if order.is_default() {
                "default"
            } else {
                "paid"
            };

            info!(
                "Routing {addr} to {order_kind} order {} at {}",
                order.id, order.target,
            );

            let settings = settings.clone();
            let disconnect_token = order.register_session();
            let metatron = metatron.clone();
            let start_diff = settings.start_diff();

            debug!("Spawning stratifier task for {addr}");

            tasks.spawn(async move {
                let upstream = order.upstream().expect("active order").clone();
                let allocator = order.allocator().expect("active order").clone();
                let mut stratifier: Stratifier<Notify> = Stratifier::new(
                    addr,
                    settings,
                    allocator,
                    metatron,
                    Some(upstream.clone()),
                    stream,
                    upstream.workbase_rx(),
                    disconnect_token,
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
