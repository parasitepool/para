use super::*;

pub(crate) struct Metrics {
    upstream: RwLock<Arc<Upstream>>,
    pub(crate) metatron: Arc<Metatron>,
}

impl Metrics {
    pub(crate) fn new(upstream: Arc<Upstream>, metatron: Arc<Metatron>) -> Self {
        Self {
            upstream: RwLock::new(upstream),
            metatron,
        }
    }

    pub(crate) fn upstream(&self) -> Arc<Upstream> {
        self.upstream.read().clone()
    }

    pub(crate) fn update_upstream(&self, upstream: Arc<Upstream>) {
        *self.upstream.write() = upstream;
    }
}

impl StatusLine for Metrics {
    fn status_line(&self) -> String {
        let now = Instant::now();
        let upstream = self.upstream();
        let stats = self.metatron.snapshot();

        format!(
            "sps={:.2}  hashrate={:.2}  sessions={}  upstream_enonce1={}  accepted={}  rejected={}",
            stats.sps_1m(now),
            stats.hashrate_1m(now),
            self.metatron.total_sessions(),
            upstream.enonce1(),
            stats.accepted_shares,
            stats.rejected_shares,
        )
    }
}
