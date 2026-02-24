use super::*;

pub(crate) struct Metrics {
    pub(crate) upstream: Arc<Upstream>,
    pub(crate) metatron: Arc<Metatron>,
}

impl StatusLine for Metrics {
    fn status_line(&self) -> String {
        let now = Instant::now();
        let stats = self.metatron.snapshot();
        format!(
            "sps={:.2}  hashrate={:.2}  sessions={}  upstream_enonce1={}  accepted={}  rejected={}",
            stats.sps_1m(now),
            stats.hashrate_1m(now),
            self.metatron.total_sessions(),
            self.upstream.enonce1(),
            stats.accepted_shares,
            stats.rejected_shares,
        )
    }
}
