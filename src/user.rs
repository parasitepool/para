use {super::*, dashmap::DashMap};

#[allow(unused)]
pub(crate) struct User {
    address: Address,
    workers: DashMap<String, Arc<Worker>>,
    authorized: Instant,
}

impl User {
    pub(crate) fn new(address: Address) -> Self {
        Self {
            address,
            workers: DashMap::new(),
            authorized: Instant::now(),
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

    pub(crate) fn hash_rate_1m(&self) -> HashRate {
        self.workers
            .iter()
            .map(|worker| worker.hash_rate_1m())
            .fold(HashRate::ZERO, |acc, r| acc + r)
    }

    pub(crate) fn sps_1m(&self) -> f64 {
        self.workers.iter().map(|worker| worker.sps_1m()).sum()
    }

    pub(crate) fn total_shares(&self) -> u64 {
        self.workers.iter().map(|worker| worker.shares()).sum()
    }

    pub(crate) fn best_ever(&self) -> f64 {
        self.workers
            .iter()
            .map(|worker| worker.best_ever())
            .fold(0.0, f64::max)
    }

    pub(crate) fn last_share(&self) -> Option<Instant> {
        self.workers
            .iter()
            .filter_map(|worker| worker.last_share())
            .max()
    }
}
