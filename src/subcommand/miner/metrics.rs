use {super::*, crate::throbber::StatusLine};

pub(crate) struct Metrics {
    hashes: AtomicU64,
    shares: AtomicU64,
    started: Instant,
}

impl Metrics {
    pub(crate) fn new() -> Self {
        Self {
            hashes: AtomicU64::new(0),
            shares: AtomicU64::new(0),
            started: Instant::now(),
        }
    }

    pub(crate) fn add_hashes(&self, hashes: u64) {
        self.hashes.fetch_add(hashes, Ordering::Relaxed);
    }

    pub(crate) fn add_share(&self) {
        self.shares.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn total_hashes(&self) -> u64 {
        self.hashes.load(Ordering::Relaxed)
    }

    pub(crate) fn total_shares(&self) -> u64 {
        self.shares.load(Ordering::Relaxed)
    }

    pub(crate) fn uptime(&self) -> Duration {
        self.started.elapsed()
    }

    pub(crate) fn avg_hashrate(&self) -> f64 {
        let hashes = self.total_hashes() as f64;
        let secs = self.uptime().as_secs_f64();
        if secs > 0.0 { hashes / secs } else { 0.0 }
    }
}

impl StatusLine for Metrics {
    fn status_line(&self) -> String {
        format!(
            "hashrate={}H/s  shares={}  uptime={}s",
            ckpool::HashRate(self.avg_hashrate()),
            self.total_shares(),
            self.uptime().as_secs()
        )
    }
}
