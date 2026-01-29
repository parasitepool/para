use super::*;

#[derive(Clone, Debug, Parser)]
pub(crate) struct PoolOptions {
    #[arg(long, help = "Listen at <ADDRESS>.")]
    pub(crate) address: Option<String>,

    #[arg(long, help = "Listen on <PORT>.")]
    pub(crate) port: Option<u16>,

    #[arg(long, help = "Enable HTTP API on <HTTP_PORT>. Disabled if not set.")]
    pub(crate) http_port: Option<u16>,

    #[arg(long, help = "ACME domain for TLS certificate.")]
    pub(crate) acme_domain: Vec<String>,

    #[arg(long, help = "ACME contact email for TLS certificate.")]
    pub(crate) acme_contact: Vec<String>,

    #[arg(long, help = "ACME cache directory.")]
    pub(crate) acme_cache: Option<PathBuf>,

    #[arg(long, help = "Load Bitcoin Core data dir from <BITCOIN_DATA_DIR>.")]
    pub(crate) bitcoin_data_dir: Option<PathBuf>,

    #[arg(
        long,
        help = "Authenticate to Bitcoin Core RPC with <BITCOIN_RPC_PASSWORD>."
    )]
    pub(crate) bitcoin_rpc_password: Option<String>,

    #[arg(long, help = "Connect to Bitcoin Core RPC at <BITCOIN_RPC_PORT>.")]
    pub(crate) bitcoin_rpc_port: Option<u16>,

    #[arg(
        long,
        help = "Authenticate to Bitcoin Core RPC as <BITCOIN_RPC_USERNAME>."
    )]
    pub(crate) bitcoin_rpc_username: Option<String>,

    #[arg(long, help = "Load Bitcoin Core RPC cookie file from <COOKIE_FILE>.")]
    pub(crate) bitcoin_rpc_cookie_file: Option<PathBuf>,

    #[arg(long, help = "Block template update interval in seconds.")]
    pub(crate) update_interval: Option<u64>,

    #[arg(long, help = "Run on <CHAIN>.")]
    pub(crate) chain: Option<Chain>,

    #[arg(long, alias = "datadir", help = "Store data in <DATA_DIR>.")]
    pub(crate) data_dir: Option<PathBuf>,

    #[arg(long, help = "Use version rolling with <VERSION_MASK>.")]
    pub(crate) version_mask: Option<Version>,

    #[arg(long, help = "Give <START_DIFF> to new clients.")]
    pub(crate) start_diff: Option<Difficulty>,

    #[arg(long, help = "Minimum difficulty for vardiff.")]
    pub(crate) min_diff: Option<Difficulty>,

    #[arg(long, help = "Maximum difficulty for vardiff.")]
    pub(crate) max_diff: Option<Difficulty>,

    #[arg(
        long,
        help = "Target <VARDIFF_PERIOD> seconds between share submissions."
    )]
    pub(crate) vardiff_period: Option<f64>,

    #[arg(
        long,
        help = "Average the share submission rate over <VARDIFF_WINDOW> seconds."
    )]
    pub(crate) vardiff_window: Option<f64>,

    #[arg(long, help = "Subscribe to <ZMQ_BLOCK_NOTIFICATIONS>.")]
    pub(crate) zmq_block_notifications: Option<Endpoint>,

    #[arg(long, help = "Set enonce1 size in bytes (2-8).")]
    pub(crate) enonce1_size: Option<usize>,

    #[arg(long, help = "Set enonce2 size in bytes (2-8).")]
    pub(crate) enonce2_size: Option<usize>,

    #[arg(long, help = "Disable bouncer.")]
    pub(crate) disable_bouncer: bool,

    #[arg(
        long,
        value_parser = validate_database_url,
        help = "Connect to Postgres at <DATABASE_URL> for event storage."
    )]
    pub(crate) database_url: Option<String>,
    #[arg(
        long,
        value_parser = validate_events_file,
        help = "Write events to JSON or CSV <EVENTS_FILE>."
    )]
    pub(crate) events_file: Option<PathBuf>,
}

fn validate_events_file(s: &str) -> Result<PathBuf> {
    let path = PathBuf::from(s);
    let ext = path.extension().and_then(|e| e.to_str());
    ensure!(
        matches!(ext, Some("json") | Some("csv")),
        "Events file must have .json or .csv extension"
    );
    Ok(path)
}

fn validate_database_url(s: &str) -> anyhow::Result<String> {
    ensure!(
        s.starts_with("postgres://") || s.starts_with("postgresql://"),
        "Database URL must start with postgres:// or postgresql://"
    );
    Ok(s.to_string())
}
