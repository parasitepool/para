use super::*;

pub(crate) struct Metrics {
    pub(crate) upstream: Arc<Upstream>,
    pub(crate) metatron: Arc<Metatron>,
}

impl StatusLine for Metrics {
    fn status_line(&self) -> String {
        format!(
            "sps={:.2}  hash_rate={}  connections={}  upstream_enonce1={}  upstream_accepted={}  upstream_rejected={}",
            self.metatron.sps_1m(),
            self.metatron.hash_rate_1m(),
            self.metatron.total_connections(),
            self.upstream.enonce1(),
            self.upstream.upstream_accepted(),
            self.upstream.upstream_rejected(),
        )
    }
}
