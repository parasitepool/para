use {super::*, settings::Settings};

pub mod miner;
mod ping;
pub(crate) mod pool;
pub mod server;
mod settings_cmd;
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
    #[command(about = "Show resolved settings")]
    Settings(settings_cmd::SettingsCmd),
    #[command(about = "Sync shares via HTTP")]
    Sync(sync::Sync),
    #[command(about = "Monitor block templates")]
    Template(template::Template),
}

impl Subcommand {
    pub(crate) async fn run(self, settings: Settings, cancel_token: CancellationToken) -> Result {
        match self {
            Self::Miner(miner) => miner.run(settings, cancel_token).await,
            Self::Ping(ping) => ping.run(settings, cancel_token).await,
            Self::Pool(pool) => pool.run(settings, cancel_token).await,
            Self::Server(server) => {
                let handle = Handle::new();

                let mut sync_task = None;

                if let Some(sync_endpoint) = &settings.server_sync_endpoint {
                    let hostname = System::host_name().ok_or(anyhow!("no hostname found"))?;

                    if !sync_endpoint.contains(&hostname) {
                        let sync_settings = settings.clone();
                        let endpoint = sync_endpoint.clone();
                        let sync_cancel_token = cancel_token.clone();
                        let send_task = tokio::spawn(async move {
                            if let Err(e) = sync::Sync::run_with_settings(
                                sync_settings,
                                endpoint,
                                sync_cancel_token,
                            )
                            .await
                            {
                                error!("SyncSend failed: {}", e);
                            }
                        });
                        sync_task = Some(send_task);
                        info!("Started sync to endpoint: {sync_endpoint}");
                    }
                }

                let server_result = server.run(settings, handle, cancel_token).await;

                if let Some(task) = sync_task {
                    info!("Waiting for sync task to finish...");
                    let _ = task.await;
                    info!("Sync task finished");
                }

                server_result
            }
            Self::Settings(cmd) => cmd.run(settings).await,
            Self::Sync(sync) => sync.run(settings, cancel_token).await,
            Self::Template(template) => template.run(settings, cancel_token).await,
        }
    }
}
