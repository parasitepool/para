use super::*;

pub(crate) struct Metrics {
    pub(crate) upstream: Arc<Upstream>,
    pub(crate) metatron: Arc<Metatron>,
}

impl StatusLine for Metrics {
    fn status_line(&self) -> String {
        format!(
            "sps={:.2}  hashrate={:.2}  sessions={}  upstream_enonce1={}  accepted={}  rejected={}",
            self.metatron.sps_1m(),
            self.metatron.hashrate_1m(),
            self.metatron.total_sessions(),
            self.upstream.enonce1(),
            self.metatron.accepted(),
            self.metatron.rejected(),
        )
    }
}
