use {
    super::*,
    dashmap::DashMap,
    parking_lot::Mutex,
};

pub(crate) struct WorkerStats {
    pub workername: String,
    hash_rates: SharedHashRates,
    shares: AtomicU64,
    best_share: Mutex<f64>,
    best_ever: Mutex<f64>,
    last_share: Mutex<Option<Instant>>,
}

impl WorkerStats {
    pub(crate) fn new(workername: String) -> Self {
        Self {
            workername,
            hash_rates: SharedHashRates::new(),
            shares: AtomicU64::new(0),
            best_share: Mutex::new(0.0),
            best_ever: Mutex::new(0.0),
            last_share: Mutex::new(None),
        }
    }

    pub(crate) fn record_share(&self, difficulty: f64) {
        self.hash_rates.record(difficulty);
        self.shares.fetch_add(1, Ordering::Relaxed);
        *self.last_share.lock() = Some(Instant::now());

        let mut best = self.best_share.lock();
        if difficulty > *best {
            *best = difficulty;
            let mut ever = self.best_ever.lock();
            if difficulty > *ever {
                *ever = difficulty;
            }
        }
    }

    pub(crate) fn hash_rate_1m(&self) -> HashRate {
        self.hash_rates.hash_rate_1m()
    }

    pub(crate) fn hash_rate_5m(&self) -> HashRate {
        self.hash_rates.hash_rate_5m()
    }

    pub(crate) fn hash_rate_1h(&self) -> HashRate {
        self.hash_rates.hash_rate_1h()
    }

    pub(crate) fn hash_rate_1d(&self) -> HashRate {
        self.hash_rates.hash_rate_1d()
    }

    pub(crate) fn hash_rate_7d(&self) -> HashRate {
        self.hash_rates.hash_rate_7d()
    }

    pub(crate) fn shares(&self) -> u64 {
        self.shares.load(Ordering::Relaxed)
    }

    pub(crate) fn best_share(&self) -> f64 {
        *self.best_share.lock()
    }

    pub(crate) fn best_ever(&self) -> f64 {
        *self.best_ever.lock()
    }

    pub(crate) fn last_share(&self) -> Option<Instant> {
        *self.last_share.lock()
    }

    pub(crate) fn last_share_timestamp(&self) -> Option<u64> {
        self.last_share().map(|_| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
        })
    }
}

pub(crate) struct UserStats {
    pub address: Address<bitcoin::address::NetworkUnchecked>,
    workers: DashMap<String, Arc<WorkerStats>>,
    authorized: Instant,
}

impl UserStats {
    pub(crate) fn new(address: Address<bitcoin::address::NetworkUnchecked>) -> Self {
        Self {
            address,
            workers: DashMap::new(),
            authorized: Instant::now(),
        }
    }

    pub(crate) fn get_or_create_worker(&self, workername: &str) -> Arc<WorkerStats> {
        self.workers
            .entry(workername.to_string())
            .or_insert_with(|| Arc::new(WorkerStats::new(workername.to_string())))
            .clone()
    }

    pub(crate) fn worker_count(&self) -> usize {
        self.workers.len()
    }

    pub(crate) fn hash_rate_1m(&self) -> HashRate {
        self.workers
            .iter()
            .map(|w| w.hash_rate_1m())
            .fold(HashRate::ZERO, |acc, r| acc + r)
    }

    pub(crate) fn hash_rate_5m(&self) -> HashRate {
        self.workers
            .iter()
            .map(|w| w.hash_rate_5m())
            .fold(HashRate::ZERO, |acc, r| acc + r)
    }

    pub(crate) fn hash_rate_1h(&self) -> HashRate {
        self.workers
            .iter()
            .map(|w| w.hash_rate_1h())
            .fold(HashRate::ZERO, |acc, r| acc + r)
    }

    pub(crate) fn hash_rate_1d(&self) -> HashRate {
        self.workers
            .iter()
            .map(|w| w.hash_rate_1d())
            .fold(HashRate::ZERO, |acc, r| acc + r)
    }

    pub(crate) fn hash_rate_7d(&self) -> HashRate {
        self.workers
            .iter()
            .map(|w| w.hash_rate_7d())
            .fold(HashRate::ZERO, |acc, r| acc + r)
    }

    pub(crate) fn total_shares(&self) -> u64 {
        self.workers.iter().map(|w| w.shares()).sum()
    }

    pub(crate) fn best_share(&self) -> f64 {
        self.workers
            .iter()
            .map(|w| w.best_share())
            .fold(0.0, f64::max)
    }

    pub(crate) fn best_ever(&self) -> f64 {
        self.workers
            .iter()
            .map(|w| w.best_ever())
            .fold(0.0, f64::max)
    }

    pub(crate) fn last_share(&self) -> Option<Instant> {
        self.workers.iter().filter_map(|w| w.last_share()).max()
    }

    pub(crate) fn last_share_timestamp(&self) -> Option<u64> {
        self.last_share().map(|_| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
        })
    }

    pub(crate) fn authorized_timestamp(&self) -> u64 {
        let elapsed = self.authorized.elapsed();
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .saturating_sub(elapsed)
            .as_secs()
    }

    pub(crate) fn workers(&self) -> Vec<Arc<WorkerStats>> {
        self.workers.iter().map(|r| r.value().clone()).collect()
    }
}

