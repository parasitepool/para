use {super::*, dashmap::DashMap};

#[allow(unused)]
pub(crate) struct User {
    address: Address,
    workers: DashMap<String, Arc<Worker>>,
    authorized: Instant,
}

#[allow(unused)]
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

    pub(crate) fn total_shares(&self) -> u64 {
        self.workers.iter().map(|w| w.shares()).sum()
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

    pub(crate) fn workers(&self) -> Vec<Arc<Worker>> {
        self.workers.iter().map(|r| r.value().clone()).collect()
    }
}
