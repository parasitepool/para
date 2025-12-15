use {super::*, parking_lot::Mutex};

#[allow(unused)]
pub(crate) struct Worker {
    workername: String,
    dsps_1m: Mutex<DecayingAverage>,
    sps_1m: Mutex<DecayingAverage>,
    accepted: AtomicU64,
    rejected: AtomicU64,
    best_ever: Mutex<f64>,
    last_share: Mutex<Option<Instant>>,
}

impl Worker {
    pub(crate) fn new(workername: String) -> Self {
        Self {
            workername,
            dsps_1m: Mutex::new(DecayingAverage::new(Duration::from_secs(60))),
            sps_1m: Mutex::new(DecayingAverage::new(Duration::from_secs(60))),
            accepted: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
            best_ever: Mutex::new(0.0),
            last_share: Mutex::new(None),
        }
    }

    pub(crate) fn record_accepted(&self, difficulty: f64) {
        let now = Instant::now();
        self.dsps_1m.lock().record(difficulty, now);
        self.sps_1m.lock().record(1.0, now);
        self.accepted.fetch_add(1, Ordering::Relaxed);
        *self.last_share.lock() = Some(now);

        let mut best = self.best_ever.lock();
        if difficulty > *best {
            *best = difficulty;
        }
    }

    pub(crate) fn record_rejected(&self) {
        self.rejected.fetch_add(1, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn workername(&self) -> String {
        self.workername.clone()
    }

    pub(crate) fn hash_rate_1m(&self) -> HashRate {
        HashRate::from_dsps(self.dsps_1m.lock().value_at(Instant::now()))
    }

    pub(crate) fn sps_1m(&self) -> f64 {
        self.sps_1m.lock().value_at(Instant::now())
    }

    pub(crate) fn accepted(&self) -> u64 {
        self.accepted.load(Ordering::Relaxed)
    }

    pub(crate) fn rejected(&self) -> u64 {
        self.rejected.load(Ordering::Relaxed)
    }

    pub(crate) fn best_ever(&self) -> f64 {
        *self.best_ever.lock()
    }

    pub(crate) fn last_share(&self) -> Option<Instant> {
        *self.last_share.lock()
    }
}
