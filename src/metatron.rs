use {super::*, crate::throbber::StatusLine};

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
