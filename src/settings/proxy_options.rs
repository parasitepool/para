use super::*;

#[derive(Clone, Debug, Parser)]
pub(crate) struct ProxyOptions {
    #[arg(long, help = "Upstream stratum endpoint <HOST:PORT>.")]
    pub(crate) upstream: String,

    #[arg(long, help = "Run on <CHAIN>.")]
    pub(crate) chain: Option<Chain>,

    #[arg(long, help = "Username for upstream authentication.")]
    pub(crate) username: Username,

    #[arg(long, help = "Password for upstream authentication.")]
    pub(crate) password: Option<String>,

    #[arg(long, help = "Listen at <ADDRESS> for downstream miners.")]
    pub(crate) address: Option<String>,

    #[arg(long, help = "Listen on <PORT> for downstream miners.")]
    pub(crate) port: Option<u16>,

    #[arg(long, help = "Enable HTTP API on <HTTP_PORT>. Disabled if not set.")]
    pub(crate) http_port: Option<u16>,

    #[arg(long, help = "Upstream connection timeout in seconds.")]
    pub(crate) timeout: Option<u64>,

    #[arg(long, help = "Load Bitcoin Core data dir from <BITCOIN_DATA_DIR>.")]
    pub(crate) bitcoin_data_dir: Option<PathBuf>,

    #[arg(long, help = "Connect to Bitcoin Core RPC at <BITCOIN_RPC_PORT>.")]
    pub(crate) bitcoin_rpc_port: Option<u16>,

    #[arg(long, help = "Load Bitcoin Core RPC cookie file from <COOKIE_FILE>.")]
    pub(crate) bitcoin_rpc_cookie_file: Option<PathBuf>,

    #[arg(
        long,
        help = "Authenticate to Bitcoin Core RPC as <BITCOIN_RPC_USERNAME>."
    )]
    pub(crate) bitcoin_rpc_username: Option<String>,

    #[arg(
        long,
        help = "Authenticate to Bitcoin Core RPC with <BITCOIN_RPC_PASSWORD>."
    )]
    pub(crate) bitcoin_rpc_password: Option<String>,

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

    #[arg(long, help = "ACME domain for TLS certificate.")]
    pub(crate) acme_domain: Vec<String>,

    #[arg(long, help = "ACME contact email for TLS certificate.")]
    pub(crate) acme_contact: Vec<String>,

    #[arg(long, help = "ACME cache directory.")]
    pub(crate) acme_cache: Option<PathBuf>,
}
