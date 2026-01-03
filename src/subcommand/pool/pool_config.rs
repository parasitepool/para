use super::*;

#[derive(Clone, Debug, Parser)]
pub(crate) struct PoolConfig {
    #[arg(long, help = "Listen at <ADDRESS>.")]
    address: Option<String>,
    #[arg(long, help = "Enable HTTP API on <API_PORT>. Disabled if not set.")]
    api_port: Option<u16>,
    #[arg(long, help = "ACME domain for TLS certificate.")]
    acme_domain: Vec<String>,
    #[arg(long, help = "ACME contact email for TLS certificate.")]
    acme_contact: Vec<String>,
    #[arg(long, help = "ACME cache directory.", default_value = "acme-cache")]
    acme_cache: PathBuf,
    #[arg(long, help = "Load Bitcoin Core data dir from <BITCOIN_DATA_DIR>.")]
    bitcoin_data_dir: Option<PathBuf>,
    #[arg(
        long,
        help = "Authenticate to Bitcoin Core RPC with <BITCOIN_RPC_PASSWORD>."
    )]
    bitcoin_rpc_password: Option<String>,
    #[arg(long, help = "Connect to Bitcoin Core RPC at <BITCOIN_RPC_PORT>.")]
    bitcoin_rpc_port: Option<u16>,
    #[arg(
        long,
        help = "Authenticate to Bitcoin Core RPC as <BITCOIN_RPC_USERNAME>."
    )]
    bitcoin_rpc_username: Option<String>,
    #[arg(long, help = "Load Bitcoin Core RPC cookie file from <COOKIE_FILE>.")]
    bitcoin_rpc_cookie_file: Option<PathBuf>,
    #[arg(
        long,
        help = "Block template <UPDATE_INTERVAL> in seconds.",
        default_value = "10"
    )]
    update_interval: u64,
    #[arg(long = "chain", help = "Run on <CHAIN>.")]
    chain: Option<Chain>,
    #[arg(long, alias = "datadir", help = "Store acme cache in <DATA_DIR>.")]
    data_dir: Option<PathBuf>,
    #[arg(long, help = "Listen on <PORT>.")]
    port: Option<u16>,
    #[arg(
        long,
        help = "Use version rolling with <VERSION_MASK>.",
        default_value = "1fffe000"
    )]
    version_mask: Version,
    #[arg(
        long,
        help = "Give <STARTING_DIFFICULTY> to new clients.",
        default_value = "1"
    )]
    start_diff: Difficulty,
    #[arg(
        long,
        help = "Target <VARDIFF_PERIOD> seconds between share submissions.",
        default_value = "5.0"
    )]
    vardiff_period: f64,
    #[arg(
        long,
        help = "Average the share submission rate over <VARDIFF_WINDOW> seconds.",
        default_value = "300"
    )]
    vardiff_window: f64,
    #[arg(
        long,
        help = "Subscribe to <ZMQ_BLOCK_NOTIFICATION>.",
        default_value = "tcp://127.0.0.1:28332"
    )]
    zmq_block_notifications: Endpoint,
    #[arg(
        long,
        help = "Set extranonce2 size in bytes (2-8).",
        default_value = "8",
        value_parser = clap::value_parser!(u8).range(2..=8)
    )]
    extranonce2_size: u8,
    #[arg(
        long,
        help = "Warn after <REJECT_WARN_THRESHOLD> seconds of consecutive rejects.",
        default_value = "60"
    )]
    reject_warn_threshold: u64,
    #[arg(
        long,
        help = "Suggest reconnect after <REJECT_RECONNECT_THRESHOLD> seconds of consecutive rejects.",
        default_value = "120"
    )]
    reject_reconnect_threshold: u64,
    #[arg(
        long,
        help = "Force disconnect after <REJECT_DROP_THRESHOLD> seconds of consecutive rejects.",
        default_value = "180"
    )]
    reject_drop_threshold: u64,
    #[arg(
        long,
        help = "Session TTL for reconnecting miners in seconds.",
        default_value = "600"
    )]
    session_ttl: u64,
}

impl PoolConfig {
    pub(crate) fn chain(&self) -> Chain {
        self.chain.unwrap_or(Chain::Mainnet)
    }

    pub(crate) fn bitcoin_rpc_port(&self) -> u16 {
        self.bitcoin_rpc_port
            .unwrap_or_else(|| self.chain().default_rpc_port())
    }

    pub(crate) fn bitcoin_credentials(&self) -> Result<Auth> {
        if let Some((user, pass)) = &self
            .bitcoin_rpc_username
            .as_ref()
            .zip(self.bitcoin_rpc_password.as_ref())
        {
            Ok(Auth::UserPass((*user).clone(), (*pass).clone()))
        } else {
            Ok(Auth::CookieFile(self.cookie_file()?))
        }
    }

    pub(crate) fn bitcoin_rpc_url(&self) -> String {
        format!("127.0.0.1:{}/", self.bitcoin_rpc_port())
    }

    pub(crate) fn bitcoin_rpc_client(&self) -> Result<bitcoincore_rpc::Client> {
        let rpc_url = self.bitcoin_rpc_url();

        let bitcoin_credentials = self.bitcoin_credentials()?;

        info!("Connecting to Bitcoin Core at {rpc_url}",);

        let client =
            bitcoincore_rpc::Client::new(&rpc_url, bitcoin_credentials.clone()).map_err(|_| {
                anyhow!(format!(
                    "failed to connect to Bitcoin Core RPC at `{rpc_url}` with {}",
                    match bitcoin_credentials {
                        Auth::None => "no credentials".into(),
                        Auth::UserPass(_, _) => "username and password".into(),
                        Auth::CookieFile(cookie_file) =>
                            format!("cookie file at {}", cookie_file.display()),
                    }
                ))
            })?;

        let mut checks = 0;
        let rpc_chain = loop {
            match client.get_blockchain_info() {
                Ok(blockchain_info) => {
                    break match blockchain_info.chain.to_string().as_str() {
                        "bitcoin" => Chain::Mainnet,
                        "regtest" => Chain::Regtest,
                        "signet" => Chain::Signet,
                        "testnet" => Chain::Testnet,
                        "testnet4" => Chain::Testnet4,
                        other => bail!("Bitcoin RPC server on unknown chain: {other}"),
                    };
                }
                Err(bitcoincore_rpc::Error::JsonRpc(bitcoincore_rpc::jsonrpc::Error::Rpc(err)))
                    if err.code == -28 => {}
                Err(err) => {
                    bail!("Failed to connect to Bitcoin Core RPC at `{rpc_url}`:  {err}")
                }
            }

            ensure! {
              checks < 100,
              "Failed to connect to Bitcoin Core RPC at `{rpc_url}`",
            }

            checks += 1;
            thread::sleep(Duration::from_millis(100));
        };

        let para_chain = self.chain();

        if rpc_chain != para_chain {
            bail!("Bitcoin RPC server is on {rpc_chain} but para is on {para_chain}");
        }

        Ok(client)
    }

    pub(crate) fn cookie_file(&self) -> Result<PathBuf> {
        if let Some(cookie_file) = &self.bitcoin_rpc_cookie_file {
            return Ok(cookie_file.clone());
        }

        let path = if let Some(bitcoin_data_dir) = &self.bitcoin_data_dir {
            bitcoin_data_dir.clone()
        } else if cfg!(target_os = "linux") {
            dirs::home_dir()
                .ok_or_else(|| anyhow!("failed to get cookie file path: could not get home dir"))?
                .join(".bitcoin")
        } else {
            dirs::data_dir()
                .ok_or_else(|| anyhow!("failed to get cookie file path: could not get data dir"))?
                .join("Bitcoin")
        };

        let path = self.chain().join_with_data_dir(path);

        Ok(path.join(".cookie"))
    }

    pub(crate) fn port(&self) -> u16 {
        self.port.unwrap_or(42069)
    }

    pub(crate) fn address(&self) -> String {
        self.address.clone().unwrap_or("0.0.0.0".into())
    }

    pub(crate) fn version_mask(&self) -> Version {
        self.version_mask
    }

    pub(crate) fn start_diff(&self) -> Difficulty {
        self.start_diff
    }

    pub(crate) fn update_interval(&self) -> Duration {
        Duration::from_secs(self.update_interval)
    }

    pub(crate) fn zmq_block_notifications(&self) -> Endpoint {
        self.zmq_block_notifications.clone()
    }

    pub(crate) fn vardiff_period(&self) -> Duration {
        Duration::from_secs_f64(self.vardiff_period)
    }

    pub(crate) fn vardiff_window(&self) -> Duration {
        Duration::from_secs_f64(self.vardiff_window)
    }

    pub(crate) fn api_port(&self) -> Option<u16> {
        self.api_port
    }

    pub(crate) fn acme_domains(&self) -> Vec<String> {
        self.acme_domain.clone()
    }

    pub(crate) fn acme_contacts(&self) -> Vec<String> {
        self.acme_contact.clone()
    }

    pub(crate) fn acme_cache(&self) -> PathBuf {
        if let Some(data_dir) = &self.data_dir {
            data_dir.join(&self.acme_cache)
        } else {
            self.acme_cache.clone()
        }
    }

    pub(crate) fn extranonce2_size(&self) -> usize {
        self.extranonce2_size as usize
    }

    pub(crate) fn reject_config(&self) -> reject_tracker::RejectConfig {
        reject_tracker::RejectConfig {
            warn_threshold: Duration::from_secs(self.reject_warn_threshold),
            reconnect_threshold: Duration::from_secs(self.reject_reconnect_threshold),
            drop_threshold: Duration::from_secs(self.reject_drop_threshold),
        }
    }

    pub(crate) fn session_ttl(&self) -> Duration {
        Duration::from_secs(self.session_ttl)
    }
}
