use {
    super::*,
    crate::{
        api, http_server,
        settings::{ProxyOptions, Settings},
    },
    stratum::Notify,
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

        let (nexus, events) = Nexus::connect(settings.clone()).await?;
        let nexus = Arc::new(nexus);

        let mode = Mode::Proxy {
            enonce1: nexus.enonce1().clone(),
            enonce2_size: nexus.enonce2_size(),
        };

        let (workbase_rx, sink_tx) = nexus
            .clone()
            .spawn(events, cancel_token.clone(), &mut tasks)
            .await
            .context("failed to start upstream event loop")?;

        let metatron = Arc::new(Metatron::new());
        let share_tx = metatron
            .clone()
            .spawn(Some(sink_tx), cancel_token.clone(), &mut tasks);

        let argus = Arc::new(Argus {
            nexus: nexus.clone(),
            metatron: metatron.clone(),
        });

        http_server::spawn(
            &settings,
            api::proxy::router(argus.clone()),
            cancel_token.clone(),
            &mut tasks,
        )?;

        let address = settings.address();
        let port = settings.port();
        let listener = TcpListener::bind((address, port))
            .await
            .with_context(|| format!("failed to bind to {address}:{port}"))?;

        info!("Stratum server listening for downstream miners on {address}:{port}");

        if !integration_test() && !logs_enabled() {
            spawn_throbber(argus, cancel_token.clone(), &mut tasks);
        }

        loop {
            tokio::select! {
                Ok((stream, addr)) = listener.accept() => {
                    info!("Spawning stratifier task for {addr}");

                    let workbase_rx = workbase_rx.clone();
                    let settings = settings.clone();
                    let metatron = metatron.clone();
                    let share_tx = share_tx.clone();
                    let mode = mode.clone();
                    let conn_cancel_token = cancel_token.child_token();

                    tasks.spawn(async move {
                        let mut stratifier: Stratifier<Notify> = Stratifier::new(
                            settings,
                            mode,
                            metatron,
                            share_tx,
                            addr,
                            stream,
                            workbase_rx,
                            conn_cancel_token,
                        );

                        if let Err(err) = stratifier.serve().await {
                            error!("Stratifier error for {addr}: {err}");
                        }
                    });
                }

                _ = async {
                    while nexus.is_connected() {
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
