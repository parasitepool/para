use {
    super::*,
    crate::{
        api,
        event_sink::build_event_sink,
        http_server,
        router::{OrderKind, Router},
    },
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
        let tasks = TaskTracker::new();

        let settings = Arc::new(
            Settings::from_proxy_options(self.options.clone())
                .context("failed to create settings")?,
        );

        let bitcoin_client = Arc::new(settings.bitcoin_rpc_client().await?);

        let address = settings.address();
        let port = settings.port();
        let listener = TcpListener::bind((address, port))
            .await
            .with_context(|| format!("failed to bind to {address}:{port}"))?;

        info!("Stratum proxy listening for downstream miners on {address}:{port}");

        let upstream_target = settings
            .upstream_targets()
            .first()
            .context("no upstream target configured")?
            .clone();

        let metatron = Arc::new(Metatron::new());
        metatron.spawn(cancel_token.clone(), &tasks);

        let router = Arc::new(Router::new(
            settings.clone(),
            metatron.clone(),
            None,
            tasks.clone(),
            cancel_token.clone(),
        ));

        let event_tx = build_event_sink(&settings, cancel_token.clone(), &tasks)
            .await
            .context("failed to build record sink")?;

        router
            .add_order(upstream_target, OrderKind::Sink, settings.hash_price())
            .await?;

        http_server::spawn(
            &settings,
            api::proxy::router(router.clone(), bitcoin_client, settings.chain(), logs),
            cancel_token.clone(),
            &tasks,
        )?;

        if !integration_test() && !logs_enabled() {
            spawn_throbber(router.clone(), cancel_token.clone(), &tasks);
        }

        router.serve(listener, event_tx, cancel_token).await
    }
}
