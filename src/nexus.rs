use super::*;

pub(crate) struct Nexus {
    upstream_url: String,
    upstream_username: String,
    downstream_address: String,
    downstream_port: u16,
    connected: AtomicBool,
}

impl Nexus {
    pub(crate) fn new(
        upstream_url: String,
        upstream_username: String,
        downstream_address: String,
        downstream_port: u16,
    ) -> Self {
        Self {
            upstream_url,
            upstream_username,
            downstream_address,
            downstream_port,
            connected: AtomicBool::new(false),
        }
    }

    pub(crate) fn set_connected(&self, connected: bool) {
        self.connected.store(connected, Ordering::SeqCst);
    }

    pub(crate) fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    pub(crate) fn upstream_url(&self) -> &str {
        &self.upstream_url
    }

    pub(crate) fn upstream_username(&self) -> &str {
        &self.upstream_username
    }

    pub(crate) fn downstream_address(&self) -> &str {
        &self.downstream_address
    }

    pub(crate) fn downstream_port(&self) -> u16 {
        self.downstream_port
    }
}
