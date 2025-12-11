use {
    super::*,
    crate::stratum::{Client, ClientConfig, Event, Notify, SubscribeResult},
    bitcoin::{Transaction, consensus::Decodable},
    std::io::Cursor,
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
    #[arg(
        long,
        help = "Show raw mining.notify JSON array as received from protocol."
    )]
    pub raw: bool,
}

/// Interpreted block template output
#[derive(Debug, Serialize, Deserialize)]
pub struct InterpretedOutput {
    pub job_id: JobId,
    pub prevhash: String,
    pub coinbase: CoinbaseInfo,
    pub merkle_branches: Vec<String>,
    pub version: String,
    pub nbits: String,
    pub ntime: String,
    pub ntime_human: String,
    pub difficulty: f64,
    pub clean_jobs: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CoinbaseInfo {
    pub input_text: Option<String>,
    pub outputs: Vec<CoinbaseOutput>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CoinbaseOutput {
    pub value_sats: u64,
    pub value_btc: f64,
}

impl Template {
    pub async fn run(self, cancel_token: CancellationToken) -> anyhow::Result<()> {
        info!(
            "Connecting to {} with user {}",
            self.stratum_endpoint, self.username
        );

        let address = resolve_stratum_endpoint(&self.stratum_endpoint).await?;

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

        loop {
            tokio::select! {
                _ = cancel_token.cancelled() => {
                    info!("Shutting down template monitor");
                    break;
                }
                event = events.recv() => {
                    match event {
                        Ok(Event::Notify(notify)) => {
                            let timestamp = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap()
                                .as_secs();

                            if self.raw {
                                // Print raw mining.notify as JSON array (protocol format)
                                println!("{}", serde_json::to_string_pretty(&notify)?);
                            } else {
                                let output = self.interpret_template(
                                    &subscription,
                                    &notify,
                                    &address,
                                    timestamp,
                                )?;
                                println!("{}", serde_json::to_string_pretty(&output)?);
                            }

                            if !self.watch {
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

    fn interpret_template(
        &self,
        subscription: &SubscribeResult,
        notify: &Notify,
        _address: &std::net::SocketAddr,
        _timestamp: u64,
    ) -> anyhow::Result<InterpretedOutput> {
        // Build full coinbase transaction
        let extranonce2_placeholder = "00".repeat(subscription.extranonce2_size);
        let coinbase_hex = format!(
            "{}{}{}{}",
            notify.coinb1, subscription.extranonce1, extranonce2_placeholder, notify.coinb2
        );

        let coinbase_bytes = hex::decode(&coinbase_hex)?;
        let coinbase_tx = Transaction::consensus_decode(&mut Cursor::new(&coinbase_bytes))?;

        // Extract ASCII text from coinbase input
        let input_text = Self::extract_coinbase_text(&coinbase_tx);

        // Extract outputs
        let outputs: Vec<CoinbaseOutput> = coinbase_tx
            .output
            .iter()
            .map(|out| {
                let sats = out.value.to_sat();
                CoinbaseOutput {
                    value_sats: sats,
                    value_btc: sats as f64 / 100_000_000.0,
                }
            })
            .collect();

        // Calculate difficulty
        let (difficulty, _) = Self::calculate_difficulty_and_target(notify.nbits);

        // Parse ntime for human readable
        let ntime_str = notify.ntime.to_string();
        let ntime_u64 = u64::from_str_radix(&ntime_str, 16).unwrap_or(0);

        Ok(InterpretedOutput {
            job_id: notify.job_id,
            prevhash: notify.prevhash.to_string(),
            coinbase: CoinbaseInfo {
                input_text,
                outputs,
            },
            merkle_branches: notify
                .merkle_branches
                .iter()
                .map(|m| m.to_string())
                .collect(),
            version: notify.version.to_string(),
            nbits: notify.nbits.to_string(),
            ntime: ntime_str,
            ntime_human: Self::format_timestamp(ntime_u64),
            difficulty,
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

    fn calculate_difficulty_and_target(nbits: Nbits) -> (f64, String) {
        let difficulty = crate::stratum::Difficulty::from(nbits);
        let diff_float = difficulty.as_f64();
        let target_bytes = difficulty.to_target().to_be_bytes();
        let target_hex = hex::encode(target_bytes);
        (diff_float, target_hex)
    }

    fn format_timestamp(unix_time: u64) -> String {
        chrono::DateTime::from_timestamp(unix_time as i64, 0)
            .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
            .unwrap_or_else(|| unix_time.to_string())
    }
}
