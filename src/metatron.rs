use super::*;

pub(crate) struct Metatron {
    enonce1_counter: AtomicU64,
    blocks: AtomicU64,
    started: Instant,
    connections: AtomicU64,
    users: DashMap<Address, Arc<User>>,
    sessions: DashMap<Extranonce, SessionSnapshot>,
}

impl Metatron {
    pub(crate) fn new() -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Self {
            enonce1_counter: AtomicU64::new(seed),
            blocks: AtomicU64::new(0),
            started: Instant::now(),
            connections: AtomicU64::new(0),
            users: DashMap::new(),
            sessions: DashMap::new(),
        }
    }

    pub(crate) fn spawn(
        self: Arc<Self>,
        sink: Option<mpsc::Sender<Share>>,
        cancel: CancellationToken,
        tasks: &mut JoinSet<()>,
    ) -> mpsc::Sender<Share> {
        info!("Spawning metatron task");
        let (share_tx, mut share_rx) = mpsc::channel(SHARE_CHANNEL_CAPACITY);

        tasks.spawn(async move {
            let mut cleanup_interval = tokio::time::interval(Duration::from_secs(60));

            loop {
                tokio::select! {
                    biased;

                    _ = cancel.cancelled() => {
                        info!("Shutting down metatron, draining {} pending shares", share_rx.len());

                        while let Ok(share) = share_rx.try_recv() {
                            self.process_share(&share, &sink);
                        }

                        break;
                    }

                    _ = cleanup_interval.tick() => {
                        self.sessions
                            .retain(|_, session| !session.is_expired(SESSION_TTL));
                    }

                    Some(share) = share_rx.recv() => {
                        self.process_share(&share, &sink);
                    }
                }
            }
        });

        share_tx
    }

    pub(crate) fn next_enonce1(&self) -> Extranonce {
        let value = self.enonce1_counter.fetch_add(1, Ordering::Relaxed);
        let bytes = value.to_le_bytes();
        Extranonce::from_bytes(&bytes[..ENONCE1_SIZE])
    }

    fn process_share(&self, share: &Share, sink: &Option<mpsc::Sender<Share>>) {
        let worker = self.get_or_create_worker(share.address.clone(), &share.workername);

        if share.result {
            let pool_diff = share.pool_diff.expect("accepted share must have pool_diff"); // TODO
            worker.record_accepted(pool_diff, share.share_diff);
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

    pub(crate) fn store_session(&self, session: SessionSnapshot) {
        info!("Storing session for enonce1 {}", session.enonce1);
        self.sessions.insert(session.enonce1.clone(), session);
    }

    pub(crate) fn take_session(&self, enonce1: &Extranonce) -> Option<SessionSnapshot> {
        self.sessions
            .remove(enonce1)
            .map(|(_, session)| session)
            .filter(|s| !s.is_expired(SESSION_TTL))
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

    pub(crate) fn best_ever(&self) -> Option<Difficulty> {
        self.users.iter().filter_map(|user| user.best_ever()).max()
    }

    pub(crate) fn uptime(&self) -> Duration {
        self.started.elapsed()
    }

    pub(crate) fn users(&self) -> &DashMap<Address, Arc<User>> {
        &self.users
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

    #[test]
    fn next_enonce1_is_sequential() {
        let metatron = Metatron::new();
        let e1 = metatron.next_enonce1();
        let e2 = metatron.next_enonce1();
        let e3 = metatron.next_enonce1();

        let v1 = u32::from_le_bytes(e1.as_bytes().try_into().unwrap());
        let v2 = u32::from_le_bytes(e2.as_bytes().try_into().unwrap());
        let v3 = u32::from_le_bytes(e3.as_bytes().try_into().unwrap());

        assert_eq!(v2, v1 + 1);
        assert_eq!(v3, v2 + 1);
    }

    #[test]
    fn next_enonce1_has_correct_size() {
        let metatron = Metatron::new();
        assert_eq!(metatron.next_enonce1().len(), ENONCE1_SIZE);
    }

    #[test]
    fn next_enonce1_is_unique() {
        let metatron = Metatron::new();
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1000 {
            let enonce = metatron.next_enonce1();
            assert!(seen.insert(enonce), "duplicate enonce1 generated");
        }
    }
}
