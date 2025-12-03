use {super::*, parking_lot::Mutex};

struct HashRate {
    last_total: u64,
    smoothed: DecayingAverage,
}

pub(crate) struct Metrics {
    hashes: AtomicU64,
    shares: AtomicU64,
    started: Instant,
    hash_rate: Mutex<HashRate>,
}

impl Metrics {
    pub(crate) fn new() -> Self {
        Self {
            hashes: AtomicU64::new(0),
            shares: AtomicU64::new(0),
            started: Instant::now(),
            hash_rate: Mutex::new(HashRate {
                last_total: 0,
                smoothed: DecayingAverage::new(Duration::from_secs(5)),
            }),
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

    pub(crate) fn hash_rate(&self) -> f64 {
        let mut state = self.hash_rate.lock();
        let total = self.total_hashes();
        let delta = total.saturating_sub(state.last_total);
        state.smoothed.record(delta as f64, Instant::now());
        state.last_total = total;
        state.smoothed.value()
    }
}

impl StatusLine for Metrics {
    fn status_line(&self) -> String {
        format!(
            "hashrate={}H/s  shares={}  uptime={}s",
            ckpool::HashRate(self.hash_rate()),
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
        thread::sleep(Duration::from_millis(10));
        let rate = metrics.hash_rate();
        assert_eq!(rate, 0.0);
    }

    #[test]
    fn avg_hashrate_is_finite() {
        let metrics = Metrics::new();
        let rate = metrics.hash_rate();
        assert!(rate.is_finite(), "hashrate should be finite: {rate}");
    }

    #[test]
    fn avg_hashrate_increases_with_hashes() {
        let metrics = Metrics::new();
        thread::sleep(Duration::from_millis(50));
        metrics.add_hashes(100_000);

        // First call samples the delta
        let rate = metrics.hash_rate();
        assert!(rate > 0.0, "hashrate should be positive: {rate}");
        assert!(rate.is_finite(), "hashrate should be finite: {rate}");
    }

    #[test]
    fn avg_hashrate_smooths_over_samples() {
        let metrics = Metrics::new();

        // Simulate periodic sampling like the throbber does
        for _ in 0..5 {
            thread::sleep(Duration::from_millis(50));
            metrics.add_hashes(10_000);
            let _ = metrics.hash_rate(); // Sample
        }

        let rate = metrics.hash_rate();
        assert!(rate > 0.0, "smoothed rate should be positive: {rate}");
        assert!(rate.is_finite(), "smoothed rate should be finite: {rate}");
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
