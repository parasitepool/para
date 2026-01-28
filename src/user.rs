use {super::*, dashmap::DashMap};

pub(crate) struct User {
    pub(crate) address: Address,
    pub(crate) workers: DashMap<String, Arc<Worker>>,
    pub(crate) authorized: u64,
}

impl User {
    pub(crate) fn new(address: Address) -> Self {
        Self {
            address,
            workers: DashMap::new(),
            authorized: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time went backwards")
                .as_secs(),
        }
    }

    pub(crate) fn get_or_create_worker(&self, workername: &str) -> Arc<Worker> {
        self.workers
            .entry(workername.to_string())
            .or_insert_with(|| Arc::new(Worker::new(workername.to_string())))
            .clone()
    }

    pub(crate) fn worker_count(&self) -> usize {
        self.workers.len()
    }

    pub(crate) fn hashrate_1m(&self) -> HashRate {
        self.workers
            .iter()
            .map(|worker| worker.hashrate_1m())
            .fold(HashRate::ZERO, |acc, r| acc + r)
    }

    pub(crate) fn hashrate_5m(&self) -> HashRate {
        self.workers
            .iter()
            .map(|worker| worker.hashrate_5m())
            .fold(HashRate::ZERO, |acc, r| acc + r)
    }

    pub(crate) fn hashrate_15m(&self) -> HashRate {
        self.workers
            .iter()
            .map(|worker| worker.hashrate_15m())
            .fold(HashRate::ZERO, |acc, r| acc + r)
    }

    pub(crate) fn hashrate_1hr(&self) -> HashRate {
        self.workers
            .iter()
            .map(|worker| worker.hashrate_1hr())
            .fold(HashRate::ZERO, |acc, r| acc + r)
    }

    pub(crate) fn hashrate_6hr(&self) -> HashRate {
        self.workers
            .iter()
            .map(|worker| worker.hashrate_6hr())
            .fold(HashRate::ZERO, |acc, r| acc + r)
    }

    pub(crate) fn hashrate_1d(&self) -> HashRate {
        self.workers
            .iter()
            .map(|worker| worker.hashrate_1d())
            .fold(HashRate::ZERO, |acc, r| acc + r)
    }

    pub(crate) fn hashrate_7d(&self) -> HashRate {
        self.workers
            .iter()
            .map(|worker| worker.hashrate_7d())
            .fold(HashRate::ZERO, |acc, r| acc + r)
    }

    pub(crate) fn sps_1m(&self) -> f64 {
        self.workers.iter().map(|worker| worker.sps_1m()).sum()
    }

    pub(crate) fn sps_5m(&self) -> f64 {
        self.workers.iter().map(|worker| worker.sps_5m()).sum()
    }

    pub(crate) fn sps_15m(&self) -> f64 {
        self.workers.iter().map(|worker| worker.sps_15m()).sum()
    }

    pub(crate) fn sps_1hr(&self) -> f64 {
        self.workers.iter().map(|worker| worker.sps_1hr()).sum()
    }

    pub(crate) fn accepted(&self) -> u64 {
        self.workers.iter().map(|worker| worker.accepted()).sum()
    }

    pub(crate) fn rejected(&self) -> u64 {
        self.workers.iter().map(|worker| worker.rejected()).sum()
    }

    pub(crate) fn best_ever(&self) -> Option<Difficulty> {
        self.workers
            .iter()
            .filter_map(|worker| worker.best_ever())
            .max()
    }

    pub(crate) fn total_work(&self) -> f64 {
        self.workers.iter().map(|w| w.total_work()).sum()
    }

    pub(crate) fn last_share(&self) -> Option<Instant> {
        self.workers
            .iter()
            .filter_map(|worker| worker.last_share())
            .max()
    }

    pub(crate) fn workers(&self) -> impl Iterator<Item = Arc<Worker>> {
        self.workers.iter().map(|entry| entry.value().clone())
    }
}

impl From<Address> for User {
    fn from(address: Address) -> Self {
        Self::new(address)
    }
}
