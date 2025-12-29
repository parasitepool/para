use {
    super::*,
    crate::stratum::{
        Client, ClientConfig, Difficulty, Event, MerkleNode, Notify, SubscribeResult, Username,
        merkle_root,
    },
};

#[derive(Debug, Parser)]
pub struct Template {
    #[arg(help = "Stratum <HOST:PORT>.")]
    stratum_endpoint: String,
    #[arg(long, help = "Stratum <USERNAME>.")]
    pub username: Username,
    #[arg(long, help = "Stratum <PASSWORD>.")]
    pub password: Option<String>,
    #[arg(long, help = "Continue watching for template updates.")]
    pub watch: bool,
    #[arg(long, help = "Show raw mining.notify message.")]
    pub raw: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Output {
    pub job_id: JobId,
    pub prevhash: PrevHash,
    pub previous_block_hash: BlockHash,
    pub coinbase: CoinbaseInfo,
    pub merkle_root: MerkleNode,
    pub merkle_branches: Vec<MerkleNode>,
    pub ntime: Ntime,
    pub ntime_human: String,
    pub nbits: Nbits,
    pub network_difficulty: Difficulty,
    pub pool_difficulty: Option<Difficulty>,
    pub version: Version,
    pub clean_jobs: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoinbaseInfo {
    pub size_bytes: usize,
    pub ascii_tag: Option<String>,
    pub outputs: Vec<CoinbaseOutput>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoinbaseOutput {
    pub value: Amount,
    pub script_pubkey: ScriptBuf,
    pub address: Option<String>,
}

impl Template {
    pub async fn run(self, cancel_token: CancellationToken) -> anyhow::Result<()> {
        info!(
            "Connecting to {} with user {}",
            self.stratum_endpoint, self.username
        );

        let address = resolve_stratum_endpoint(&self.stratum_endpoint)
            .await
            .with_context(|| format!("Failed to resolve endpoint '{}'", self.stratum_endpoint))?;

        let config = ClientConfig {
            address: address.to_string(),
            username: self.username.clone(),
            user_agent: USER_AGENT.into(),
            password: self.password.clone(),
            timeout: Duration::from_secs(5),
        };

        let client = Client::new(config);
        let mut events = client.connect().await?;

        let (subscription, _, _) = client.subscribe().await?;

        client.authorize().await?;

        let mut pool_difficulty = None;

        loop {
            tokio::select! {
                _ = cancel_token.cancelled() => {
                    info!("Shutting down template monitor");
                    break;
                }
                event = events.recv() => {
                    match event {
                        Ok(Event::Notify(notify)) => {
                            if self.raw {
                                println!("{}", serde_json::to_string_pretty(&notify)?);
                            } else {
                                let output = self.interpret_template(
                                    &subscription,
                                    &notify,
                                    pool_difficulty,
                                )?;

                                println!("{}", serde_json::to_string_pretty(&output)?);

                            }
                        }
                        Ok(Event::SetDifficulty(difficulty)) => {
                           pool_difficulty = Some(difficulty);
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

    fn interpret_template(
        &self,
        subscription: &SubscribeResult,
        notify: &Notify,
        pool_difficulty: Option<Difficulty>,
    ) -> anyhow::Result<Output> {
        let extranonce2 = Extranonce::random(subscription.extranonce2_size);
        let coinbase_bin = hex::decode(format!(
            "{}{}{}{}",
            notify.coinb1, subscription.extranonce1, extranonce2, notify.coinb2
        ))?;

        let mut cursor = bitcoin::io::Cursor::new(&coinbase_bin);
        let coinbase_tx = bitcoin::Transaction::consensus_decode_from_finite_reader(&mut cursor)?;

        let ascii_tag = Self::extract_coinbase_text(&coinbase_tx);

        let outputs = coinbase_tx
            .output
            .iter()
            .map(|txout| CoinbaseOutput {
                value: txout.value,
                script_pubkey: txout.script_pubkey.clone(),
                address: Address::from_script(&txout.script_pubkey, Network::Bitcoin)
                    .map(|address| address.to_string())
                    .ok(),
            })
            .collect();

        let ntime_str = notify.ntime.to_string();
        let ntime_u64 = u64::from_str_radix(&ntime_str, 16).unwrap_or(0);

        let merkle_root = merkle_root(
            &notify.coinb1,
            &notify.coinb2,
            &subscription.extranonce1,
            &extranonce2,
            &notify.merkle_branches,
        )?;

        Ok(Output {
            job_id: notify.job_id,
            prevhash: notify.prevhash.clone(),
            previous_block_hash: BlockHash::from(notify.prevhash.clone()),
            coinbase: CoinbaseInfo {
                size_bytes: coinbase_bin.len(),
                ascii_tag,
                outputs,
            },
            merkle_root,
            merkle_branches: notify.merkle_branches.clone(),
            ntime: notify.ntime,
            ntime_human: Self::format_timestamp(ntime_u64),
            nbits: notify.nbits,
            network_difficulty: Difficulty::from(notify.nbits),
            pool_difficulty,
            version: notify.version,
            clean_jobs: notify.clean_jobs,
        })
    }

    fn extract_coinbase_text(tx: &Transaction) -> Option<String> {
        if tx.input.is_empty() {
            return None;
        }

        let script_sig = &tx.input[0].script_sig;
        let bytes = script_sig.as_bytes();

        // Extract ASCII strings from the scriptSig (skip first 4 bytes - height)
        let mut ascii_parts: Vec<String> = Vec::new();
        let mut current_string = String::new();

        for &byte in bytes.iter().skip(4) {
            if (0x20..=0x7e).contains(&byte) {
                current_string.push(byte as char);
            } else if !current_string.is_empty() {
                if current_string.len() >= 3 {
                    ascii_parts.push(current_string.clone());
                }
                current_string.clear();
            }
        }
        if current_string.len() >= 3 {
            ascii_parts.push(current_string);
        }

        if ascii_parts.is_empty() {
            None
        } else {
            Some(ascii_parts.join(" "))
        }
    }

    fn format_timestamp(unix_time: u64) -> String {
        chrono::DateTime::from_timestamp(unix_time as i64, 0)
            .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
            .unwrap_or_else(|| unix_time.to_string())
    }
}
