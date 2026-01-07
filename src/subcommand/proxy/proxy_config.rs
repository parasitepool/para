use super::*;

#[derive(Clone, Debug, Parser)]
pub(crate) struct ProxyConfig {
    #[arg(help = "Upstream stratum pool <HOST:PORT>.")]
    upstream: String,
    #[arg(long, help = "Username/address for upstream authentication.")]
    username: Username,
    #[arg(long, help = "Password for upstream authentication.")]
    password: Option<String>,
    #[arg(long, help = "Listen at <ADDRESS> for downstream miners.")]
    address: Option<String>,
    #[arg(long, help = "Listen on <PORT> for downstream miners.")]
    port: Option<u16>,
    #[arg(long, help = "Enable HTTP API on <API_PORT>. Disabled if not set.")]
    api_port: Option<u16>,
    #[arg(
        long,
        help = "Upstream connection timeout in seconds.",
        default_value = "30"
    )]
    timeout: u64,
}

impl ProxyConfig {
    pub(crate) fn upstream(&self) -> &str {
        &self.upstream
    }

    pub(crate) fn username(&self) -> Username {
        self.username.clone()
    }

    pub(crate) fn password(&self) -> Option<String> {
        self.password.clone()
    }

    pub(crate) fn address(&self) -> String {
        self.address.clone().unwrap_or_else(|| "0.0.0.0".into())
    }

    pub(crate) fn port(&self) -> u16 {
        self.port.unwrap_or(42069)
    }

    pub(crate) fn api_port(&self) -> Option<u16> {
        self.api_port
    }

    pub(crate) fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout)
    }
}
