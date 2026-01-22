use crate::record_sink::build_record_sink;
use {
    super::*,
    crate::{api, http_server},
};

#[derive(Parser, Debug)]
pub(crate) struct Pool {
    #[command(flatten)]
    pub(crate) options: PoolOptions,
}

impl Pool {
    pub(crate) async fn run(&self, cancel_token: CancellationToken) -> Result {
        let mut tasks = JoinSet::new();

        let settings = Arc::new(
            Settings::from_pool_options(self.options.clone())
                .context("failed to create settings")?,
        );

        let workbase_rx = spawn_generator(settings.clone(), cancel_token.clone(), &mut tasks)
            .await
            .context("failed to subscribe to ZMQ block notifications")?;

        let extranonces = Extranonces::Pool(
            PoolExtranonces::new(settings.enonce1_size(), settings.enonce2_size())
                .context("invalid extranonce configuration")?,
        );

        let metatron = Arc::new(Metatron::new(extranonces));
        metatron.clone().spawn(cancel_token.clone(), &mut tasks);

        http_server::spawn(
            &settings,
            api::pool::router(metatron.clone()),
            cancel_token.clone(),
            &mut tasks,
        )?;

        let event_tx = build_record_sink(&settings, cancel_token.clone(), &mut tasks)
            .await
            .context("failed to build record sink")?;

        let address = settings.address();
        let port = settings.port();

        let listener = TcpListener::bind((address, port))
            .await
            .with_context(|| format!("failed to bind to {address}:{port}"))?;

        info!("Stratum server listening on {address}:{port}");

        if !integration_test() && !logs_enabled() {
            spawn_throbber(metatron.clone(), cancel_token.clone(), &mut tasks);
        }

        loop {
            tokio::select! {
                Ok((stream, addr)) = listener.accept() => {
                    info!("Spawning stratifier task for {addr}");

                    let workbase_rx = workbase_rx.clone();
                    let settings = settings.clone();
                    let metatron = metatron.clone();
                    let conn_cancel_token = cancel_token.child_token();
                    let event_tx = event_tx.clone();

                    tasks.spawn(async move {
                        let mut stratifier: Stratifier<BlockTemplate> = Stratifier::new(
                            addr,
                            settings.clone(),
                            metatron,
                            None,
                            stream,
                            workbase_rx,
                            conn_cancel_token,
                            event_tx,
                        );

                        if let Err(err) = stratifier.serve().await {
                            error!("Stratifier error: {err}")
                        }
                    });
                }
                _ = cancel_token.cancelled() => {
                    info!("Shutting down stratum server");
                    break;
                }
            }
        }

        info!("Waiting for {} tasks to complete...", tasks.len());
        while tasks.join_next().await.is_some() {}
        info!("All pool tasks stopped");

        Ok(())
    }
}
