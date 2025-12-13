use super::*;

pub(crate) struct Metatron {
    blocks: AtomicU64,
    accepted: AtomicU64,
    rejected: AtomicU64,
    started: Instant,
    connections: AtomicU64,
    users: DashMap<Address<bitcoin::address::NetworkUnchecked>, Arc<UserStats>>,
}

impl Metatron {
    pub(crate) fn new() -> Self {
        Self {
            blocks: AtomicU64::new(0),
            accepted: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
            started: Instant::now(),
            connections: AtomicU64::new(0),
            users: DashMap::new(),
        }
    }

    pub(crate) fn get_or_create_worker(
        &self,
        address: Address<bitcoin::address::NetworkUnchecked>,
        workername: &str,
    ) -> Arc<WorkerStats> {
        let user = self
            .users
            .entry(address.clone())
            .or_insert_with(|| Arc::new(UserStats::new(address)))
            .clone();

        user.get_or_create_worker(workername)
    }

    pub(crate) fn record_share(
        &self,
        address: &Address<bitcoin::address::NetworkUnchecked>,
        workername: &str,
        difficulty: f64,
    ) {
        if let Some(user) = self.users.get(address) {
            let worker = user.get_or_create_worker(workername);
            worker.record_share(difficulty);
        }
        self.accepted.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn add_block(&self) {
        self.blocks.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn add_rejected(&self) {
        self.rejected.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn add_connection(&self) {
        self.connections.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn sub_connection(&self) {
        self.connections.fetch_sub(1, Ordering::Relaxed);
    }

    pub(crate) fn hash_rate_1m(&self) -> HashRate {
        self.users
            .iter()
            .map(|u| u.hash_rate_1m())
            .fold(HashRate::ZERO, |acc, r| acc + r)
    }

    pub(crate) fn hash_rate_5m(&self) -> HashRate {
        self.users
            .iter()
            .map(|u| u.hash_rate_5m())
            .fold(HashRate::ZERO, |acc, r| acc + r)
    }

    pub(crate) fn hash_rate_1h(&self) -> HashRate {
        self.users
            .iter()
            .map(|u| u.hash_rate_1h())
            .fold(HashRate::ZERO, |acc, r| acc + r)
    }

    pub(crate) fn hash_rate_1d(&self) -> HashRate {
        self.users
            .iter()
            .map(|u| u.hash_rate_1d())
            .fold(HashRate::ZERO, |acc, r| acc + r)
    }

    pub(crate) fn hash_rate_7d(&self) -> HashRate {
        self.users
            .iter()
            .map(|u| u.hash_rate_7d())
            .fold(HashRate::ZERO, |acc, r| acc + r)
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

    pub(crate) fn total_connections(&self) -> u64 {
        self.connections.load(Ordering::Relaxed)
    }

    pub(crate) fn total_users(&self) -> usize {
        self.users.len()
    }

    pub(crate) fn total_workers(&self) -> usize {
        self.users.iter().map(|u| u.worker_count()).sum()
    }

    pub(crate) fn total_shares(&self) -> u64 {
        self.users.iter().map(|u| u.total_shares()).sum()
    }

    pub(crate) fn uptime(&self) -> Duration {
        self.started.elapsed()
    }

    pub(crate) fn get_user(
        &self,
        address: &Address<bitcoin::address::NetworkUnchecked>,
    ) -> Option<Arc<UserStats>> {
        self.users.get(address).map(|r| r.value().clone())
    }

    pub(crate) fn users(&self) -> Vec<Arc<UserStats>> {
        self.users.iter().map(|r| r.value().clone()).collect()
    }
}

impl Default for Metatron {
    fn default() -> Self {
        Self::new()
    }
}

impl StatusLine for Metatron {
    fn status_line(&self) -> String {
        format!(
            "hr_5m={}  users={}  workers={}  conns={}  accepted={}  rejected={}  blocks={}  uptime={}s",
            self.hash_rate_5m(),
            self.total_users(),
            self.total_workers(),
            self.total_connections(),
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

    fn test_address() -> Address<bitcoin::address::NetworkUnchecked> {
        "bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq"
            .parse()
            .unwrap()
    }

    #[test]
    fn new_metatron_starts_at_zero() {
        let metatron = Metatron::new();
        assert_eq!(metatron.total_connections(), 0);
        assert_eq!(metatron.accepted(), 0);
        assert_eq!(metatron.rejected(), 0);
        assert_eq!(metatron.total_blocks(), 0);
        assert_eq!(metatron.total_users(), 0);
        assert_eq!(metatron.total_workers(), 0);
    }

    #[test]
    fn connection_count_increments_and_decrements() {
        let metatron = Metatron::new();
        assert_eq!(metatron.total_connections(), 0);

        metatron.add_connection();
        metatron.add_connection();
        assert_eq!(metatron.total_connections(), 2);

        metatron.sub_connection();
        assert_eq!(metatron.total_connections(), 1);
    }

    #[test]
    fn get_or_create_worker_creates_user_and_worker() {
        let metatron = Metatron::new();
        let addr = test_address();

        let worker = metatron.get_or_create_worker(addr.clone(), "rig1");
        assert_eq!(worker.workername, "rig1");
        assert_eq!(metatron.total_users(), 1);
        assert_eq!(metatron.total_workers(), 1);

        let worker2 = metatron.get_or_create_worker(addr.clone(), "rig2");
        assert_eq!(worker2.workername, "rig2");
        assert_eq!(metatron.total_users(), 1);
        assert_eq!(metatron.total_workers(), 2);
    }

    #[test]
    fn record_share_updates_stats() {
        let metatron = Metatron::new();
        let addr = test_address();

        metatron.get_or_create_worker(addr.clone(), "rig1");
        metatron.record_share(&addr, "rig1", 100.0);
        metatron.record_share(&addr, "rig1", 200.0);

        assert_eq!(metatron.accepted(), 2);
        assert_eq!(metatron.total_shares(), 2);

        let user = metatron.get_user(&addr).unwrap();
        assert_eq!(user.total_shares(), 2);
    }

    #[test]
    fn rejected_count_increments() {
        let metatron = Metatron::new();
        metatron.add_rejected();
        metatron.add_rejected();
        assert_eq!(metatron.rejected(), 2);
    }

    #[test]
    fn block_count_increments() {
        let metatron = Metatron::new();
        metatron.add_block();
        assert_eq!(metatron.total_blocks(), 1);
    }

    #[test]
    fn hash_rates_aggregate_from_workers() {
        let metatron = Metatron::new();
        let addr = test_address();

        metatron.get_or_create_worker(addr.clone(), "rig1");
        metatron.record_share(&addr, "rig1", 1000.0);

        let rate = metatron.hash_rate_5m();
        assert!(rate.0 > 0.0, "hashrate should be positive: {}", rate);
    }
}
