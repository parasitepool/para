use super::*;

#[derive(Clone, Debug, Parser)]
pub(crate) struct RouterOptions {
    #[command(flatten)]
    pub(crate) common: CommonOptions,

    #[arg(long, help = "Wallet external <DESCRIPTOR>.")]
    pub(crate) descriptor: String,

    #[arg(long, help = "Wallet internal <CHANGE_DESCRIPTOR>.")]
    pub(crate) change_descriptor: Option<String>,

    #[arg(long, default_value_t = 0, help = "Block height <WALLET_BIRTHDAY>.")]
    pub(crate) wallet_birthday: u32,

    #[arg(
        long,
        default_value_t = 30,
        help = "Upstream connection <TIMEOUT> in seconds."
    )]
    pub(crate) timeout: u64,

    #[arg(
        long,
        default_value_t = ENONCE1_EXTENSION_SIZE,
        help = "Extend upstream enonce1 by <ENONCE1_EXTENSION_SIZE> bytes."
    )]
    pub(crate) enonce1_extension_size: usize,

    #[arg(long, default_value_t = 60, help = "<TICK_INTERVAL> in seconds.")]
    pub(crate) tick_interval: u64,

    #[arg(
        long,
        help = "Sink order with upstream target <USER[:PASS]@HOST:PORT>."
    )]
    pub(crate) sink_order: Vec<UpstreamTarget>,

    #[arg(long, help = "Start halted, rejecting new bucket orders.")]
    pub(crate) halt: bool,

    #[arg(long, help = "Direct all hashrate to bucket orders.")]
    pub(crate) boost: bool,

    #[arg(
        long,
        default_value_t = 1e18,
        help = "Total <CAPACITY_WORK> in hash days."
    )]
    pub(crate) capacity_work: f64,
}
