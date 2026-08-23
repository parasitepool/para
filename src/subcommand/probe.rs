use {
    super::*,
    std::net::IpAddr,
    stratum::client::{Client, Event, EventReceiver},
};

#[derive(Debug, Parser)]
pub struct Probe {
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
    #[arg(long, default_value = "5", help = "Fail after <TIMEOUT> seconds.")]
    pub timeout: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Output {
    pub endpoint: String,
    pub dns: Option<DnsInfo>,
    pub tcp: Option<TcpInfo>,
    pub subscribe: Option<SubscribeInfo>,
    pub authorize: Option<AuthorizeInfo>,
    pub notify: Option<NotifyInfo>,
    pub coinbase: Option<CoinbaseInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DnsInfo {
    pub resolved: Vec<IpAddr>,
    pub lookup_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TcpInfo {
    pub peer_address: SocketAddr,
    pub connect_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscribeInfo {
    pub rtt_ms: f64,
    pub enonce1: Extranonce,
    pub enonce2_size: usize,
    pub subscriptions: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthorizeInfo {
    pub rtt_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotifyInfo {
    pub wait_ms: f64,
    pub job_id: JobId,
    pub prevhash: PrevHash,
    pub previous_block_hash: BlockHash,
    pub merkle_root: MerkleNode,
    pub merkle_branches: Vec<MerkleNode>,
    pub ntime: Ntime,
    pub ntime_human: String,
    pub ntime_skew_seconds: i64,
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

impl Output {
    fn new(endpoint: String) -> Self {
        Self {
            endpoint,
            dns: None,
            tcp: None,
            subscribe: None,
            authorize: None,
            notify: None,
            coinbase: None,
        }
    }
}

impl Probe {
    pub async fn run(self, cancel_token: CancellationToken) -> Result {
        info!(
            "Connecting to {} with user {}",
            self.stratum_endpoint, self.username
        );

        let endpoint = ensure_port(&self.stratum_endpoint);
        let timeout = Duration::from_secs(self.timeout);

        let mut output = Output::new(endpoint.clone());

        macro_rules! bail_with_report {
            ($output:expr, $err:expr) => {{
                if !self.raw {
                    println!("{}", serde_json::to_string_pretty(&$output)?);
                }
                return Err($err);
            }};
        }

        {
            let start = Instant::now();

            let lookup = tokio::time::timeout(timeout, tokio::net::lookup_host(&endpoint))
                .await
                .with_context(|| format!("DNS lookup for `{endpoint}` timed out"))
                .and_then(|result| {
                    result.with_context(|| format!("failed to resolve `{endpoint}`"))
                });

            match lookup {
                Ok(addrs) => {
                    let resolved = addrs.map(|addr| addr.ip()).collect::<Vec<IpAddr>>();

                    output.dns = Some(DnsInfo {
                        resolved,
                        lookup_ms: start.elapsed().as_secs_f64() * 1000.0,
                    });
                }
                Err(err) => bail_with_report!(output, err),
            }
        }

        let client = Client::new(
            endpoint.clone(),
            self.username.clone(),
            self.password.clone(),
            USER_AGENT.into(),
            timeout,
        );

        let mut events = {
            let start = Instant::now();

            match client.connect().await {
                Ok(events) => {
                    let peer_address = match client.peer_address().await {
                        Ok(peer_address) => peer_address,
                        Err(err) => bail_with_report!(
                            output,
                            anyhow!(err).context("failed to get peer address")
                        ),
                    };

                    output.tcp = Some(TcpInfo {
                        peer_address,
                        connect_ms: start.elapsed().as_secs_f64() * 1000.0,
                    });

                    events
                }
                Err(err) => bail_with_report!(
                    output,
                    anyhow!(err).context(format!(
                        "failed to connect to stratum server at `{endpoint}`"
                    ))
                ),
            }
        };

        let subscription = match client.subscribe().await {
            Ok((subscription, rtt, _)) => {
                output.subscribe = Some(SubscribeInfo {
                    rtt_ms: rtt.as_secs_f64() * 1000.0,
                    enonce1: subscription.enonce1.clone(),
                    enonce2_size: subscription.enonce2_size,
                    subscriptions: subscription.subscriptions.clone(),
                });

                subscription
            }
            Err(err) => {
                bail_with_report!(
                    output,
                    anyhow!(err).context("stratum mining.subscribe failed")
                )
            }
        };

        match client.authorize().await {
            Ok((rtt, _)) => {
                output.authorize = Some(AuthorizeInfo {
                    rtt_ms: rtt.as_secs_f64() * 1000.0,
                });
            }
            Err(err) => {
                bail_with_report!(
                    output,
                    anyhow!(err).context("stratum mining.authorize failed")
                )
            }
        }

        let mut pool_difficulty = None;
        let mut wait_start = Instant::now();
        let mut first_notify = true;

        loop {
            tokio::select! {
                _ = cancel_token.cancelled() => {
                    info!("Shutting down probe");
                    break;
                }
                event = Self::recv_event(&mut events, first_notify.then_some(timeout)) => {
                    let event = match event {
                        Some(Ok(event)) => event,
                        Some(Err(err)) => {
                            bail_with_report!(output, anyhow!(err).context("stratum event error"));
                        }
                        None => {
                            bail_with_report!(
                                output,
                                anyhow!("timed out waiting for mining.notify")
                            );
                        }
                    };

                    match event {
                        Event::Notify(notify) => {
                            let wait_ms = wait_start.elapsed().as_secs_f64() * 1000.0;

                            if self.raw {
                                println!("{}", serde_json::to_string_pretty(&notify)?);
                            } else {
                                self.interpret_notify(&mut output, &subscription, &notify, pool_difficulty, wait_ms)?;

                                println!("{}", serde_json::to_string_pretty(&output)?);
                            }

                            if !self.watch {
                                break;
                            }

                            first_notify = false;
                            wait_start = Instant::now();
                        }
                        Event::SetDifficulty(difficulty) => {
                            pool_difficulty = Some(difficulty);
                        }
                        Event::Disconnected => {
                            bail_with_report!(output, anyhow!("disconnected from stratum server"));
                        }
                        _ => {}
                    }
                }
            }
        }

        Ok(())
    }

    async fn recv_event(
        events: &mut EventReceiver,
        timeout: Option<Duration>,
    ) -> Option<stratum::client::Result<Event>> {
        match timeout {
            Some(timeout) => tokio::time::timeout(timeout, events.recv()).await.ok(),
            None => Some(events.recv().await),
        }
    }

    fn interpret_notify(
        &self,
        output: &mut Output,
        subscription: &SubscribeResponse,
        notify: &Notify,
        pool_difficulty: Option<Difficulty>,
        wait_ms: f64,
    ) -> Result {
        let enonce2 = Extranonce::random(subscription.enonce2_size);
        let coinbase_bin = hex::decode(format!(
            "{}{}{}{}",
            notify.coinb1, subscription.enonce1, enonce2, notify.coinb2
        ))?;

        let mut cursor = bitcoin::io::Cursor::new(&coinbase_bin);
        let coinbase_tx = bitcoin::Transaction::consensus_decode_from_finite_reader(&mut cursor)?;

        let ascii_tag = Self::extract_coinbase_text(&coinbase_tx);

        let network = self.username.infer_network()?;

        let outputs = coinbase_tx
            .output
            .iter()
            .map(|txout| CoinbaseOutput {
                value: txout.value,
                script_pubkey: txout.script_pubkey.clone(),
                address: Address::from_script(&txout.script_pubkey, network)
                    .map(|address| address.to_string())
                    .ok(),
            })
            .collect();

        let ntime_unix = u32::from(notify.ntime);
        let ntime_human = chrono::DateTime::from_timestamp(ntime_unix.into(), 0)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_else(|| ntime_unix.to_string());

        let ntime_skew_seconds = i64::from(ntime_unix) - chrono::Utc::now().timestamp();

        let merkle_root = merkle_root(
            &notify.coinb1,
            &notify.coinb2,
            &subscription.enonce1,
            &enonce2,
            &notify.merkle_branches,
        )?;

        output.notify = Some(NotifyInfo {
            wait_ms,
            job_id: notify.job_id,
            prevhash: notify.prevhash.clone(),
            previous_block_hash: BlockHash::from(notify.prevhash.clone()),
            merkle_root,
            merkle_branches: notify.merkle_branches.clone(),
            ntime: notify.ntime,
            ntime_human,
            ntime_skew_seconds,
            nbits: notify.nbits,
            network_difficulty: Difficulty::from(notify.nbits),
            pool_difficulty,
            version: notify.version,
            clean_jobs: notify.clean_jobs,
        });

        output.coinbase = Some(CoinbaseInfo {
            size_bytes: coinbase_bin.len(),
            ascii_tag,
            outputs,
        });

        Ok(())
    }

    fn extract_coinbase_text(tx: &Transaction) -> Option<String> {
        if tx.input.is_empty() {
            return None;
        }

        let script_sig = &tx.input[0].script_sig;
        let bytes = script_sig.as_bytes();

        if bytes.is_empty() {
            return None;
        }

        let height_len = bytes[0] as usize;
        let skip_bytes = 1 + height_len;

        if bytes.len() <= skip_bytes {
            return None;
        }

        let mut ascii_parts: Vec<String> = Vec::new();
        let mut current_string = String::new();

        for &byte in bytes.iter().skip(skip_bytes) {
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
}
