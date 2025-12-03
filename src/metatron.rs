use super::*;

pub(crate) struct Metatron {
    blocks: AtomicU64,
    shares: AtomicU64,
    started: Instant,
    workers: AtomicU64,
}

impl Metatron {
    pub(crate) fn new() -> Self {
        Self {
            blocks: AtomicU64::new(0),
            shares: AtomicU64::new(0),
            started: Instant::now(),
            workers: AtomicU64::new(0),
        }
    }

    pub(crate) fn add_block(&self) {
        self.blocks.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn add_share(&self) {
        self.shares.fetch_add(1, Ordering::Relaxed);
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

    pub(crate) fn total_shares(&self) -> u64 {
        self.shares.load(Ordering::Relaxed)
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
            "workers={}  shares={}  blocks={}  uptime={}s",
            self.total_workers(),
            self.total_shares(),
            self.total_blocks(),
            self.uptime().as_secs()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_metatron_starts_at_zero() {
        let metatron = Metatron::new();
        assert_eq!(metatron.total_workers(), 0);
        assert_eq!(metatron.total_shares(), 0);
        assert_eq!(metatron.total_blocks(), 0);
    }

    #[test]
    fn worker_count_increments_and_decrements() {
        let metatron = Metatron::new();
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
    fn share_count_increments() {
        let metatron = Metatron::new();
        metatron.add_share();
        metatron.add_share();
        metatron.add_share();
        assert_eq!(metatron.total_shares(), 3);
    }

    #[test]
    fn block_count_increments() {
        let metatron = Metatron::new();
        metatron.add_block();
        assert_eq!(metatron.total_blocks(), 1);
    }

    #[test]
    fn status_line_contains_all_fields() {
        let metatron = Metatron::new();
        metatron.add_worker();
        metatron.add_worker();
        metatron.add_share();
        metatron.add_share();
        metatron.add_share();
        metatron.add_block();

        let line = metatron.status_line();
        assert!(line.contains("workers=2"), "missing workers: {line}");
        assert!(line.contains("shares=3"), "missing shares: {line}");
        assert!(line.contains("blocks=1"), "missing blocks: {line}");
        assert!(line.contains("uptime="), "missing uptime: {line}");
    }

    #[test]
    fn status_line_format_is_stable() {
        let metatron = Metatron::new();
        let line = metatron.status_line();
        assert!(
            line.starts_with("workers=0  shares=0  blocks=0  uptime="),
            "unexpected format: {line}"
        );
    }
}
