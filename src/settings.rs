use {super::*, bitcoincore_rpc::Auth, std::collections::BTreeMap};

/// TOML config file structure
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    // Global settings
    pub chain: Option<Chain>,
    pub data_dir: Option<PathBuf>,
    pub config: Option<PathBuf>,
    pub config_dir: Option<PathBuf>,
    pub bitcoin_data_dir: Option<PathBuf>,
    pub bitcoin_rpc_port: Option<u16>,
    pub bitcoin_rpc_username: Option<String>,
    pub bitcoin_rpc_password: Option<String>,
    pub bitcoin_rpc_cookie_file: Option<PathBuf>,

    // Subcommand sections
    pub pool: Option<PoolSection>,
    pub server: Option<ServerSection>,
    pub miner: Option<MinerSection>,
    pub sync: Option<SyncSection>,
    pub template: Option<TemplateSection>,
    pub ping: Option<PingSection>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PoolSection {
    pub chain: Option<Chain>,
    pub address: Option<String>,
    pub port: Option<u16>,
    pub bitcoin_data_dir: Option<PathBuf>,
    pub bitcoin_rpc_port: Option<u16>,
    pub bitcoin_rpc_username: Option<String>,
    pub bitcoin_rpc_password: Option<String>,
    pub bitcoin_rpc_cookie_file: Option<PathBuf>,
    pub data_dir: Option<PathBuf>,
    pub update_interval: Option<u64>,
    pub version_mask: Option<String>,
    pub start_diff: Option<String>,
    pub vardiff_period: Option<f64>,
    pub vardiff_window: Option<f64>,
    pub zmq_block_notifications: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerSection {
    pub address: Option<String>,
    pub port: Option<u16>,
    pub data_dir: Option<PathBuf>,
    pub admin_token: Option<String>,
    pub api_token: Option<String>,
    pub acme_domain: Option<Vec<String>>,
    pub acme_contact: Option<Vec<String>>,
    pub alerts_ntfy_channel: Option<String>,
    pub database_url: Option<String>,
    pub log_dir: Option<PathBuf>,
    pub nodes: Option<Vec<String>>,
    pub sync_endpoint: Option<String>,
    pub ttl: Option<u64>,
    pub migrate_accounts: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MinerSection {
    pub stratum_endpoint: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub mode: Option<String>,
    pub cpu_cores: Option<usize>,
    pub throttle: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SyncSection {
    pub endpoint: Option<String>,
    pub batch_size: Option<i64>,
    pub database_url: Option<String>,
    pub admin_token: Option<String>,
    pub id_file: Option<String>,
    pub reset_id: Option<bool>,
    pub terminate_when_complete: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TemplateSection {
    pub stratum_endpoint: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub watch: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PingSection {
    pub stratum_endpoint: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub count: Option<u64>,
    pub timeout: Option<u64>,
}

/// Unified settings struct with all resolved configuration
#[derive(Debug, Clone, Default, Serialize)]
pub struct Settings {
    // Global / shared settings
    pub chain: Option<Chain>,
    pub data_dir: Option<PathBuf>,
    pub config: Option<PathBuf>,
    pub config_dir: Option<PathBuf>,
    pub bitcoin_data_dir: Option<PathBuf>,
    pub bitcoin_rpc_port: Option<u16>,
    pub bitcoin_rpc_username: Option<String>,
    pub bitcoin_rpc_password: Option<String>,
    pub bitcoin_rpc_cookie_file: Option<PathBuf>,

    // Pool settings
    pub pool_address: Option<String>,
    pub pool_port: Option<u16>,
    pub pool_update_interval: Option<u64>,
    pub pool_version_mask: Option<String>,
    pub pool_start_diff: Option<String>,
    pub pool_vardiff_period: Option<f64>,
    pub pool_vardiff_window: Option<f64>,
    pub pool_zmq_block_notifications: Option<String>,

    // Server settings
    pub server_address: Option<String>,
    pub server_port: Option<u16>,
    pub server_admin_token: Option<String>,
    pub server_api_token: Option<String>,
    pub server_acme_domain: Option<Vec<String>>,
    pub server_acme_contact: Option<Vec<String>>,
    pub server_alerts_ntfy_channel: Option<String>,
    pub server_database_url: Option<String>,
    pub server_log_dir: Option<PathBuf>,
    pub server_nodes: Option<Vec<String>>,
    pub server_sync_endpoint: Option<String>,
    pub server_ttl: Option<u64>,
    pub server_migrate_accounts: bool,

    // Miner settings
    pub miner_stratum_endpoint: Option<String>,
    pub miner_username: Option<String>,
    pub miner_password: Option<String>,
    pub miner_mode: Option<String>,
    pub miner_cpu_cores: Option<usize>,
    pub miner_throttle: Option<String>,

    // Sync settings
    pub sync_endpoint: Option<String>,
    pub sync_batch_size: Option<i64>,
    pub sync_database_url: Option<String>,
    pub sync_admin_token: Option<String>,
    pub sync_id_file: Option<String>,
    pub sync_reset_id: bool,
    pub sync_terminate_when_complete: bool,

    // Template settings
    pub template_stratum_endpoint: Option<String>,
    pub template_username: Option<String>,
    pub template_password: Option<String>,
    pub template_watch: bool,

    // Ping settings
    pub ping_stratum_endpoint: Option<String>,
    pub ping_username: Option<String>,
    pub ping_password: Option<String>,
    pub ping_count: Option<u64>,
    pub ping_timeout: Option<u64>,
}

impl Settings {
    /// Load settings from all sources with proper priority
    pub fn load(options: crate::options::Options) -> Result<Self> {
        let mut env = BTreeMap::<String, String>::new();

        for (var, value) in std::env::vars_os() {
            let Some(var) = var.to_str() else {
                continue;
            };

            let Some(key) = var.strip_prefix("PARA_") else {
                continue;
            };

            env.insert(
                key.into(),
                value.into_string().map_err(|value| {
                    anyhow!(
                        "environment variable `{var}` not valid unicode: `{}`",
                        value.to_string_lossy()
                    )
                })?,
            );
        }

        Self::merge(options, env)
    }

    /// Merge all configuration sources
    pub fn merge(options: crate::options::Options, env: BTreeMap<String, String>) -> Result<Self> {
        // Start with CLI options (highest priority)
        let settings = Self::from_options(&options);

        // Merge with environment variables
        let settings = settings.or(Self::from_env(&env)?);

        // Determine config path
        let config_path = Self::find_config_path(&settings)?;

        // Load and merge config file
        let config = if let Some(config_path) = config_path {
            toml::from_str(&fs::read_to_string(&config_path).context(anyhow!(
                "failed to open config file `{}`",
                config_path.display()
            ))?)
            .context(anyhow!(
                "failed to deserialize config file `{}`",
                config_path.display()
            ))?
        } else {
            Config::default()
        };

        // Merge with config file (subcommand section has priority over global)
        let settings = settings.or(Self::from_config(&config));

        // Apply defaults
        let settings = settings.or_defaults()?;

        // Validate
        Self::validate(&settings)?;

        Ok(settings)
    }

    fn find_config_path(settings: &Self) -> Result<Option<PathBuf>> {
        // 1. Explicit --config flag
        if let Some(path) = &settings.config {
            return Ok(Some(path.clone()));
        }

        // 2. --config-dir/para.toml
        if let Some(dir) = &settings.config_dir {
            let path = dir.join("para.toml");
            if path.exists() {
                return Ok(Some(path));
            }
        }

        // 3. --data-dir/para.toml
        if let Some(dir) = &settings.data_dir {
            let path = dir.join("para.toml");
            if path.exists() {
                return Ok(Some(path));
            }
        }

        // 4. XDG config dir (~/.config/para/para.toml)
        if let Some(config_dir) = dirs::config_dir() {
            let path = config_dir.join("para").join("para.toml");
            if path.exists() {
                return Ok(Some(path));
            }
        }

        Ok(None)
    }

    pub fn from_options(options: &crate::options::Options) -> Self {
        Self {
            chain: options
                .signet
                .then_some(Chain::Signet)
                .or(options.regtest.then_some(Chain::Regtest))
                .or(options.testnet.then_some(Chain::Testnet))
                .or(options.testnet4.then_some(Chain::Testnet4))
                .or(options.chain),
            data_dir: options.data_dir.clone(),
            config: options.config.clone(),
            config_dir: options.config_dir.clone(),
            bitcoin_data_dir: options.bitcoin_data_dir.clone(),
            bitcoin_rpc_port: options.bitcoin_rpc_port,
            bitcoin_rpc_username: options.bitcoin_rpc_username.clone(),
            bitcoin_rpc_password: options.bitcoin_rpc_password.clone(),
            bitcoin_rpc_cookie_file: options.bitcoin_rpc_cookie_file.clone(),
            ..Default::default()
        }
    }

    pub fn from_env(env: &BTreeMap<String, String>) -> Result<Self> {
        let get_bool = |key: &str| {
            env.get(key)
                .map(|value| !value.is_empty() && value != "0" && value.to_lowercase() != "false")
                .unwrap_or_default()
        };

        let get_string = |key: &str| env.get(key).cloned();

        let get_path = |key: &str| env.get(key).map(PathBuf::from);

        let get_chain = |key: &str| -> Result<Option<Chain>> {
            env.get(key)
                .map(|chain| chain.parse::<Chain>())
                .transpose()
                .with_context(|| {
                    format!("failed to parse environment variable PARA_{key} as chain")
                })
        };

        let get_u16 = |key: &str| -> Result<Option<u16>> {
            env.get(key)
                .map(|int| int.parse::<u16>())
                .transpose()
                .with_context(|| format!("failed to parse environment variable PARA_{key} as u16"))
        };

        let get_u64 = |key: &str| -> Result<Option<u64>> {
            env.get(key)
                .map(|int| int.parse::<u64>())
                .transpose()
                .with_context(|| format!("failed to parse environment variable PARA_{key} as u64"))
        };

        let get_i64 = |key: &str| -> Result<Option<i64>> {
            env.get(key)
                .map(|int| int.parse::<i64>())
                .transpose()
                .with_context(|| format!("failed to parse environment variable PARA_{key} as i64"))
        };

        let get_f64 = |key: &str| -> Result<Option<f64>> {
            env.get(key)
                .map(|f| f.parse::<f64>())
                .transpose()
                .with_context(|| format!("failed to parse environment variable PARA_{key} as f64"))
        };

        let get_usize = |key: &str| -> Result<Option<usize>> {
            env.get(key)
                .map(|int| int.parse::<usize>())
                .transpose()
                .with_context(|| {
                    format!("failed to parse environment variable PARA_{key} as usize")
                })
        };

        let get_vec = |key: &str| -> Option<Vec<String>> {
            env.get(key).map(|s| {
                s.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
        };

        Ok(Self {
            // Global
            chain: get_chain("CHAIN")?,
            data_dir: get_path("DATA_DIR"),
            config: get_path("CONFIG"),
            config_dir: get_path("CONFIG_DIR"),
            bitcoin_data_dir: get_path("BITCOIN_DATA_DIR"),
            bitcoin_rpc_port: get_u16("BITCOIN_RPC_PORT")?,
            bitcoin_rpc_username: get_string("BITCOIN_RPC_USERNAME"),
            bitcoin_rpc_password: get_string("BITCOIN_RPC_PASSWORD"),
            bitcoin_rpc_cookie_file: get_path("BITCOIN_RPC_COOKIE_FILE"),

            // Pool
            pool_address: get_string("POOL_ADDRESS"),
            pool_port: get_u16("POOL_PORT")?,
            pool_update_interval: get_u64("POOL_UPDATE_INTERVAL")?,
            pool_version_mask: get_string("POOL_VERSION_MASK"),
            pool_start_diff: get_string("POOL_START_DIFF"),
            pool_vardiff_period: get_f64("POOL_VARDIFF_PERIOD")?,
            pool_vardiff_window: get_f64("POOL_VARDIFF_WINDOW")?,
            pool_zmq_block_notifications: get_string("POOL_ZMQ_BLOCK_NOTIFICATIONS"),

            // Server
            server_address: get_string("SERVER_ADDRESS"),
            server_port: get_u16("SERVER_PORT")?,
            server_admin_token: get_string("SERVER_ADMIN_TOKEN"),
            server_api_token: get_string("SERVER_API_TOKEN"),
            server_acme_domain: get_vec("SERVER_ACME_DOMAIN"),
            server_acme_contact: get_vec("SERVER_ACME_CONTACT"),
            server_alerts_ntfy_channel: get_string("SERVER_ALERTS_NTFY_CHANNEL"),
            server_database_url: get_string("SERVER_DATABASE_URL").or(get_string("DATABASE_URL")),
            server_log_dir: get_path("SERVER_LOG_DIR"),
            server_nodes: get_vec("SERVER_NODES"),
            server_sync_endpoint: get_string("SERVER_SYNC_ENDPOINT"),
            server_ttl: get_u64("SERVER_TTL")?,
            server_migrate_accounts: get_bool("SERVER_MIGRATE_ACCOUNTS"),

            // Miner
            miner_stratum_endpoint: get_string("MINER_STRATUM_ENDPOINT"),
            miner_username: get_string("MINER_USERNAME"),
            miner_password: get_string("MINER_PASSWORD"),
            miner_mode: get_string("MINER_MODE"),
            miner_cpu_cores: get_usize("MINER_CPU_CORES")?,
            miner_throttle: get_string("MINER_THROTTLE"),

            // Sync
            sync_endpoint: get_string("SYNC_ENDPOINT"),
            sync_batch_size: get_i64("SYNC_BATCH_SIZE")?,
            sync_database_url: get_string("SYNC_DATABASE_URL").or(get_string("DATABASE_URL")),
            sync_admin_token: get_string("SYNC_ADMIN_TOKEN"),
            sync_id_file: get_string("SYNC_ID_FILE"),
            sync_reset_id: get_bool("SYNC_RESET_ID"),
            sync_terminate_when_complete: get_bool("SYNC_TERMINATE_WHEN_COMPLETE"),

            // Template
            template_stratum_endpoint: get_string("TEMPLATE_STRATUM_ENDPOINT"),
            template_username: get_string("TEMPLATE_USERNAME"),
            template_password: get_string("TEMPLATE_PASSWORD"),
            template_watch: get_bool("TEMPLATE_WATCH"),

            // Ping
            ping_stratum_endpoint: get_string("PING_STRATUM_ENDPOINT"),
            ping_username: get_string("PING_USERNAME"),
            ping_password: get_string("PING_PASSWORD"),
            ping_count: get_u64("PING_COUNT")?,
            ping_timeout: get_u64("PING_TIMEOUT")?,
        })
    }

    pub fn from_config(config: &Config) -> Self {
        let pool = config.pool.as_ref();
        let server = config.server.as_ref();
        let miner = config.miner.as_ref();
        let sync = config.sync.as_ref();
        let template = config.template.as_ref();
        let ping = config.ping.as_ref();

        Self {
            // Global settings (subcommand section overrides global)
            chain: pool.and_then(|p| p.chain).or(config.chain),
            data_dir: pool
                .and_then(|p| p.data_dir.clone())
                .or(config.data_dir.clone()),
            config: config.config.clone(),
            config_dir: config.config_dir.clone(),
            bitcoin_data_dir: pool
                .and_then(|p| p.bitcoin_data_dir.clone())
                .or(config.bitcoin_data_dir.clone()),
            bitcoin_rpc_port: pool
                .and_then(|p| p.bitcoin_rpc_port)
                .or(config.bitcoin_rpc_port),
            bitcoin_rpc_username: pool
                .and_then(|p| p.bitcoin_rpc_username.clone())
                .or(config.bitcoin_rpc_username.clone()),
            bitcoin_rpc_password: pool
                .and_then(|p| p.bitcoin_rpc_password.clone())
                .or(config.bitcoin_rpc_password.clone()),
            bitcoin_rpc_cookie_file: pool
                .and_then(|p| p.bitcoin_rpc_cookie_file.clone())
                .or(config.bitcoin_rpc_cookie_file.clone()),

            // Pool
            pool_address: pool.and_then(|p| p.address.clone()),
            pool_port: pool.and_then(|p| p.port),
            pool_update_interval: pool.and_then(|p| p.update_interval),
            pool_version_mask: pool.and_then(|p| p.version_mask.clone()),
            pool_start_diff: pool.and_then(|p| p.start_diff.clone()),
            pool_vardiff_period: pool.and_then(|p| p.vardiff_period),
            pool_vardiff_window: pool.and_then(|p| p.vardiff_window),
            pool_zmq_block_notifications: pool.and_then(|p| p.zmq_block_notifications.clone()),

            // Server
            server_address: server.and_then(|s| s.address.clone()),
            server_port: server.and_then(|s| s.port),
            server_admin_token: server.and_then(|s| s.admin_token.clone()),
            server_api_token: server.and_then(|s| s.api_token.clone()),
            server_acme_domain: server.and_then(|s| s.acme_domain.clone()),
            server_acme_contact: server.and_then(|s| s.acme_contact.clone()),
            server_alerts_ntfy_channel: server.and_then(|s| s.alerts_ntfy_channel.clone()),
            server_database_url: server.and_then(|s| s.database_url.clone()),
            server_log_dir: server.and_then(|s| s.log_dir.clone()),
            server_nodes: server.and_then(|s| s.nodes.clone()),
            server_sync_endpoint: server.and_then(|s| s.sync_endpoint.clone()),
            server_ttl: server.and_then(|s| s.ttl),
            server_migrate_accounts: server.and_then(|s| s.migrate_accounts).unwrap_or(false),

            // Miner
            miner_stratum_endpoint: miner.and_then(|m| m.stratum_endpoint.clone()),
            miner_username: miner.and_then(|m| m.username.clone()),
            miner_password: miner.and_then(|m| m.password.clone()),
            miner_mode: miner.and_then(|m| m.mode.clone()),
            miner_cpu_cores: miner.and_then(|m| m.cpu_cores),
            miner_throttle: miner.and_then(|m| m.throttle.clone()),

            // Sync
            sync_endpoint: sync.and_then(|s| s.endpoint.clone()),
            sync_batch_size: sync.and_then(|s| s.batch_size),
            sync_database_url: sync.and_then(|s| s.database_url.clone()),
            sync_admin_token: sync.and_then(|s| s.admin_token.clone()),
            sync_id_file: sync.and_then(|s| s.id_file.clone()),
            sync_reset_id: sync.and_then(|s| s.reset_id).unwrap_or(false),
            sync_terminate_when_complete: sync
                .and_then(|s| s.terminate_when_complete)
                .unwrap_or(false),

            // Template
            template_stratum_endpoint: template.and_then(|t| t.stratum_endpoint.clone()),
            template_username: template.and_then(|t| t.username.clone()),
            template_password: template.and_then(|t| t.password.clone()),
            template_watch: template.and_then(|t| t.watch).unwrap_or(false),

            // Ping
            ping_stratum_endpoint: ping.and_then(|p| p.stratum_endpoint.clone()),
            ping_username: ping.and_then(|p| p.username.clone()),
            ping_password: ping.and_then(|p| p.password.clone()),
            ping_count: ping.and_then(|p| p.count),
            ping_timeout: ping.and_then(|p| p.timeout),
        }
    }

    /// Merge self with another Settings, self takes priority
    pub fn or(self, other: Self) -> Self {
        Self {
            // Global
            chain: self.chain.or(other.chain),
            data_dir: self.data_dir.or(other.data_dir),
            config: self.config.or(other.config),
            config_dir: self.config_dir.or(other.config_dir),
            bitcoin_data_dir: self.bitcoin_data_dir.or(other.bitcoin_data_dir),
            bitcoin_rpc_port: self.bitcoin_rpc_port.or(other.bitcoin_rpc_port),
            bitcoin_rpc_username: self.bitcoin_rpc_username.or(other.bitcoin_rpc_username),
            bitcoin_rpc_password: self.bitcoin_rpc_password.or(other.bitcoin_rpc_password),
            bitcoin_rpc_cookie_file: self
                .bitcoin_rpc_cookie_file
                .or(other.bitcoin_rpc_cookie_file),

            // Pool
            pool_address: self.pool_address.or(other.pool_address),
            pool_port: self.pool_port.or(other.pool_port),
            pool_update_interval: self.pool_update_interval.or(other.pool_update_interval),
            pool_version_mask: self.pool_version_mask.or(other.pool_version_mask),
            pool_start_diff: self.pool_start_diff.or(other.pool_start_diff),
            pool_vardiff_period: self.pool_vardiff_period.or(other.pool_vardiff_period),
            pool_vardiff_window: self.pool_vardiff_window.or(other.pool_vardiff_window),
            pool_zmq_block_notifications: self
                .pool_zmq_block_notifications
                .or(other.pool_zmq_block_notifications),

            // Server
            server_address: self.server_address.or(other.server_address),
            server_port: self.server_port.or(other.server_port),
            server_admin_token: self.server_admin_token.or(other.server_admin_token),
            server_api_token: self.server_api_token.or(other.server_api_token),
            server_acme_domain: self.server_acme_domain.or(other.server_acme_domain),
            server_acme_contact: self.server_acme_contact.or(other.server_acme_contact),
            server_alerts_ntfy_channel: self
                .server_alerts_ntfy_channel
                .or(other.server_alerts_ntfy_channel),
            server_database_url: self.server_database_url.or(other.server_database_url),
            server_log_dir: self.server_log_dir.or(other.server_log_dir),
            server_nodes: self.server_nodes.or(other.server_nodes),
            server_sync_endpoint: self.server_sync_endpoint.or(other.server_sync_endpoint),
            server_ttl: self.server_ttl.or(other.server_ttl),
            server_migrate_accounts: self.server_migrate_accounts || other.server_migrate_accounts,

            // Miner
            miner_stratum_endpoint: self.miner_stratum_endpoint.or(other.miner_stratum_endpoint),
            miner_username: self.miner_username.or(other.miner_username),
            miner_password: self.miner_password.or(other.miner_password),
            miner_mode: self.miner_mode.or(other.miner_mode),
            miner_cpu_cores: self.miner_cpu_cores.or(other.miner_cpu_cores),
            miner_throttle: self.miner_throttle.or(other.miner_throttle),

            // Sync
            sync_endpoint: self.sync_endpoint.or(other.sync_endpoint),
            sync_batch_size: self.sync_batch_size.or(other.sync_batch_size),
            sync_database_url: self.sync_database_url.or(other.sync_database_url),
            sync_admin_token: self.sync_admin_token.or(other.sync_admin_token),
            sync_id_file: self.sync_id_file.or(other.sync_id_file),
            sync_reset_id: self.sync_reset_id || other.sync_reset_id,
            sync_terminate_when_complete: self.sync_terminate_when_complete
                || other.sync_terminate_when_complete,

            // Template
            template_stratum_endpoint: self
                .template_stratum_endpoint
                .or(other.template_stratum_endpoint),
            template_username: self.template_username.or(other.template_username),
            template_password: self.template_password.or(other.template_password),
            template_watch: self.template_watch || other.template_watch,

            // Ping
            ping_stratum_endpoint: self.ping_stratum_endpoint.or(other.ping_stratum_endpoint),
            ping_username: self.ping_username.or(other.ping_username),
            ping_password: self.ping_password.or(other.ping_password),
            ping_count: self.ping_count.or(other.ping_count),
            ping_timeout: self.ping_timeout.or(other.ping_timeout),
        }
    }

    fn or_defaults(self) -> Result<Self> {
        let chain = self.chain.unwrap_or_default();

        let bitcoin_data_dir = match &self.bitcoin_data_dir {
            Some(dir) => dir.clone(),
            None => {
                if cfg!(target_os = "linux") {
                    dirs::home_dir()
                        .ok_or_else(|| {
                            anyhow!("failed to get bitcoin data dir: could not get home dir")
                        })?
                        .join(".bitcoin")
                } else {
                    dirs::data_dir()
                        .ok_or_else(|| {
                            anyhow!("failed to get bitcoin data dir: could not get data dir")
                        })?
                        .join("Bitcoin")
                }
            }
        };

        let data_dir = match &self.data_dir {
            Some(dir) => dir.clone(),
            None => dirs::data_dir()
                .ok_or_else(|| anyhow!("could not get data dir"))?
                .join("para"),
        };

        let cookie_file = match &self.bitcoin_rpc_cookie_file {
            Some(path) => path.clone(),
            None => chain.join_with_data_dir(&bitcoin_data_dir).join(".cookie"),
        };

        Ok(Self {
            chain: Some(chain),
            data_dir: Some(data_dir),
            config: None,
            config_dir: None,
            bitcoin_data_dir: Some(bitcoin_data_dir),
            bitcoin_rpc_port: Some(
                self.bitcoin_rpc_port
                    .unwrap_or_else(|| chain.default_rpc_port()),
            ),
            bitcoin_rpc_username: self.bitcoin_rpc_username,
            bitcoin_rpc_password: self.bitcoin_rpc_password,
            bitcoin_rpc_cookie_file: Some(cookie_file),

            // Pool defaults
            pool_address: Some(self.pool_address.unwrap_or_else(|| "0.0.0.0".into())),
            pool_port: Some(self.pool_port.unwrap_or(42069)),
            pool_update_interval: Some(self.pool_update_interval.unwrap_or(10)),
            pool_version_mask: Some(self.pool_version_mask.unwrap_or_else(|| "1fffe000".into())),
            pool_start_diff: Some(self.pool_start_diff.unwrap_or_else(|| "1".into())),
            pool_vardiff_period: Some(self.pool_vardiff_period.unwrap_or(5.0)),
            pool_vardiff_window: Some(self.pool_vardiff_window.unwrap_or(300.0)),
            pool_zmq_block_notifications: Some(
                self.pool_zmq_block_notifications
                    .unwrap_or_else(|| "tcp://127.0.0.1:28332".into()),
            ),

            // Server defaults
            server_address: Some(self.server_address.unwrap_or_else(|| "0.0.0.0".into())),
            server_port: self.server_port,
            server_admin_token: self.server_admin_token,
            server_api_token: self.server_api_token,
            server_acme_domain: self.server_acme_domain,
            server_acme_contact: self.server_acme_contact,
            server_alerts_ntfy_channel: self.server_alerts_ntfy_channel,
            server_database_url: Some(
                self.server_database_url
                    .unwrap_or_else(|| "postgres://satoshi:nakamoto@127.0.0.1:5432/ckpool".into()),
            ),
            server_log_dir: self.server_log_dir,
            server_nodes: self.server_nodes,
            server_sync_endpoint: self.server_sync_endpoint,
            server_ttl: Some(self.server_ttl.unwrap_or(30)),
            server_migrate_accounts: self.server_migrate_accounts,

            // Miner defaults
            miner_stratum_endpoint: self.miner_stratum_endpoint,
            miner_username: self.miner_username,
            miner_password: self.miner_password,
            miner_mode: Some(self.miner_mode.unwrap_or_else(|| "continuous".into())),
            miner_cpu_cores: self.miner_cpu_cores,
            miner_throttle: self.miner_throttle,

            // Sync defaults
            sync_endpoint: Some(
                self.sync_endpoint
                    .unwrap_or_else(|| "http://127.0.0.1:8080".into()),
            ),
            sync_batch_size: Some(self.sync_batch_size.unwrap_or(1_000_000)),
            sync_database_url: Some(
                self.sync_database_url
                    .unwrap_or_else(|| "postgres://satoshi:nakamoto@127.0.0.1:5432/ckpool".into()),
            ),
            sync_admin_token: self.sync_admin_token,
            sync_id_file: Some(self.sync_id_file.unwrap_or_else(|| "current_id.txt".into())),
            sync_reset_id: self.sync_reset_id,
            sync_terminate_when_complete: self.sync_terminate_when_complete,

            // Template defaults
            template_stratum_endpoint: self.template_stratum_endpoint,
            template_username: self.template_username,
            template_password: self.template_password,
            template_watch: self.template_watch,

            // Ping defaults
            ping_stratum_endpoint: self.ping_stratum_endpoint,
            ping_username: self.ping_username,
            ping_password: self.ping_password,
            ping_count: self.ping_count,
            ping_timeout: Some(self.ping_timeout.unwrap_or(5)),
        })
    }

    fn validate(settings: &Self) -> Result<()> {
        // Validate RPC username/password pairs
        match (
            &settings.bitcoin_rpc_username,
            &settings.bitcoin_rpc_password,
        ) {
            (None, Some(_)) => bail!("bitcoin RPC username specified without password"),
            (Some(_), None) => bail!("bitcoin RPC password specified without username"),
            _ => {}
        }

        Ok(())
    }

    // Convenience accessors
    pub fn chain(&self) -> Chain {
        self.chain.unwrap_or_default()
    }

    pub fn bitcoin_rpc_port(&self) -> u16 {
        self.bitcoin_rpc_port
            .unwrap_or_else(|| self.chain().default_rpc_port())
    }

    pub fn bitcoin_rpc_url(&self) -> String {
        format!("127.0.0.1:{}/", self.bitcoin_rpc_port())
    }

    pub fn bitcoin_credentials(&self) -> Result<Auth> {
        if let Some((user, pass)) = self
            .bitcoin_rpc_username
            .as_ref()
            .zip(self.bitcoin_rpc_password.as_ref())
        {
            Ok(Auth::UserPass(user.clone(), pass.clone()))
        } else {
            Ok(Auth::CookieFile(self.cookie_file()?))
        }
    }

    pub fn cookie_file(&self) -> Result<PathBuf> {
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

    pub fn data_dir(&self) -> PathBuf {
        self.data_dir.clone().unwrap_or_default()
    }

    pub fn bitcoin_rpc_client(&self) -> Result<bitcoincore_rpc::Client> {
        let rpc_url = self.bitcoin_rpc_url();
        let credentials = self.bitcoin_credentials()?;

        info!("Connecting to Bitcoin Core at {rpc_url}");

        let client = bitcoincore_rpc::Client::new(&rpc_url, credentials.clone()).map_err(|_| {
            anyhow!(
                "failed to connect to Bitcoin Core RPC at `{rpc_url}` with {}",
                match &credentials {
                    Auth::None => "no credentials".into(),
                    Auth::UserPass(_, _) => "username and password".into(),
                    Auth::CookieFile(path) => format!("cookie file at {}", path.display()),
                }
            )
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
                    bail!("Failed to connect to Bitcoin Core RPC at `{rpc_url}`: {err}")
                }
            }

            ensure!(
                checks < 100,
                "Failed to connect to Bitcoin Core RPC at `{rpc_url}`",
            );

            checks += 1;
            thread::sleep(Duration::from_millis(100));
        };

        let para_chain = self.chain();

        if rpc_chain != para_chain {
            bail!("Bitcoin RPC server is on {rpc_chain} but para is on {para_chain}");
        }

        Ok(client)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_options() -> crate::options::Options {
        crate::options::Options::default()
    }

    #[test]
    fn settings_from_empty_env() {
        let settings = Settings::from_env(&BTreeMap::new()).unwrap();
        assert!(settings.chain.is_none());
    }

    #[test]
    fn settings_from_env_chain() {
        let mut env = BTreeMap::new();
        env.insert("CHAIN".into(), "signet".into());
        let settings = Settings::from_env(&env).unwrap();
        assert_eq!(settings.chain, Some(Chain::Signet));
    }

    #[test]
    fn settings_from_env_bitcoin_rpc() {
        let mut env = BTreeMap::new();
        env.insert("BITCOIN_RPC_PORT".into(), "18443".into());
        env.insert("BITCOIN_RPC_USERNAME".into(), "user".into());
        env.insert("BITCOIN_RPC_PASSWORD".into(), "pass".into());
        let settings = Settings::from_env(&env).unwrap();
        assert_eq!(settings.bitcoin_rpc_port, Some(18443));
        assert_eq!(settings.bitcoin_rpc_username, Some("user".into()));
        assert_eq!(settings.bitcoin_rpc_password, Some("pass".into()));
    }

    #[test]
    fn settings_merge_priority() {
        let high = Settings {
            chain: Some(Chain::Signet),
            ..Default::default()
        };
        let low = Settings {
            chain: Some(Chain::Mainnet),
            bitcoin_rpc_port: Some(8332),
            ..Default::default()
        };
        let merged = high.or(low);
        assert_eq!(merged.chain, Some(Chain::Signet));
        assert_eq!(merged.bitcoin_rpc_port, Some(8332));
    }

    #[test]
    fn settings_boolean_merge_uses_or() {
        let a = Settings {
            server_migrate_accounts: true,
            ..Default::default()
        };
        let b = Settings {
            server_migrate_accounts: false,
            ..Default::default()
        };
        assert!(a.clone().or(b.clone()).server_migrate_accounts);
        assert!(b.or(a).server_migrate_accounts);
    }

    #[test]
    fn config_file_parsing() {
        let toml = r#"
            chain = "signet"
            bitcoin_rpc_port = 38332

            [pool]
            port = 42069
            start_diff = "0.001"

            [server]
            database_url = "postgres://test@localhost/test"
        "#;

        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.chain, Some(Chain::Signet));
        assert_eq!(config.bitcoin_rpc_port, Some(38332));
        assert_eq!(config.pool.as_ref().unwrap().port, Some(42069));
        assert_eq!(
            config.pool.as_ref().unwrap().start_diff,
            Some("0.001".into())
        );
        assert_eq!(
            config.server.as_ref().unwrap().database_url,
            Some("postgres://test@localhost/test".into())
        );
    }

    #[test]
    fn config_subcommand_overrides_global() {
        let toml = r#"
            chain = "mainnet"

            [pool]
            chain = "signet"
        "#;

        let config: Config = toml::from_str(toml).unwrap();
        let settings = Settings::from_config(&config);
        // Pool chain should override global
        assert_eq!(settings.chain, Some(Chain::Signet));
    }

    #[test]
    fn default_chain_is_mainnet() {
        let settings = Settings::merge(default_options(), BTreeMap::new()).unwrap();
        assert_eq!(settings.chain(), Chain::Mainnet);
    }

    #[test]
    fn rpc_username_without_password_fails() {
        let settings = Settings {
            bitcoin_rpc_username: Some("user".into()),
            bitcoin_rpc_password: None,
            ..Default::default()
        };
        assert!(Settings::validate(&settings).is_err());
    }

    #[test]
    fn rpc_password_without_username_fails() {
        let settings = Settings {
            bitcoin_rpc_username: None,
            bitcoin_rpc_password: Some("pass".into()),
            ..Default::default()
        };
        assert!(Settings::validate(&settings).is_err());
    }
}
