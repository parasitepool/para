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

        wallet.spawn(settings.tick_interval(), cancel_token.clone(), &tasks);

        let address = settings.address();
        let port = settings.port();
        let listener = TcpListener::bind((address, port))
            .await
            .with_context(|| format!("failed to bind to {address}:{port}"))?;

        let metatron = Arc::new(Metatron::new());
        metatron.spawn(cancel_token.clone(), &tasks);

        let router = Arc::new(Router::new(
            settings.clone(),
            metatron.clone(),
            Some(wallet),
            tasks.clone(),
            cancel_token.clone(),
        ));

        for upstream_target in settings.sink_orders() {
            router
                .add_order(
                    upstream_target.clone(),
                    OrderKind::Sink,
                    settings.hash_price(),
                )
                .await?;
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

        router.serve(listener, None, cancel_token).await
    }
}
