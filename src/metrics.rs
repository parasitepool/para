use super::*;

pub(crate) struct Metrics {
    upstream: std::sync::RwLock<Arc<Upstream>>,
    pub(crate) metatron: Arc<Metatron>,
}

impl Metrics {
    pub(crate) fn new(upstream: Arc<Upstream>, metatron: Arc<Metatron>) -> Self {
        Self {
            upstream: std::sync::RwLock::new(upstream),
            metatron,
        }
    }

    pub(crate) fn upstream(&self) -> Arc<Upstream> {
        self.upstream.read().unwrap().clone()
    }

    pub(crate) fn update_upstream(&self, upstream: Arc<Upstream>) {
        *self.upstream.write().unwrap() = upstream;
    }
}

impl StatusLine for Metrics {
    fn status_line(&self) -> String {
        let upstream = self.upstream();
        format!(
            "sps={:.2}  hashrate={:.2}  sessions={}  upstream_enonce1={}  accepted={}  rejected={}",
            self.metatron.sps_1m(),
            self.metatron.hashrate_1m(),
            self.metatron.total_sessions(),
            upstream.enonce1(),
            self.metatron.accepted(),
            self.metatron.rejected(),
        )
    }
}
