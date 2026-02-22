use {
    super::*, session::Session, stats::Stats, stratifier::state::Authorization, user::User,
    worker::Worker,
};

pub(crate) mod session;
mod stats;
mod user;
mod worker;

pub(crate) struct Metatron {
    blocks: AtomicU64,
    started: Instant,
    users: DashMap<Address, Arc<User>>,
    disconnected: DashMap<Extranonce, Arc<Session>>,
    extranonces: Extranonces,
    enonce_counter: AtomicU64,
    session_id_counter: AtomicU64,
    endpoint: String,
}

impl Metatron {
    pub(crate) fn new(extranonces: Extranonces, endpoint: String) -> Self {
        Self {
            blocks: AtomicU64::new(0),
            started: Instant::now(),
            users: DashMap::new(),
            disconnected: DashMap::new(),
            extranonces,
            enonce_counter: AtomicU64::new(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
            ),
            session_id_counter: AtomicU64::new(0),
            endpoint,
        }
    }

    pub(crate) fn endpoint(&self) -> &str {
        &self.endpoint
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
                        self.disconnected.retain(|_, session| {
                            let expired = session
                                .deactivated_at()
                                .is_some_and(|at| at.elapsed() >= SESSION_TTL);
                            if expired {
                                self.retire_session(session);
                            }
                            !expired
                        });

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

    pub(crate) fn new_session(&self, auth: Arc<Authorization>) -> Arc<Session> {
        let id = self.session_id_counter.fetch_add(1, Ordering::Relaxed);

        let session = Arc::new(Session::new(
            id,
            auth.enonce1.clone(),
            auth.address.clone(),
            auth.workername.clone(),
            auth.username.clone(),
            auth.version_mask,
        ));

        if let Some(user) = self.users.get(&auth.address) {
            user.new_session(&auth.workername, session.clone());
        } else {
            self.users
                .entry(auth.address.clone())
                .or_insert_with(|| Arc::new(User::new(auth.address.clone())))
                .new_session(&auth.workername, session.clone());
        }

        session
    }

    pub(crate) fn store_disconnected(&self, session: Arc<Session>) {
        info!(
            "Storing disconnected session for enonce1 {}",
            session.enonce1()
        );
        self.disconnected.insert(session.enonce1().clone(), session);
    }

    pub(crate) fn take_disconnected(&self, enonce1: &Extranonce) -> Option<Extranonce> {
        if let Some((_, session)) = self.disconnected.remove(enonce1) {
            self.retire_session(&session);
            Some(session.enonce1().clone())
        } else {
            None
        }
    }

    fn retire_session(&self, session: &Session) {
        if let Some(user) = self.users.get(session.address())
            && let Some(worker) = user.workers.get(session.workername())
        {
            worker.retire_session(session.id());
        }
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

    pub(crate) fn total_sessions(&self) -> usize {
        self.users.iter().map(|user| user.session_count()).sum()
    }

    pub(crate) fn disconnected(&self) -> usize {
        self.disconnected.len()
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
        self.users.iter().map(|user| user.worker_count()).sum()
    }

    pub(crate) fn total_work(&self) -> f64 {
        self.users.iter().map(|user| user.total_work()).sum()
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
            "sps={:.2}  hashrate={:.2}  sessions={}  users={}  workers={}  accepted={}  rejected={}  blocks={}  uptime={}s",
            self.sps_1m(),
            self.hashrate_1m(),
            self.total_sessions(),
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

    fn test_auth(enonce1: &str, workername: &str) -> Arc<Authorization> {
        Arc::new(Authorization {
            enonce1: enonce1.parse().unwrap(),
            address: test_address(),
            workername: workername.into(),
            username: Username::new(format!(
                "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.{workername}"
            )),
            version_mask: None,
        })
    }

    #[test]
    fn new_metatron_starts_at_zero() {
        let metatron = Metatron::new(pool_extranonces(), String::new());
        assert_eq!(metatron.total_sessions(), 0);
        assert_eq!(metatron.accepted(), 0);
        assert_eq!(metatron.rejected(), 0);
        assert_eq!(metatron.total_blocks(), 0);
        assert_eq!(metatron.total_users(), 0);
        assert_eq!(metatron.total_workers(), 0);
    }

    #[test]
    fn session_count_tracks_active_sessions() {
        let metatron = Metatron::new(pool_extranonces(), String::new());
        assert_eq!(metatron.total_sessions(), 0);

        let s1 = metatron.new_session(test_auth("deadbeef", "foo"));
        let s2 = metatron.new_session(test_auth("cafebabe", "foo"));
        assert_eq!(metatron.total_sessions(), 2);

        s1.deactivate();
        assert_eq!(metatron.total_sessions(), 1);

        s2.deactivate();
        assert_eq!(metatron.total_sessions(), 0);
    }

    #[test]
    fn new_session_creates_user_and_worker() {
        let metatron = Metatron::new(pool_extranonces(), String::new());

        metatron.new_session(test_auth("deadbeef", "rig1"));
        assert_eq!(metatron.total_users(), 1);
        assert_eq!(metatron.total_workers(), 1);

        metatron.new_session(test_auth("cafebabe", "rig2"));
        assert_eq!(metatron.total_users(), 1);
        assert_eq!(metatron.total_workers(), 2);
    }

    #[test]
    fn record_accepted_updates_stats() {
        let metatron = Metatron::new(pool_extranonces(), String::new());
        let session = metatron.new_session(test_auth("deadbeef", "rig1"));

        let pool_diff = Difficulty::from(1000.0);
        let share_diff = Difficulty::from(1500.0);

        session.record_accepted(pool_diff, share_diff);
        session.record_accepted(pool_diff, share_diff);

        assert_eq!(metatron.accepted(), 2);
        assert_eq!(metatron.rejected(), 0);
    }

    #[test]
    fn record_rejected_updates_stats() {
        let metatron = Metatron::new(pool_extranonces(), String::new());
        let session = metatron.new_session(test_auth("deadbeef", "rig1"));

        session.record_rejected();
        session.record_rejected();

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
        let pool_diff = Difficulty::from(100.0);
        let pool_diff_f64 = pool_diff.as_f64();

        assert_eq!(metatron.total_work(), 0.0);

        let foo_session = metatron.new_session(test_auth("deadbeef", "foo"));
        foo_session.record_accepted(pool_diff, Difficulty::from(200.0));
        foo_session.record_accepted(pool_diff, Difficulty::from(50.0));

        assert!(
            (metatron.total_work() - 2.0 * pool_diff_f64).abs()
                < f64::EPSILON * 2.0 * pool_diff_f64
        );

        let bar_session = metatron.new_session(test_auth("cafebabe", "bar"));
        bar_session.record_accepted(pool_diff, Difficulty::from(400.0));

        assert!(
            (metatron.total_work() - 3.0 * pool_diff_f64).abs()
                < f64::EPSILON * 3.0 * pool_diff_f64
        );
    }
}
