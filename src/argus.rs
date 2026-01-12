use super::*;

pub(crate) struct Argus {
    pub(crate) nexus: Arc<Nexus>,
    pub(crate) metatron: Arc<Metatron>,
}

impl StatusLine for Argus {
    fn status_line(&self) -> String {
        format!(
            "sps={:.2}  hash_rate={}  connections={}  upstream_enonce1={}  upstream_accepted={}  upstream_rejected={}",
            self.metatron.sps_1m(),
            self.metatron.hash_rate_1m(),
            self.metatron.total_connections(),
            self.nexus.enonce1(),
            self.nexus.upstream_accepted(),
            self.nexus.upstream_rejected(),
        )
    }
}
