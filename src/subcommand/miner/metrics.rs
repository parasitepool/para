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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_metrics_starts_at_zero() {
        let metrics = Metrics::new();
        assert_eq!(metrics.total_hashes(), 0);
        assert_eq!(metrics.total_shares(), 0);
    }

    #[test]
    fn hash_count_increments() {
        let metrics = Metrics::new();
        metrics.add_hashes(1000);
        metrics.add_hashes(500);
        assert_eq!(metrics.total_hashes(), 1500);
    }

    #[test]
    fn share_count_increments() {
        let metrics = Metrics::new();
        metrics.add_share();
        metrics.add_share();
        assert_eq!(metrics.total_shares(), 2);
    }

    #[test]
    fn avg_hashrate_zero_hashes_returns_zero() {
        let metrics = Metrics::new();
        thread::sleep(std::time::Duration::from_millis(10));
        let rate = metrics.avg_hashrate();
        assert_eq!(rate, 0.0);
    }

    #[test]
    fn avg_hashrate_is_finite() {
        let metrics = Metrics::new();
        let rate = metrics.avg_hashrate();
        assert!(rate.is_finite(), "hashrate should be finite: {rate}");
    }

    #[test]
    fn avg_hashrate_increases_with_hashes() {
        let metrics = Metrics::new();
        thread::sleep(std::time::Duration::from_millis(10));
        metrics.add_hashes(10_000);

        let rate = metrics.avg_hashrate();
        assert!(rate > 0.0, "hashrate should be positive: {rate}");
        assert!(rate.is_finite(), "hashrate should be finite: {rate}");
    }

    #[test]
    fn status_line_contains_all_fields() {
        let metrics = Metrics::new();
        metrics.add_hashes(1000);
        metrics.add_share();

        let line = metrics.status_line();
        assert!(line.contains("hashrate="), "missing hashrate: {line}");
        assert!(line.contains("H/s"), "missing H/s unit: {line}");
        assert!(line.contains("shares=1"), "missing shares: {line}");
        assert!(line.contains("uptime="), "missing uptime: {line}");
    }

    #[test]
    fn status_line_format_is_stable() {
        let metrics = Metrics::new();
        let line = metrics.status_line();

        assert!(
            line.contains("hashrate=") && line.contains("shares=0") && line.contains("uptime="),
            "unexpected format: {line}"
        );
    }
}
