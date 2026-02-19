use super::*;

#[derive(Clone, Debug, Parser)]
pub(crate) struct ProxyOptions {
    #[command(flatten)]
    pub(crate) common: CommonOptions,

    #[arg(long, help = "Upstream stratum endpoint <HOST:PORT>.")]
    pub(crate) upstream: String,

    #[arg(long, help = "Username for upstream authentication.")]
    pub(crate) username: Username,

    #[arg(long, help = "Password for upstream authentication.")]
    pub(crate) password: Option<String>,

    #[arg(
        long,
        default_value_t = 30,
        help = "Upstream connection timeout in seconds."
    )]
    pub(crate) timeout: u64,

    #[arg(
        long,
        default_value_t = ENONCE1_EXTENSION_SIZE,
        help = "Extend upstream enonce1 by <ENONCE1_EXTENSION_SIZE> bytes."
    )]
    pub(crate) enonce1_extension_size: usize,
}
