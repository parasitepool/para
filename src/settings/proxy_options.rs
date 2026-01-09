use super::*;

#[derive(Clone, Debug, Parser)]
pub(crate) struct ProxyOptions {
    #[arg(help = "Upstream stratum pool <HOST:PORT>.")]
    pub(crate) upstream: String,

    #[arg(long, help = "Username/address for upstream authentication.")]
    pub(crate) username: Username,

    #[arg(long, help = "Password for upstream authentication.")]
    pub(crate) password: Option<String>,

    #[arg(long, help = "Listen at <ADDRESS> for downstream miners.")]
    pub(crate) address: Option<String>,

    #[arg(long, help = "Listen on <PORT> for downstream miners.")]
    pub(crate) port: Option<u16>,

    #[arg(long, help = "Enable HTTP API on <API_PORT>. Disabled if not set.")]
    pub(crate) api_port: Option<u16>,

    #[arg(long, help = "Upstream connection timeout in seconds.")]
    pub(crate) timeout: Option<u64>,
}
