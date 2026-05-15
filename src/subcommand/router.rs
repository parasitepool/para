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
        let store = Arc::new(Store::open(
            &settings.store_path("router.redb")?,
            settings.chain(),
        )?);

        let wallet = Arc::new(Wallet::open(settings.clone(), store.clone())?);

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
            store,
            metatron.clone(),
            Some(wallet),
            tasks.clone(),
            cancel_token.clone(),
        ));

        router.restore(settings.sink_orders())?;

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
