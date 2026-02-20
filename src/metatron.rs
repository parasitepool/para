use {
    super::*,
    client::{Client, ClientId},
    user::User,
    worker::Worker,
};

pub(crate) mod client;
mod user;
mod worker;

pub(crate) struct Metatron {
    blocks: AtomicU64,
    started: Instant,
    client_id_counter: AtomicU64,
    users: DashMap<Address, Arc<User>>,
    sessions: DashMap<Extranonce, SessionSnapshot>,
    extranonces: Extranonces,
    enonce_counter: AtomicU64,
    endpoint: String,
}

impl Metatron {
    pub(crate) fn new(extranonces: Extranonces, endpoint: String) -> Self {
        Self {
            blocks: AtomicU64::new(0),
            started: Instant::now(),
            client_id_counter: AtomicU64::new(0),
            users: DashMap::new(),
            sessions: DashMap::new(),
            extranonces,
            enonce_counter: AtomicU64::new(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
            ),
            endpoint,
        }
    }

    pub(crate) fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub(crate) fn new_client(&self) -> Arc<Client> {
        let client_id = ClientId(self.client_id_counter.fetch_add(1, Ordering::Relaxed));
        Arc::new(Client::new(client_id))
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

                        info!("{}", self.status_line());
                    }
                }
            }
        });
    }

    pub(crate) fn next_enonce1(&self) -> Extranonce {
        let counter = self.enonce_counter.fetch_add(1, Ordering::Relaxed);

        match &self.extranonces {
            Extranonces::Pool(pool) => {
                let bytes = counter.to_le_bytes();
                Extranonce::from_bytes(&bytes[..pool.enonce1_size()])
            }
            Extranonces::Proxy(proxy) => {
                let upstream = proxy.upstream_enonce1().as_bytes();
                let extension_size = proxy.extension_size();
                let mut bytes = [0u8; MAX_ENONCE_SIZE * 2];

                bytes[..upstream.len()].copy_from_slice(upstream);
                bytes[upstream.len()..upstream.len() + extension_size]
                    .copy_from_slice(&counter.to_le_bytes()[..extension_size]);

                Extranonce::from_bytes(&bytes[..upstream.len() + extension_size])
            }
        }
    }

    pub(crate) fn enonce2_size(&self) -> usize {
        self.extranonces.enonce2_size()
    }

    pub(crate) fn extranonces(&self) -> &Extranonces {
        &self.extranonces
    }

    pub(crate) fn register_client(&self, address: Address, workername: &str, client: Arc<Client>) {
        let user = self
            .users
            .entry(address.clone())
            .or_insert_with(|| Arc::new(User::new(address)));

        user.register_client(workername, client);
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

    pub(crate) fn hashrate_1m(&self) -> HashRate {
        self.users
            .iter()
            .map(|user| user.hashrate_1m())
            .fold(HashRate::ZERO, |acc, r| acc + r)
    }

    pub(crate) fn hashrate_5m(&self) -> HashRate {
        self.users
            .iter()
            .map(|user| user.hashrate_5m())
            .fold(HashRate::ZERO, |acc, r| acc + r)
    }

    pub(crate) fn hashrate_15m(&self) -> HashRate {
        self.users
            .iter()
            .map(|user| user.hashrate_15m())
            .fold(HashRate::ZERO, |acc, r| acc + r)
    }

    pub(crate) fn hashrate_1hr(&self) -> HashRate {
        self.users
            .iter()
            .map(|user| user.hashrate_1hr())
            .fold(HashRate::ZERO, |acc, r| acc + r)
    }

    pub(crate) fn hashrate_6hr(&self) -> HashRate {
        self.users
            .iter()
            .map(|user| user.hashrate_6hr())
            .fold(HashRate::ZERO, |acc, r| acc + r)
    }

    pub(crate) fn hashrate_1d(&self) -> HashRate {
        self.users
            .iter()
            .map(|user| user.hashrate_1d())
            .fold(HashRate::ZERO, |acc, r| acc + r)
    }

    pub(crate) fn hashrate_7d(&self) -> HashRate {
        self.users
            .iter()
            .map(|user| user.hashrate_7d())
            .fold(HashRate::ZERO, |acc, r| acc + r)
    }

    pub(crate) fn sps_1m(&self) -> f64 {
        self.users.iter().map(|user| user.sps_1m()).sum()
    }

    pub(crate) fn sps_5m(&self) -> f64 {
        self.users.iter().map(|user| user.sps_5m()).sum()
    }

    pub(crate) fn sps_15m(&self) -> f64 {
        self.users.iter().map(|user| user.sps_15m()).sum()
    }

    pub(crate) fn sps_1hr(&self) -> f64 {
        self.users.iter().map(|user| user.sps_1hr()).sum()
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

    pub(crate) fn total_clients(&self) -> usize {
        self.users.iter().map(|u| u.client_count()).sum()
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

    pub(crate) fn total_work(&self) -> f64 {
        self.users.iter().map(|u| u.total_work()).sum()
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
            "sps={:.2}  hashrate={:.2}  clients={}  users={}  workers={}  accepted={}  rejected={}  blocks={}  uptime={}s",
            self.sps_1m(),
            self.hashrate_1m(),
            self.total_clients(),
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
        Extranonces::Proxy(ProxyExtranonces::new(upstream_enonce1, 8, 2).unwrap())
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
        let metatron = Metatron::new(pool_extranonces(), String::new());
        assert_eq!(metatron.total_clients(), 0);
        assert_eq!(metatron.accepted(), 0);
        assert_eq!(metatron.rejected(), 0);
        assert_eq!(metatron.total_blocks(), 0);
        assert_eq!(metatron.total_users(), 0);
        assert_eq!(metatron.total_workers(), 0);
    }

    #[test]
    fn client_count_tracks_active_clients() {
        let metatron = Metatron::new(pool_extranonces(), String::new());
        let addr = test_address();
        assert_eq!(metatron.total_clients(), 0);

        let c1 = metatron.new_client();
        metatron.register_client(addr.clone(), "foo", c1.clone());
        let c2 = metatron.new_client();
        metatron.register_client(addr, "foo", c2.clone());
        assert_eq!(metatron.total_clients(), 2);

        c1.deactivate();
        assert_eq!(metatron.total_clients(), 1);

        c2.deactivate();
        assert_eq!(metatron.total_clients(), 0);
    }

    #[test]
    fn register_client_creates_user_and_worker() {
        let metatron = Metatron::new(pool_extranonces(), String::new());
        let addr = test_address();

        metatron.register_client(addr.clone(), "rig1", metatron.new_client());
        assert_eq!(metatron.total_users(), 1);
        assert_eq!(metatron.total_workers(), 1);

        metatron.register_client(addr.clone(), "rig2", metatron.new_client());
        assert_eq!(metatron.total_users(), 1);
        assert_eq!(metatron.total_workers(), 2);
    }

    #[test]
    fn record_accepted_updates_stats() {
        let metatron = Metatron::new(pool_extranonces(), String::new());
        let addr = test_address();
        let client = metatron.new_client();
        metatron.register_client(addr, "rig1", client.clone());

        let pool_diff = Difficulty::from(1000.0);
        let share_diff = Difficulty::from(1500.0);

        client.record_accepted(pool_diff, share_diff);
        client.record_accepted(pool_diff, share_diff);

        assert_eq!(metatron.accepted(), 2);
        assert_eq!(metatron.rejected(), 0);
    }

    #[test]
    fn record_rejected_updates_stats() {
        let metatron = Metatron::new(pool_extranonces(), String::new());
        let addr = test_address();
        let client = metatron.new_client();
        metatron.register_client(addr, "rig1", client.clone());

        client.record_rejected();
        client.record_rejected();

        assert_eq!(metatron.accepted(), 0);
        assert_eq!(metatron.rejected(), 2);
    }

    #[test]
    fn block_count_increments() {
        let metatron = Metatron::new(pool_extranonces(), String::new());
        metatron.add_block();
        assert_eq!(metatron.total_blocks(), 1);
    }

    #[test]
    fn next_enonce1_is_sequential() {
        let metatron = Metatron::new(pool_extranonces(), String::new());
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
        let metatron = Metatron::new(pool_extranonces(), String::new());
        assert_eq!(metatron.next_enonce1().len(), ENONCE1_SIZE);
    }

    #[test]
    fn next_enonce1_is_unique() {
        let metatron = Metatron::new(pool_extranonces(), String::new());
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
            Extranonces::Proxy(ProxyExtranonces::new(upstream_enonce1.clone(), 8, 2).unwrap());
        let metatron = Metatron::new(extranonces, String::new());

        let e1 = metatron.next_enonce1();

        assert_eq!(e1.len(), 6);
        assert_eq!(&e1.as_bytes()[..4], upstream_enonce1.as_bytes());
    }

    #[test]
    fn proxy_mode_enonce2_size_reduced() {
        let metatron = Metatron::new(proxy_extranonces(), String::new());
        assert_eq!(metatron.enonce2_size(), 6);
    }

    #[test]
    fn proxy_mode_next_enonce1_is_sequential() {
        let metatron = Metatron::new(proxy_extranonces(), String::new());

        let e1 = metatron.next_enonce1();
        let e2 = metatron.next_enonce1();

        let ext1 = u16::from_le_bytes(e1.as_bytes()[4..6].try_into().unwrap());
        let ext2 = u16::from_le_bytes(e2.as_bytes()[4..6].try_into().unwrap());
        assert_eq!(ext2, ext1.wrapping_add(1));
    }

    #[test]
    fn proxy_mode_extension_size_1() {
        let upstream_enonce1 = Extranonce::from_bytes(&[0xde, 0xad, 0xbe, 0xef]);
        let extranonces =
            Extranonces::Proxy(ProxyExtranonces::new(upstream_enonce1.clone(), 8, 1).unwrap());
        let metatron = Metatron::new(extranonces, String::new());

        let e1 = metatron.next_enonce1();
        assert_eq!(e1.len(), 5);
        assert_eq!(&e1.as_bytes()[..4], upstream_enonce1.as_bytes());
        assert_eq!(metatron.enonce2_size(), 7);

        let e2 = metatron.next_enonce1();
        let ext1 = e1.as_bytes()[4];
        let ext2 = e2.as_bytes()[4];
        assert_eq!(ext2, ext1.wrapping_add(1));
    }

    #[test]
    fn total_work_accumulates() {
        let metatron = Metatron::new(pool_extranonces(), String::new());
        let addr = test_address();
        let pool_diff = Difficulty::from(100.0);
        let pool_diff_f64 = pool_diff.as_f64();

        assert_eq!(metatron.total_work(), 0.0);

        let foo_client = metatron.new_client();
        metatron.register_client(addr.clone(), "foo", foo_client.clone());
        foo_client.record_accepted(pool_diff, Difficulty::from(200.0));
        foo_client.record_accepted(pool_diff, Difficulty::from(50.0));

        assert!(
            (metatron.total_work() - 2.0 * pool_diff_f64).abs()
                < f64::EPSILON * 2.0 * pool_diff_f64
        );

        let bar_client = metatron.new_client();
        metatron.register_client(addr, "bar", bar_client.clone());
        bar_client.record_accepted(pool_diff, Difficulty::from(400.0));

        assert!(
            (metatron.total_work() - 3.0 * pool_diff_f64).abs()
                < f64::EPSILON * 3.0 * pool_diff_f64
        );
    }
}
