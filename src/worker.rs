use {super::*, parking_lot::Mutex};

struct Stats {
    dsps_1m: DecayingAverage,
    sps_1m: DecayingAverage,
    best_ever: Option<Difficulty>,
    last_share: Option<Instant>,
}

pub(crate) struct Worker {
    workername: String,
    stats: Mutex<Stats>,
    accepted: AtomicU64,
    rejected: AtomicU64,
}

impl Worker {
    pub(crate) fn new(workername: String) -> Self {
        Self {
            workername,
            stats: Mutex::new(Stats {
                dsps_1m: DecayingAverage::new(Duration::from_secs(60)),
                sps_1m: DecayingAverage::new(Duration::from_secs(60)),
                best_ever: None,
                last_share: None,
            }),
            accepted: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
        }
    }

    pub(crate) fn record_accepted(&self, pool_diff: Difficulty, share_diff: Difficulty) {
        let now = Instant::now();
        let mut stats = self.stats.lock();
        stats.dsps_1m.record(pool_diff.as_f64(), now);
        stats.sps_1m.record(1.0, now);
        stats.last_share = Some(now);
        if stats.best_ever.is_none_or(|best| share_diff > best) {
            stats.best_ever = Some(share_diff);
        }
        drop(stats);
        self.accepted.fetch_add(1, Ordering::Relaxed);
    }

    #[allow(dead_code)]
    pub(crate) fn record_rejected(&self) {
        self.rejected.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn workername(&self) -> &str {
        &self.workername
    }

    pub(crate) fn hash_rate_1m(&self) -> HashRate {
        HashRate::from_dsps(self.stats.lock().dsps_1m.value_at(Instant::now()))
    }

    pub(crate) fn sps_1m(&self) -> f64 {
        self.stats.lock().sps_1m.value_at(Instant::now())
    }

    pub(crate) fn accepted(&self) -> u64 {
        self.accepted.load(Ordering::Relaxed)
    }

    pub(crate) fn rejected(&self) -> u64 {
        self.rejected.load(Ordering::Relaxed)
    }

    pub(crate) fn best_ever(&self) -> Option<Difficulty> {
        self.stats.lock().best_ever
    }

    pub(crate) fn last_share(&self) -> Option<Instant> {
        self.stats.lock().last_share
    }
}
