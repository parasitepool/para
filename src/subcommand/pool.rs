use {
    super::*,
    crate::{
        api, http_server,
        settings::{PoolOptions, Settings},
    },
};

#[derive(Parser, Debug)]
pub(crate) struct Pool {
    #[command(flatten)]
    pub(crate) options: PoolOptions,
}

impl Pool {
    pub(crate) async fn run(&self, cancel_token: CancellationToken) -> Result {
        let settings = Arc::new(
            Settings::from_pool_options(self.options.clone())
                .context("failed to create settings")?,
        );

        let mut generator =
            Generator::new(settings.clone()).context("failed to connect to Bitcoin Core RPC")?;

        let workbase_rx = generator
            .spawn()
            .await
            .context("failed to subscribe to ZMQ block notifications")?;

        let address = settings.address();
        let port = settings.port();

        let listener = TcpListener::bind((address, port))
            .await
            .with_context(|| format!("failed to bind to {address}:{port}"))?;

        info!("Listening on {address}:{port}");

        let metatron = Arc::new(Metatron::new());
        let (share_tx, share_rx) = mpsc::channel(SHARE_CHANNEL_CAPACITY);
        let metatron_handle = {
            let metatron = metatron.clone();
            let cancel = cancel_token.clone();
            tokio::spawn(async move {
                metatron.run(share_rx, None, cancel).await;
            })
        };

        let api_handle = if let Some(api_port) = settings.api_port() {
            let http_config = http_server::HttpConfig {
                address: settings.address().to_string(),
                port: api_port,
                acme_domains: settings.acme_domains().to_vec(),
                acme_contacts: settings.acme_contacts().to_vec(),
                acme_cache: settings.acme_cache_path(),
            };

            Some(http_server::spawn(
                http_config,
                api::pool::router(metatron.clone()),
                cancel_token.clone(),
            )?)
        } else {
            None
        };

        if !integration_test() && !logs_enabled() {
            spawn_throbber(metatron.clone());
        }

        let mut stratifier_tasks = JoinSet::new();

        loop {
            tokio::select! {
                Ok((stream, worker)) = listener.accept() => {
                    info!("Accepted connection from {worker}");

                    let workbase_rx = workbase_rx.clone();
                    let settings = settings.clone();
                    let metatron = metatron.clone();
                    let share_tx = share_tx.clone();
                    let conn_cancel_token = cancel_token.child_token();

                    stratifier_tasks.spawn(async move {
                        let mut stratifier: Stratifier<BlockTemplate> = Stratifier::new(
                            settings,
                            metatron,
                            share_tx,
                            worker,
                            stream,
                            workbase_rx,
                            conn_cancel_token,
                        );

                        if let Err(err) = stratifier.serve().await {
                            error!("Stratifier error: {err}")
                        }
                    });
                }
                _ = cancel_token.cancelled() => {
                        info!("Shutting down stratum server");
                        generator.shutdown().await;
                        break;
                    }
            }
        }

        info!(
            "Waiting for {} active connections to close...",
            stratifier_tasks.len()
        );

        while stratifier_tasks.join_next().await.is_some() {}

        info!("All connections closed");

        drop(share_tx);

        let _ = metatron_handle.await;
        info!("Metatron stopped");

        if let Some(handle) = api_handle {
            let _ = handle.await;
            info!("HTTP API server stopped");
        }

        Ok(())
    }
}
