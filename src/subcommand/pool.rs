use {super::*, pool_config::PoolConfig, settings::Settings};

pub(crate) mod pool_config;

#[derive(Parser, Debug)]
pub(crate) struct Pool {
    #[command(flatten)]
    pub(crate) config: PoolConfig,
}

impl Pool {
    pub(crate) async fn run(self, settings: Settings, cancel_token: CancellationToken) -> Result {
        let settings = Arc::new(settings);
        let config = Arc::new(self.config);
        let metatron = Arc::new(Metatron::new());
        let (share_tx, share_rx) = mpsc::channel(SHARE_CHANNEL_CAPACITY);
        let address = config.address(&settings);
        let port = config.port(&settings);

        let mut generator = Generator::new(settings.clone(), config.clone())?;
        let workbase_receiver = generator.spawn().await?;

        let listener = TcpListener::bind((address.clone(), port)).await?;

        eprintln!("Listening on {address}:{port}");

        let metatron_handle = {
            let metatron = metatron.clone();
            let cancel = cancel_token.clone();
            tokio::spawn(async move {
                metatron.run(share_rx, None, cancel).await;
            })
        };

        if !integration_test() && !logs_enabled() {
            spawn_throbber(metatron.clone());
        }

        let mut connection_tasks = JoinSet::new();

        loop {
            tokio::select! {
                Ok((stream, worker)) = listener.accept() => {
                    stream.set_nodelay(true)?;

                    info!("Accepted connection from {worker}");

                    let (reader, writer) = stream.into_split();

                    let workbase_receiver = workbase_receiver.clone();
                    let settings = settings.clone();
                    let config = config.clone();
                    let metatron = metatron.clone();
                    let share_tx = share_tx.clone();
                    let conn_cancel_token = cancel_token.child_token();

                    connection_tasks.spawn(async move {
                        let mut conn = Connection::new(
                            settings,
                            config,
                            metatron,
                            share_tx,
                            worker,
                            reader,
                            writer,
                            workbase_receiver,
                            conn_cancel_token,
                        );

                        if let Err(err) = conn.serve().await {
                            error!("Worker connection error: {err}")
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
            connection_tasks.len()
        );
        while connection_tasks.join_next().await.is_some() {}
        info!("All connections closed");

        drop(share_tx);
        let _ = metatron_handle.await;
        info!("Metatron stopped");

        Ok(())
    }
}

// Tests are in pool_config.rs
