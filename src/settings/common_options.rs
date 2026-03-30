use super::*;

#[derive(Clone, Debug, Args)]
pub(crate) struct CommonOptions {
    #[arg(
        long,
        default_value = "0.0.0.0",
        help = "Listen for stratum messages at <ADDRESS>."
    )]
    pub(crate) address: String,

    #[arg(
        long,
        default_value_t = 42069,
        help = "Listen for stratum messages on port <PORT>."
    )]
    pub(crate) port: u16,

    #[arg(
        long,
        help = "Listen for stratum messages on high diff port <HIGH_DIFF_PORT> with initial difficulty 1000000."
    )]
    pub(crate) high_diff_port: Option<u16>,

    #[arg(long, help = "Enable HTTP API on <HTTP_PORT>. Disabled if not set.")]
    pub(crate) http_port: Option<u16>,

    #[command(flatten)]
    pub(crate) bitcoin: BitcoinOptions,

    #[arg(long, default_value_t, help = "Give <START_DIFF> to new clients.")]
    pub(crate) start_diff: Difficulty,

    #[arg(long, help = "Minimum difficulty for vardiff.")]
    pub(crate) min_diff: Option<Difficulty>,

    #[arg(long, help = "Maximum difficulty for vardiff.")]
    pub(crate) max_diff: Option<Difficulty>,

    #[arg(
        long,
        default_value_t = 3.33,
        help = "Target <VARDIFF_PERIOD> seconds between share submissions."
    )]
    pub(crate) vardiff_period: f64,

    #[arg(
        long,
        default_value_t = 300.0,
        help = "Average the share submission rate over <VARDIFF_WINDOW> seconds."
    )]
    pub(crate) vardiff_window: f64,

    #[arg(long, help = "Request ACME TLS certificate for <ACME_DOMAIN>.")]
    pub(crate) acme_domain: Vec<String>,

    #[arg(long, help = "Provide ACME contact <ACME_CONTACT>.")]
    pub(crate) acme_contact: Vec<String>,

    #[arg(
        long,
        default_value = "acme-cache",
        help = "Store ACME cache in <ACME_CACHE>."
    )]
    pub(crate) acme_cache: PathBuf,

    #[arg(long, alias = "datadir", help = "Store data in <DATA_DIR>.")]
    pub(crate) data_dir: Option<PathBuf>,
}
