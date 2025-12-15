use {
    super::*,
    crate::{
        settings::Settings,
        stratum::{Client, ClientConfig, Event},
    },
};

#[derive(Debug, Parser)]
pub struct Template {
    #[arg(help = "Stratum <HOST:PORT>.")]
    stratum_endpoint: Option<String>,
    #[arg(long, help = "Stratum <USERNAME>.")]
    pub username: Option<String>,
    #[arg(long, help = "Stratum <PASSWORD>.")]
    pub password: Option<String>,
    #[arg(long, help = "Continue watching for template updates.")]
    pub watch: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Output {
    pub stratum_endpoint: String,
    pub ip_address: String,
    pub timestamp: u64,
    pub extranonce1: Extranonce,
    pub extranonce2_size: usize,
    pub job_id: JobId,
    pub prevhash: PrevHash,
    pub coinb1: String,
    pub coinb2: String,
    pub merkle_branches: Vec<MerkleNode>,
    pub version: Version,
    pub nbits: Nbits,
    pub ntime: Ntime,
    pub clean_jobs: bool,
}

impl Template {
    pub async fn run(
        self,
        settings: Settings,
        cancel_token: CancellationToken,
    ) -> anyhow::Result<()> {
        let stratum_endpoint = self
            .stratum_endpoint
            .or(settings.template_stratum_endpoint.clone())
            .ok_or_else(|| anyhow!("stratum endpoint required"))?;

        let username = self
            .username
            .or(settings.template_username.clone())
            .ok_or_else(|| anyhow!("username required"))?;

        let password = self.password.or(settings.template_password.clone());

        let watch = self.watch || settings.template_watch;

        info!("Connecting to {stratum_endpoint} with user {username}");

        let address = resolve_stratum_endpoint(&stratum_endpoint).await?;

        let config = ClientConfig {
            address: address.to_string(),
            username: username.clone(),
            user_agent: USER_AGENT.into(),
            password,
            timeout: Duration::from_secs(5),
        };

        let client = Client::new(config);
        let mut events = client.connect().await?;

        let (subscription, _, _) = client.subscribe().await?;

        client.authorize().await?;

        loop {
            tokio::select! {
                _ = cancel_token.cancelled() => {
                    info!("Shutting down template monitor");
                    break;
                }
                event = events.recv() => {
                    match event {
                        Ok(Event::Notify(notify)) => {
                            let output = Output {
                                stratum_endpoint: stratum_endpoint.clone(),
                                ip_address: address.ip().to_string(),
                                timestamp: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap()
                                    .as_secs(),
                                extranonce1: subscription.extranonce1.clone(),
                                extranonce2_size: subscription.extranonce2_size,
                                job_id: notify.job_id,
                                prevhash: notify.prevhash,
                                coinb1: notify.coinb1,
                                coinb2: notify.coinb2,
                                merkle_branches: notify.merkle_branches,
                                version: notify.version,
                                nbits: notify.nbits,
                                ntime: notify.ntime,
                                clean_jobs: notify.clean_jobs,
                            };

                            println!("{}", serde_json::to_string_pretty(&output)?);

                            if !watch {
                                break;
                            }
                        }
                         Ok(Event::Disconnected) => {
                            error!("Disconnected from stratum server");
                            break;
                        }
                        _ => {}
                    }
                }
            }
        }

        Ok(())
    }
}
