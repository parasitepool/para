use {super::*, parking_lot::Mutex};

pub(crate) struct Metatron {
    blocks: AtomicU64,
    accepted: AtomicU64,
    rejected: AtomicU64,
    dsps: Mutex<DecayingAverage>,
    started: Instant,
    workers: AtomicU64,
}

impl Metatron {
    pub(crate) fn new(window: Duration) -> Self {
        Self {
            blocks: AtomicU64::new(0),
            accepted: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
            dsps: Mutex::new(DecayingAverage::new(window)),
            started: Instant::now(),
            workers: AtomicU64::new(0),
        }
    }

    pub(crate) fn add_block(&self) {
        self.blocks.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn add_accepted(&self, difficulty: f64) {
        self.accepted.fetch_add(1, Ordering::Relaxed);
        self.dsps.lock().record(difficulty, Instant::now());
    }

    pub(crate) fn add_rejected(&self) {
        self.rejected.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn hash_rate(&self) -> HashRate {
        HashRate::from_dsps(self.dsps.lock().value())
    }

    pub(crate) fn add_worker(&self) {
        self.workers.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn sub_worker(&self) {
        self.workers.fetch_sub(1, Ordering::Relaxed);
    }

    pub(crate) fn total_blocks(&self) -> u64 {
        self.blocks.load(Ordering::Relaxed)
    }

    pub(crate) fn accepted(&self) -> u64 {
        self.accepted.load(Ordering::Relaxed)
    }

    pub(crate) fn rejected(&self) -> u64 {
        self.rejected.load(Ordering::Relaxed)
    }

    pub(crate) fn total_workers(&self) -> u64 {
        self.workers.load(Ordering::Relaxed)
    }

    pub(crate) fn uptime(&self) -> Duration {
        self.started.elapsed()
    }
}

impl StatusLine for Metatron {
    fn status_line(&self) -> String {
        format!(
            "hashrate={}  workers={}  accepted={}  rejected={}  blocks={}  uptime={}s",
            self.hash_rate(),
            self.total_workers(),
            self.accepted(),
            self.rejected(),
            self.total_blocks(),
            self.uptime().as_secs()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn new_metatron_starts_at_zero() {
        let metatron = Metatron::new(secs(300));
        assert_eq!(metatron.total_workers(), 0);
        assert_eq!(metatron.accepted(), 0);
        assert_eq!(metatron.rejected(), 0);
        assert_eq!(metatron.total_blocks(), 0);
    }

    #[test]
    fn worker_count_increments_and_decrements() {
        let metatron = Metatron::new(secs(300));
        assert_eq!(metatron.total_workers(), 0);

        metatron.add_worker();
        metatron.add_worker();
        assert_eq!(metatron.total_workers(), 2);

        metatron.sub_worker();
        assert_eq!(metatron.total_workers(), 1);

        metatron.sub_worker();
        assert_eq!(metatron.total_workers(), 0);
    }

    #[test]
    fn accepted_count_increments() {
        let metatron = Metatron::new(secs(300));
        metatron.add_accepted(10.0);
        metatron.add_accepted(20.0);
        metatron.add_accepted(30.0);
        assert_eq!(metatron.accepted(), 3);
    }

    #[test]
    fn rejected_count_increments() {
        let metatron = Metatron::new(secs(300));
        metatron.add_rejected();
        metatron.add_rejected();
        assert_eq!(metatron.rejected(), 2);
    }

    #[test]
    fn block_count_increments() {
        let metatron = Metatron::new(secs(300));
        metatron.add_block();
        assert_eq!(metatron.total_blocks(), 1);
    }

    #[test]
    fn status_line_contains_all_fields() {
        let metatron = Metatron::new(secs(300));
        metatron.add_worker();
        metatron.add_worker();
        metatron.add_accepted(10.0);
        metatron.add_accepted(10.0);
        metatron.add_accepted(10.0);
        metatron.add_rejected();
        metatron.add_block();

        let line = metatron.status_line();
        assert!(line.contains("hashrate="), "missing hashrate: {line}");
        assert!(line.contains("workers=2"), "missing workers: {line}");
        assert!(line.contains("accepted=3"), "missing accepted: {line}");
        assert!(line.contains("rejected=1"), "missing rejected: {line}");
        assert!(line.contains("blocks=1"), "missing blocks: {line}");
        assert!(line.contains("uptime="), "missing uptime: {line}");
    }

    #[test]
    fn status_line_format_is_stable() {
        let metatron = Metatron::new(secs(300));
        let line = metatron.status_line();
        assert!(
            line.starts_with(
                "hashrate=0 H/s  workers=0  accepted=0  rejected=0  blocks=0  uptime="
            ),
            "unexpected format: {line}"
        );
    }

    #[test]
    fn hash_rate_accumulates() {
        let metatron = Metatron::new(secs(300));
        metatron.add_accepted(100.0);
        metatron.add_accepted(100.0);

        let rate = metatron.hash_rate();
        assert!(rate.0 > 0.0, "hashrate should be positive: {}", rate);
    }
}
