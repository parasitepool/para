use super::*;

#[derive(Clone, Debug, Parser)]
pub(crate) struct RouterOptions {
    #[command(flatten)]
    pub(crate) common: CommonOptions,

    #[arg(long, help = "Wallet external <DESCRIPTOR>.")]
    pub(crate) descriptor: String,

    #[arg(long, help = "Wallet internal (change) <DESCRIPTOR>.")]
    pub(crate) change_descriptor: Option<String>,

    #[arg(long, default_value_t = 0, help = "Wallet sync start block height.")]
    pub(crate) wallet_birthday: u32,

    #[arg(
        long,
        default_value_t = 3600,
        help = "Invoice payment timeout in seconds."
    )]
    pub(crate) invoice_timeout: u64,

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

    #[arg(long, default_value_t = 60, help = "Tick interval in seconds.")]
    pub(crate) tick_interval: u64,

    #[arg(
        long = "hashprice",
        value_parser = clap::value_parser!(u64).range(1..),
        help = "Price per petahash-day in sats."
    )]
    pub(crate) hash_price: u64,

    #[arg(long, help = "Default upstream <USER[:PASS]@HOST:PORT>.")]
    pub(crate) default_order: Vec<UpstreamTarget>,
}
