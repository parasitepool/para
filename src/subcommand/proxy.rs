use crate::record_sink::build_record_sink;
use {
    super::*,
    crate::{api, http_server},
};

#[derive(Parser, Debug)]
pub(crate) struct Proxy {
    #[command(flatten)]
    pub(crate) options: ProxyOptions,
}

impl Proxy {
    pub(crate) async fn run(&self, cancel_token: CancellationToken) -> Result {
        let mut tasks = JoinSet::new();

        let settings = Arc::new(
            Settings::from_proxy_options(self.options.clone())
                .context("failed to create settings")?,
        );

        let (upstream, events) = Upstream::connect(settings.clone()).await?;

        let upstream = Arc::new(upstream);

        let workbase_rx = upstream
            .clone()
            .spawn(events, cancel_token.clone(), &mut tasks)
            .await
            .context("failed to start upstream event loop")?;

        let extranonces = Extranonces::Proxy(
            ProxyExtranonces::new(upstream.enonce1().clone(), upstream.enonce2_size())
                .context("upstream extranonce configuration incompatible with proxy mode")?,
        );
        let metatron = Arc::new(Metatron::new(extranonces));
        metatron.clone().spawn(cancel_token.clone(), &mut tasks);

        let metrics = Arc::new(Metrics {
            upstream: upstream.clone(),
            metatron: metatron.clone(),
        });

        http_server::spawn(
            &settings,
            api::proxy::router(metrics.clone()),
            cancel_token.clone(),
            &mut tasks,
        )?;

        let event_tx = if let Some((tx, handle, sink_cancel)) =
            build_record_sink(&settings)
                .await
                .context("failed to build record sink")?
        {
            tasks.spawn(async move {
                let _ = handle.await;
            });
            // Store sink cancellation token to cancel when main cancel_token is triggered
            tasks.spawn({
                let cancel_token = cancel_token.clone();
                async move {
                    cancel_token.cancelled().await;
                    sink_cancel.cancel();
                }
            });
            Some(tx)
        } else {
            None
        };

        let address = settings.address();
        let port = settings.port();
        let listener = TcpListener::bind((address, port))
            .await
            .with_context(|| format!("failed to bind to {address}:{port}"))?;

        info!("Stratum server listening for downstream miners on {address}:{port}");

        if !integration_test() && !logs_enabled() {
            spawn_throbber(metrics, cancel_token.clone(), &mut tasks);
        }

        loop {
            tokio::select! {
                Ok((stream, addr)) = listener.accept() => {
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
                        );

                        if let Err(err) = stratifier.serve().await {
                            error!("Stratifier error for {addr}: {err}");
                        }
                    });
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
            }
        }

        info!("Waiting for {} tasks to complete...", tasks.len());
        while tasks.join_next().await.is_some() {}
        info!("All proxy tasks stopped");

        Ok(())
    }
}
