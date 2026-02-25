use super::*;

#[derive(Clone, Debug, Parser)]
pub(crate) struct ProxyOptions {
    #[command(flatten)]
    pub(crate) common: CommonOptions,

    #[arg(long, help = "Upstream <USER[:PASS]@HOST:PORT>.")]
    pub(crate) upstream: UpstreamTarget,

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
