use super::*;

pub mod miner;
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
    pub(crate) async fn run(self, cancel_token: CancellationToken) -> Result {
        match self {
            Self::Miner(miner) => miner.run(cancel_token).await,
            Self::Ping(ping) => ping.run(cancel_token).await,
            Self::Pool(pool) => pool.run(cancel_token).await,
            Self::Server(server) => {
                let handle = Handle::new();

                let mut sync_task = None;

                if let Some(sync_endpoint) = server.config.sync_endpoint() {
                    let hostname = System::host_name().ok_or(anyhow!("no hostname found"))?;

                    if !sync_endpoint.contains(&hostname) {
                        let mut sync = sync::Sync::default().with_endpoint(sync_endpoint.clone());

                        if let Some(token) = server.config.admin_token() {
                            sync = sync.with_admin_token(token);
                        }

                        let sync_cancel_token = cancel_token.clone();
                        let send_task = tokio::spawn(async move {
                            if let Err(e) = sync.run(sync_cancel_token).await {
                                error!("SyncSend failed: {}", e);
                            }
                        });
                        sync_task = Some(send_task);
                        info!("Started sync to endpoint: {sync_endpoint}");
                    }
                }

                let server_result = server.run(handle, cancel_token).await;

                if let Some(task) = sync_task {
                    task.abort();
                    let _ = task.await;
                }

                server_result
            }
            Self::Sync(sync) => sync.run(cancel_token).await,
            Self::Template(template) => template.run(cancel_token).await,
        }
    }
}
