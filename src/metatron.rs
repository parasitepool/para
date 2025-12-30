use {
    super::*,
    crate::api::{PoolStats, UserDetail, UserSummary, WorkerSummary},
};

pub(crate) struct Metatron {
    blocks: AtomicU64,
    started: Instant,
    connections: AtomicU64,
    users: DashMap<Address, Arc<User>>,
}

impl Metatron {
    pub(crate) fn new() -> Self {
        Self {
            blocks: AtomicU64::new(0),
            started: Instant::now(),
            connections: AtomicU64::new(0),
            users: DashMap::new(),
        }
    }

    pub(crate) async fn run(
        self: Arc<Self>,
        mut rx: mpsc::Receiver<Share>,
        sink: Option<mpsc::Sender<Share>>,
        cancel: CancellationToken,
    ) {
        loop {
            tokio::select! {
                biased;

                _ = cancel.cancelled() => {
                    info!("Metatron shutting down, draining {} pending shares", rx.len());

                    while let Ok(share) = rx.try_recv() {
                        self.process_share(&share, &sink);
                    }

                    break;
                }

                Some(share) = rx.recv() => {
                    self.process_share(&share, &sink);
                }
            }
        }

        info!(
            "Metatron stopped: {} users, {} workers, {} accepted, {} rejected",
            self.total_users(),
            self.total_workers(),
            self.accepted(),
            self.rejected()
        );
    }

    fn process_share(&self, share: &Share, sink: &Option<mpsc::Sender<Share>>) {
        let worker = self.get_or_create_worker(share.address.clone(), &share.workername);

        if share.result {
            worker.record_accepted(share.pool_diff, share.share_diff);
        } else {
            worker.record_rejected();
        }

        if let Some(tx) = sink
            && tx.try_send(share.clone()).is_err()
        {
            warn!("Share sink full, dropping event");
        }
    }

    fn get_or_create_worker(&self, address: Address, workername: &str) -> Arc<Worker> {
        let user = self
            .users
            .entry(address.clone())
            .or_insert_with(|| Arc::new(User::new(address)))
            .clone();

        user.get_or_create_worker(workername)
    }

    pub(crate) fn add_block(&self) {
        self.blocks.fetch_add(1, Ordering::Relaxed);
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
            .map(|user| user.hash_rate_1m())
            .fold(HashRate::ZERO, |acc, r| acc + r)
    }

    pub(crate) fn sps_1m(&self) -> f64 {
        self.users.iter().map(|user| user.sps_1m()).sum()
    }

    pub(crate) fn accepted(&self) -> u64 {
        self.users.iter().map(|user| user.accepted()).sum()
    }

    pub(crate) fn rejected(&self) -> u64 {
        self.users.iter().map(|user| user.rejected()).sum()
    }

    pub(crate) fn total_blocks(&self) -> u64 {
        self.blocks.load(Ordering::Relaxed)
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

    pub(crate) fn last_share(&self) -> Option<Instant> {
        self.users.iter().filter_map(|user| user.last_share()).max()
    }

    pub(crate) fn best_ever(&self) -> f64 {
        self.users
            .iter()
            .map(|user| user.best_ever())
            .fold(0.0, f64::max)
    }

    pub(crate) fn uptime(&self) -> Duration {
        self.started.elapsed()
    }

    pub(crate) fn stats(&self) -> PoolStats {
        PoolStats {
            hash_rate_1m: self.hash_rate_1m(),
            sps_1m: self.sps_1m(),
            users: self.total_users(),
            workers: self.total_workers(),
            connections: self.total_connections(),
            accepted: self.accepted(),
            rejected: self.rejected(),
            blocks: self.total_blocks(),
            best_ever: self.best_ever(),
            last_share: self.last_share().map(|time| time.elapsed().as_secs()),
            uptime_secs: self.uptime().as_secs(),
        }
    }

    pub(crate) fn users(&self) -> Vec<UserSummary> {
        self.users
            .iter()
            .map(|entry| {
                let user = entry.value();
                UserSummary {
                    address: entry.key().to_string(),
                    hash_rate: user.hash_rate_1m(),
                    shares_per_second: user.sps_1m(),
                    workers: user.worker_count(),
                    accepted: user.accepted(),
                    rejected: user.rejected(),
                    best_ever: user.best_ever(),
                }
            })
            .collect()
    }

    pub(crate) fn user(&self, address: &str) -> Option<UserDetail> {
        let parsed: Address = address
            .parse::<Address<bitcoin::address::NetworkUnchecked>>()
            .ok()?
            .assume_checked();

        self.users.get(&parsed).map(|entry| {
            let user = entry.value();
            UserDetail {
                address: parsed.to_string(),
                hash_rate: user.hash_rate_1m(),
                shares_per_second: user.sps_1m(),
                accepted: user.accepted(),
                rejected: user.rejected(),
                best_ever: user.best_ever(),
                workers: user
                    .workers()
                    .map(|worker| WorkerSummary {
                        name: worker.workername().to_string(),
                        hash_rate: worker.hash_rate_1m(),
                        shares_per_second: worker.sps_1m(),
                        accepted: worker.accepted(),
                        rejected: worker.rejected(),
                        best_ever: worker.best_ever(),
                    })
                    .collect(),
            }
        })
    }
}

impl StatusLine for Metatron {
    fn status_line(&self) -> String {
        format!(
            "sps={:.2}  hash_rate={}  connections={}  users={}  workers={}  accepted={}  rejected={}  blocks={}  uptime={}s",
            self.sps_1m() + 0.0,
            self.hash_rate_1m(),
            self.total_connections(),
            self.total_users(),
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

    fn test_address() -> Address {
        "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc"
            .parse::<Address<bitcoin::address::NetworkUnchecked>>()
            .unwrap()
            .assume_checked()
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
        assert_eq!(worker.workername(), "rig1");
        assert_eq!(metatron.total_users(), 1);
        assert_eq!(metatron.total_workers(), 1);

        let worker2 = metatron.get_or_create_worker(addr.clone(), "rig2");
        assert_eq!(worker2.workername(), "rig2");
        assert_eq!(metatron.total_users(), 1);
        assert_eq!(metatron.total_workers(), 2);
    }

    #[test]
    fn rejected_count_increments() {
        let metatron = Metatron::new();
        let addr = test_address();
        let worker = metatron.get_or_create_worker(addr, "rig1");
        worker.record_rejected();
        worker.record_rejected();
        assert_eq!(metatron.rejected(), 2);
    }

    #[test]
    fn block_count_increments() {
        let metatron = Metatron::new();
        metatron.add_block();
        assert_eq!(metatron.total_blocks(), 1);
    }
}
