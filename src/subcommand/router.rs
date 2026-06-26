use {
    super::*,
    crate::{api, generator::get_block_template, http_server},
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
        let template = get_block_template(&bitcoin_client, &settings).await?;
        let initial_hash_value = HashValue::compute(template.coinbase_value, template.bits);

        let store = Arc::new(Store::open(
            &settings.store_path("router.redb")?,
            settings.chain(),
        )?);

        let metatron = Arc::new(Metatron::open(store)?);

        let wallet = Arc::new(Wallet::open(settings.clone(), metatron.store().clone())?);

        wallet.spawn(settings.tick_interval(), cancel_token.clone(), &tasks);

        let address = settings.address();
        let port = settings.port();
        let listener = TcpListener::bind((address, port))
            .await
            .with_context(|| format!("failed to bind to {address}:{port}"))?;

        metatron.spawn(cancel_token.clone(), &tasks);

        let router = Arc::new(Router::new(
            settings.clone(),
            metatron.clone(),
            Some(wallet),
            tasks.clone(),
            cancel_token.clone(),
            initial_hash_value,
        ));

        router.restore(settings.sink_orders())?;

        http_server::spawn(
            &settings,
            api::router::router(
                router.clone(),
                bitcoin_client.clone(),
                settings.chain(),
                logs,
                settings.http_api_token(),
                settings.http_admin_token(),
            ),
            cancel_token.clone(),
            &tasks,
        )?;

        if !integration_test() && !logs_enabled() {
            spawn_throbber(router.clone(), cancel_token.clone(), &tasks);
        }

        info!("Stratum router listening for downstream miners on {address}:{port}");

        router
            .serve(listener, None, Some(bitcoin_client), cancel_token)
            .await
    }
}
