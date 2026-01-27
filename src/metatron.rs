use super::*;

pub(crate) struct Metatron {
    blocks: AtomicU64,
    started: Instant,
    connections: AtomicU64,
    users: DashMap<Address, Arc<User>>,
    sessions: DashMap<Extranonce, SessionSnapshot>,
    extranonces: Extranonces,
    counter: AtomicU64,
}

impl Metatron {
    pub(crate) fn new(extranonces: Extranonces) -> Self {
        Self {
            blocks: AtomicU64::new(0),
            started: Instant::now(),
            connections: AtomicU64::new(0),
            users: DashMap::new(),
            sessions: DashMap::new(),
            extranonces,
            counter: AtomicU64::new(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
            ),
        }
    }

    pub(crate) fn spawn(self: Arc<Self>, cancel: CancellationToken, tasks: &mut JoinSet<()>) {
        info!("Spawning metatron session cleanup task");

        tasks.spawn(async move {
            let mut cleanup_interval = tokio::time::interval(Duration::from_secs(60));

            loop {
                tokio::select! {
                    biased;

                    _ = cancel.cancelled() => {
                        info!("Shutting down metatron");
                        break;
                    }

                    _ = cleanup_interval.tick() => {
                        self.sessions
                            .retain(|_, session| !session.is_expired(SESSION_TTL));
                    }
                }
            }
        });
    }

    pub(crate) fn next_enonce1(&self) -> Extranonce {
        let counter = self.counter.fetch_add(1, Ordering::Relaxed);

        match &self.extranonces {
            Extranonces::Pool(pool) => {
                let bytes = counter.to_le_bytes();
                Extranonce::from_bytes(&bytes[..pool.enonce1_size()])
            }
            Extranonces::Proxy(proxy) => {
                let upstream = proxy.upstream_enonce1().as_bytes();
                let mut bytes = [0u8; MAX_ENONCE_SIZE + ENONCE1_EXTENSION_SIZE];
                bytes[..upstream.len()].copy_from_slice(upstream);
                bytes[upstream.len()..upstream.len() + ENONCE1_EXTENSION_SIZE]
                    .copy_from_slice(&counter.to_le_bytes()[..ENONCE1_EXTENSION_SIZE]);
                Extranonce::from_bytes(&bytes[..upstream.len() + ENONCE1_EXTENSION_SIZE])
            }
        }
    }

    pub(crate) fn enonce2_size(&self) -> usize {
        self.extranonces.enonce2_size()
    }

    pub(crate) fn extranonces(&self) -> &Extranonces {
        &self.extranonces
    }

    pub(crate) fn get_or_create_worker(&self, address: Address, workername: &str) -> Arc<Worker> {
        let user = self
            .users
            .entry(address.clone())
            .or_insert_with(|| Arc::new(User::new(address)));

        user.get_or_create_worker(workername)
    }

    pub(crate) fn store_session(&self, session: SessionSnapshot) {
        info!("Storing session for enonce1 {}", session.enonce1);
        self.sessions.insert(session.enonce1.clone(), session);
    }

    pub(crate) fn take_session(&self, enonce1: &Extranonce) -> Option<SessionSnapshot> {
        self.sessions.remove(enonce1).map(|(_, session)| session)
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

    pub(crate) fn hashrate_1m(&self) -> HashRate {
        self.users
            .iter()
            .map(|user| user.hashrate_1m())
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

    pub(crate) fn disconnected(&self) -> usize {
        self.sessions.len()
    }

    pub(crate) fn idle(&self) -> usize {
        let now = Instant::now();
        self.users
            .iter()
            .map(|user| {
                user.workers()
                    .filter(|worker| {
                        worker
                            .last_share()
                            .is_none_or(|last| now.duration_since(last).as_secs() > 60)
                    })
                    .count()
            })
            .sum()
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
            "sps={:.2}  hashrate={:.2}  connections={}  users={}  workers={}  accepted={}  rejected={}  blocks={}  uptime={}s",
            self.sps_1m(),
            self.hashrate_1m(),
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

    fn proxy_extranonces() -> Extranonces {
        let upstream_enonce1 = Extranonce::from_bytes(&[0xde, 0xad, 0xbe, 0xef]);
        Extranonces::Proxy(ProxyExtranonces::new(upstream_enonce1, 8).unwrap())
    }

    fn test_address() -> Address {
        "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc"
            .parse::<Address<bitcoin::address::NetworkUnchecked>>()
            .unwrap()
            .assume_checked()
    }

    fn pool_extranonces() -> Extranonces {
        Extranonces::Pool(PoolExtranonces::new(ENONCE1_SIZE, 8).unwrap())
    }

    #[test]
    fn new_metatron_starts_at_zero() {
        let metatron = Metatron::new(pool_extranonces());
        assert_eq!(metatron.total_connections(), 0);
        assert_eq!(metatron.accepted(), 0);
        assert_eq!(metatron.rejected(), 0);
        assert_eq!(metatron.total_blocks(), 0);
        assert_eq!(metatron.total_users(), 0);
        assert_eq!(metatron.total_workers(), 0);
    }

    #[test]
    fn connection_count_increments_and_decrements() {
        let metatron = Metatron::new(pool_extranonces());
        assert_eq!(metatron.total_connections(), 0);

        metatron.add_connection();
        metatron.add_connection();
        assert_eq!(metatron.total_connections(), 2);

        metatron.sub_connection();
        assert_eq!(metatron.total_connections(), 1);
    }

    #[test]
    fn get_or_create_worker_creates_user_and_worker() {
        let metatron = Metatron::new(pool_extranonces());
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
    fn record_accepted_updates_stats() {
        let metatron = Metatron::new(pool_extranonces());
        let addr = test_address();
        let worker = metatron.get_or_create_worker(addr, "rig1");

        let pool_diff = Difficulty::from(1000.0);
        let share_diff = Difficulty::from(1500.0);

        worker.record_accepted(pool_diff, share_diff);
        worker.record_accepted(pool_diff, share_diff);

        assert_eq!(metatron.accepted(), 2);
        assert_eq!(metatron.rejected(), 0);
    }

    #[test]
    fn record_rejected_updates_stats() {
        let metatron = Metatron::new(pool_extranonces());
        let addr = test_address();
        let worker = metatron.get_or_create_worker(addr, "rig1");

        worker.record_rejected();
        worker.record_rejected();

        assert_eq!(metatron.accepted(), 0);
        assert_eq!(metatron.rejected(), 2);
    }

    #[test]
    fn block_count_increments() {
        let metatron = Metatron::new(pool_extranonces());
        metatron.add_block();
        assert_eq!(metatron.total_blocks(), 1);
    }

    #[test]
    fn next_enonce1_is_sequential() {
        let metatron = Metatron::new(pool_extranonces());
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
        let metatron = Metatron::new(pool_extranonces());
        assert_eq!(metatron.next_enonce1().len(), ENONCE1_SIZE);
    }

    #[test]
    fn next_enonce1_is_unique() {
        let metatron = Metatron::new(pool_extranonces());
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1000 {
            let enonce = metatron.next_enonce1();
            assert!(seen.insert(enonce), "duplicate enonce1 generated");
        }
    }

    #[test]
    fn proxy_mode_next_enonce1_extends_upstream() {
        let upstream_enonce1 = Extranonce::from_bytes(&[0xde, 0xad, 0xbe, 0xef]);
        let extranonces =
            Extranonces::Proxy(ProxyExtranonces::new(upstream_enonce1.clone(), 8).unwrap());
        let metatron = Metatron::new(extranonces);

        let e1 = metatron.next_enonce1();

        assert_eq!(e1.len(), 6);
        assert_eq!(&e1.as_bytes()[..4], upstream_enonce1.as_bytes());
    }

    #[test]
    fn proxy_mode_enonce2_size_reduced() {
        let metatron = Metatron::new(proxy_extranonces());
        assert_eq!(metatron.enonce2_size(), 6);
    }

    #[test]
    fn proxy_mode_next_enonce1_is_sequential() {
        let metatron = Metatron::new(proxy_extranonces());

        let e1 = metatron.next_enonce1();
        let e2 = metatron.next_enonce1();

        let ext1 = u16::from_le_bytes(e1.as_bytes()[4..6].try_into().unwrap());
        let ext2 = u16::from_le_bytes(e2.as_bytes()[4..6].try_into().unwrap());
        assert_eq!(ext2, ext1 + 1);
    }
}
