use super::*;

mod miner;
pub(crate) mod server;
mod sync;

#[derive(Debug, Parser)]
pub(crate) enum Subcommand {
    #[command(about = "Run a toy miner")]
    Miner(miner::Miner),
    #[command(about = "Run API server")]
    Server(server::Server),
    #[command(about = "Send shares to ZMQ endpoint")]
    SyncSend(sync::SyncSend),
    #[command(about = "Receive and process shares from ZMQ endpoint")]
    SyncReceive(sync::SyncReceive),
}

impl Subcommand {
    pub(crate) fn run(self) -> Result {
        match self {
            Self::Miner(miner) => miner.run(),
            Self::Server(server) => {
                let handle = Handle::new();
                let rt = Runtime::new()?;

                let mut sync_tasks = Vec::new();

                if !server.config.nodes().is_empty() {
                    let sync_receive = sync::SyncReceive::default();
                    let sync_handle = handle.clone();

                    let receive_task = rt.spawn_blocking(move || {
                        let sync_rt =
                            Runtime::new().expect("Failed to create sync receive runtime");
                        sync_rt.block_on(async {
                            if let Err(e) = sync_receive.with_zmq_endpoint("tcp://0.0.0.0:5555".into_string()).run(sync_handle).await {
                                error!("SyncReceive failed: {}", e);
                            }
                        });
                    });
                    sync_tasks.push(receive_task);
                    info!("Started SyncReceive due to configured nodes");
                } else if let Some(zmq_endpoint) = server.config.zmq_endpoint() {
                    let sync_send = sync::SyncSend::default().with_zmq_endpoint(zmq_endpoint);
                    let sync_handle = handle.clone();

                    let send_task = rt.spawn_blocking(move || {
                        let sync_rt = Runtime::new().expect("Failed to create sync send runtime");
                        sync_rt.block_on(async {
                            if let Err(e) = sync_send.run(sync_handle).await {
                                error!("SyncSend failed: {}", e);
                            }
                        });
                    });
                    sync_tasks.push(send_task);
                    info!(
                        "Started SyncSend to endpoint: {}",
                        server.config.zmq_endpoint().unwrap()
                    );
                }

                if sync_tasks.is_empty() {
                    warn!(
                        "No sync operations configured. Use --nodes for SyncReceive or --zmq-endpoint for SyncSend"
                    );
                }

                // Set up shutdown coordination
                let shutdown_handle = handle.clone();
                rt.spawn(async move {
                    let _ = tokio::signal::ctrl_c().await;
                    println!("Received shutdown signal, stopping server...");
                    shutdown_handle.shutdown();
                });

                let server_result = rt.block_on(async { server.run(handle).await });

                // Abort sync tasks and wait for them to finish
                for sync_task in sync_tasks {
                    sync_task.abort();
                    let _ = rt.block_on(sync_task);
                }

                server_result
            }
            Self::SyncSend(sync_send) => {
                let handle = Handle::new();

                Runtime::new()?.block_on(async { sync_send.run(handle).await.unwrap() });

                Ok(())
            }
            Self::SyncReceive(sync_receive) => {
                let handle = Handle::new();

                Runtime::new()?.block_on(async { sync_receive.run(handle).await.unwrap() });

                Ok(())
            }
        }
    }
}
