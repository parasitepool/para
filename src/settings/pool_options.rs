use super::*;

#[derive(Clone, Debug, Parser)]
pub(crate) struct PoolOptions {
    #[command(flatten)]
    pub(crate) common: CommonOptions,

    #[arg(
        long,
        default_value_t = 10,
        help = "Block template update interval in seconds."
    )]
    pub(crate) update_interval: u64,

    #[arg(
        long,
        default_value_t,
        help = "Use version rolling with <VERSION_MASK>."
    )]
    pub(crate) version_mask: Version,

    #[arg(
        long,
        default_value = "tcp://127.0.0.1:28332",
        help = "Subscribe to <ZMQ_BLOCK_NOTIFICATIONS>."
    )]
    pub(crate) zmq_block_notifications: Endpoint,

    #[arg(long, default_value_t = ENONCE1_SIZE, help = "Set enonce1 size in bytes (2-8).")]
    pub(crate) enonce1_size: usize,

    #[arg(long, default_value_t = MAX_ENONCE_SIZE, help = "Set enonce2 size in bytes (2-8).")]
    pub(crate) enonce2_size: usize,

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
