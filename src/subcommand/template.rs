use {
    super::*,
    crate::stratum::{Client, Message},
};

#[derive(Debug, Parser)]
pub struct Template {
    #[arg(help = "Stratum <HOST:PORT>.")]
    stratum_endpoint: String,
    #[arg(long, help = "Stratum <USERNAME>.")]
    pub username: String,
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
    pub async fn run(self, cancel_token: CancellationToken) -> anyhow::Result<()> {
        info!(
            "Connecting to {} with user {}",
            self.stratum_endpoint, self.username
        );

        let address = resolve_stratum_endpoint(&self.stratum_endpoint).await?;

        let mut client = Client::connect(
            address,
            self.username,
            self.password,
            Duration::from_secs(5),
        )
        .await?;

        let (subscription, _, _) = client.subscribe(USER_AGENT.into()).await?;

        client.authorize().await?;

        loop {
            tokio::select! {
                _ = cancel_token.cancelled() => {
                    info!("Shutting down template monitor");
                    break;
                }
                message = client.incoming.recv() => {
                    if let Some(message) = message
                        && let Message::Notification { method, params } = message
                        && method == "mining.notify"
                        && let Ok(notify) = serde_json::from_value::<Notify>(params)
                    {
                        let output = Output {
                            stratum_endpoint: self.stratum_endpoint.clone(),
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

                        if !self.watch {
                            break;
                        }
                    }
                }
            }
        }

        Ok(())
    }
}
