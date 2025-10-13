use super::*;

mod miner;
mod ping;
pub(crate) mod pool;
pub mod server;
pub mod sync;
pub mod template;

#[derive(Debug, Parser)]
pub(crate) enum Subcommand {
    #[command(about = "Run a toy miner")]
    Miner(miner::Miner),
    #[command(about = "Measure Stratum message ping")]
    Ping(ping::Ping),
    #[command(about = "Run a toy solo pool")]
    Pool(pool::Pool),
    #[command(about = "Run API server")]
    Server(server::Server),
    #[command(about = "Sync shares via HTTP")]
    Sync(sync::Sync),
    #[command(about = "Monitor block templates")]
    Template(template::Template),
}

impl Subcommand {
    pub(crate) fn run(self) -> Result {
        match self {
            Self::Miner(miner) => miner.run(),
            Self::Ping(ping) => Runtime::new()?.block_on(async { ping.run().await }),
            Self::Pool(pool) => Runtime::new()?.block_on(async { pool.run().await }),
            Self::Server(server) => {
                let handle = Handle::new();
                let rt = Runtime::new()?;

                let mut sync_task = None;

                if let Some(sync_endpoint) = server.config.sync_endpoint() {
                    let hostname = System::host_name().ok_or(anyhow!("no hostname found"))?;

                    if !sync_endpoint.to_string().contains(&hostname) {
                        let mut sync = sync::Sync::default().with_endpoint(sync_endpoint.to_string());

                        if let Some(token) = server.config.admin_token() {
                            sync = sync.with_admin_token(token);
                        }

                        let send_task = rt.spawn_blocking(move || {
                            let sync_rt =
                                Runtime::new().expect("Failed to create sync send runtime");
                            sync_rt.block_on(async {
                                if let Err(e) = sync.run().await {
                                    error!("SyncSend failed: {}", e);
                                }
                            });
                        });
                        sync_task = Some(send_task);
                        info!("Started sync to endpoint: {sync_endpoint}");
                    }
                }

                let shutdown_handle = handle.clone();
                rt.spawn(async move {
                    let _ = ctrl_c().await;
                    info!("Received shutdown signal, stopping server...");
                    shutdown_handle.shutdown();
                });

                let server_result = rt.block_on(async { server.run(handle).await });

                if let Some(task) = sync_task {
                    task.abort();
                    let _ = rt.block_on(task);
                }

                server_result
            }
            Self::Sync(sync) => Ok(Runtime::new()?.block_on(async { sync.run().await })?),
            Self::Template(template) => Runtime::new()?.block_on(async { template.run().await }),
        }
    }
}
