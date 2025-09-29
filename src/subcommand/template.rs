use super::*;
use crate::stratum::{Client, Message};

#[derive(Debug, Parser)]
pub struct Template {
    stratum_endpoint: String,
    #[arg(long, help = "Stratum <USERNAME>.")]
    pub username: String,
    #[arg(long, help = "Stratum <PASSWORD>.")]
    pub password: Option<String>,
    #[arg(long, help = "Continue watching for template updates.")]
    pub watch: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TemplateData {
    pub timestamp: u64,
    pub pool_url: String,
    pub job_id: Option<String>,
    pub prev_hash: Option<String>,
    pub coinbase1: Option<String>,
    pub coinbase2: Option<String>,
    pub merkle_branches: Option<Vec<String>>,
    pub version: Option<String>,
    pub nbits: Option<String>,
    pub ntime: Option<String>,
    pub clean_jobs: Option<bool>,
    pub extranonce1: Option<String>,
    pub extranonce2_length: Option<u64>,
}

impl Template {
    pub async fn run(self) -> anyhow::Result<()> {
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

        let (subscription, _, _) = client.subscribe().await?;

        client.authorize().await?;

        let pool_url = format!("{}", address);

        loop {
            if let Some(message) = client.incoming.recv().await {
                eprintln!("Received message: {:?}", message);

                if let Message::Notification { method, params } = message
                    && method == "mining.notify"
                    && let Ok(notify) = serde_json::from_value::<crate::stratum::Notify>(params)
                {
                    let template = TemplateData {
                        timestamp: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_secs(),
                        pool_url: pool_url.clone(),
                        job_id: Some(notify.job_id.clone()),
                        prev_hash: Some(notify.prevhash.to_string()),
                        coinbase1: Some(notify.coinb1.clone()),
                        coinbase2: Some(notify.coinb2.clone()),
                        merkle_branches: Some(
                            notify
                                .merkle_branches
                                .iter()
                                .map(|b| b.to_string())
                                .collect(),
                        ),
                        version: Some(notify.version.to_string()),
                        nbits: Some(notify.nbits.to_string()),
                        ntime: Some(notify.ntime.to_string()),
                        clean_jobs: Some(notify.clean_jobs),
                        extranonce1: Some(subscription.extranonce1.to_string()),
                        extranonce2_length: Some(subscription.extranonce2_size as u64),
                    };

                    let output = serde_json::to_string(&template)?;
                    println!("{}", output);

                    if !self.watch {
                        break;
                    }
                }
            }

            sleep(Duration::from_millis(100)).await;
        }

        Ok(())
    }
}
