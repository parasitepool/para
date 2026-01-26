use {super::*, parking_lot::Mutex};

struct HashRateCounter {
    last_total: u64,
    smoothed: DecayingAverage,
}

pub(crate) struct Metrics {
    hashes: AtomicU64,
    shares: AtomicU64,
    started: Instant,
    hashrate: Mutex<HashRateCounter>,
    sps: Mutex<DecayingAverage>,
}

impl Metrics {
    pub(crate) fn new() -> Self {
        Self {
            hashes: AtomicU64::new(0),
            shares: AtomicU64::new(0),
            started: Instant::now(),
            hashrate: Mutex::new(HashRateCounter {
                last_total: 0,
                smoothed: DecayingAverage::new(Duration::from_secs(5)),
            }),
            sps: Mutex::new(DecayingAverage::new(Duration::from_secs(10))),
        }
    }

    pub(crate) fn add_hashes(&self, hashes: u64) {
        self.hashes.fetch_add(hashes, Ordering::Relaxed);
    }

    pub(crate) fn add_share(&self) {
        self.shares.fetch_add(1, Ordering::Relaxed);
        self.sps.lock().record(1.0, Instant::now());
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

    pub(crate) fn hashrate(&self) -> HashRate {
        let mut state = self.hashrate.lock();
        let total = self.total_hashes();
        let delta = total.saturating_sub(state.last_total);
        let now = Instant::now();
        state.smoothed.record(delta as f64, now);
        state.last_total = total;
        HashRate(state.smoothed.value_at(now))
    }

    pub(crate) fn sps(&self) -> f64 {
        self.sps.lock().value_at(Instant::now())
    }
}

impl StatusLine for Metrics {
    fn status_line(&self) -> String {
        format!(
            "hashrate={:.2}  shares={} ({:.2}/s)  uptime={}s",
            self.hashrate(),
            self.total_shares(),
            self.sps(),
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
        let rate = metrics.hashrate();
        assert_eq!(rate, HashRate(0.0));
    }

    #[test]
    fn avg_hashrate_is_finite() {
        let metrics = Metrics::new();
        let rate = metrics.hashrate();
        assert!(rate.0.is_finite(), "hashrate should be finite: {rate}");
    }

    #[test]
    fn avg_hashrate_increases_with_hashes() {
        let metrics = Metrics::new();
        thread::sleep(Duration::from_millis(50));
        metrics.add_hashes(100_000);

        let rate = metrics.hashrate();
        assert!(rate > HashRate(0.0), "hashrate should be positive: {rate}");
        assert!(rate.0.is_finite(), "hashrate should be finite: {rate}");
    }

    #[test]
    fn avg_hashrate_smooths_over_samples() {
        let metrics = Metrics::new();

        for _ in 0..5 {
            thread::sleep(Duration::from_millis(50));
            metrics.add_hashes(10_000);
            metrics.hashrate();
        }

        let rate = metrics.hashrate();
        assert!(
            rate > HashRate(0.0),
            "smoothed rate should be positive: {rate}"
        );
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
        assert!(line.contains("/s)"), "missing sps: {line}");
        assert!(line.contains("uptime="), "missing uptime: {line}");
    }

    #[test]
    fn status_line_format_is_stable() {
        let metrics = Metrics::new();
        let line = metrics.status_line();

        assert!(
            line.contains("hashrate=")
                && line.contains("shares=0")
                && line.contains("/s)")
                && line.contains("uptime="),
            "unexpected format: {line}"
        );
    }
}
