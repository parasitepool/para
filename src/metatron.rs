use {
    super::*,
    session::{Session, SessionId},
    stats::Stats,
    stratifier::state::Authorization,
    user::User,
    worker::Worker,
};

pub(crate) mod session;
pub(crate) mod stats;
pub(crate) mod user;
pub(crate) mod worker;

pub(crate) struct Metatron {
    blocks: AtomicU64,
    started: Instant,
    users: DashMap<Address, Arc<User>>,
    disconnected: DashMap<Extranonce, (Arc<Session>, Instant)>,
    extranonces: RwLock<Extranonces>,
    enonce_counter: AtomicU64,
    upstream_id: u32,
    session_id_counter: AtomicU32,
    endpoint: String,
}

impl Metatron {
    pub(crate) fn new(extranonces: Extranonces, endpoint: String, upstream_id: u32) -> Self {
        Self {
            blocks: AtomicU64::new(0),
            started: Instant::now(),
            users: DashMap::new(),
            disconnected: DashMap::new(),
            extranonces: RwLock::new(extranonces),
            enonce_counter: AtomicU64::new(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
            ),
            upstream_id,
            session_id_counter: AtomicU32::new(0),
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
                        self.disconnected.retain(|_, (_, disconnected_at)| {
                            disconnected_at.elapsed() < SESSION_TTL
                        });

                        info!("{}", self.status_line());
                    }
                }
            }
        });
    }

    pub(crate) fn next_enonce1(&self) -> Extranonce {
        let counter = self.enonce_counter.fetch_add(1, Ordering::Relaxed);
        let extranonces = self.extranonces.read();

        match &*extranonces {
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
        self.extranonces.read().enonce2_size()
    }

    pub(crate) fn extranonces(&self) -> parking_lot::RwLockReadGuard<'_, Extranonces> {
        self.extranonces.read()
    }

    pub(crate) fn update_extranonces(&self, extranonces: Extranonces) {
        *self.extranonces.write() = extranonces;
    }

    pub(crate) fn new_session(&self, auth: Arc<Authorization>) -> Arc<Session> {
        let counter = self.session_id_counter.fetch_add(1, Ordering::Relaxed);
        let id = SessionId::new(self.upstream_id, counter);

        let session = Arc::new(Session::new(
            id,
            auth.enonce1.clone(),
            auth.address.clone(),
            auth.workername.clone(),
            auth.username.clone(),
            auth.version_mask,
        ));

        self.users
            .entry(auth.address.clone())
            .or_insert_with(|| Arc::new(User::new(auth.address.clone())))
            .new_session(&auth.workername, session.clone());

        session
    }

    pub(crate) fn retire_session(&self, session: Arc<Session>) {
        if let Some(user) = self.users.get(session.address())
            && let Some(worker) = user.workers.get(session.workername())
        {
            worker.retire_session(session.id());
        }

        self.disconnected
            .insert(session.enonce1().clone(), (session, Instant::now()));
    }

    pub(crate) fn take_disconnected(&self, enonce1: &Extranonce) -> bool {
        self.disconnected.remove(enonce1).is_some()
    }

    pub(crate) fn add_block(&self) {
        self.blocks.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn snapshot(&self) -> Stats {
        let now = Instant::now();

        self.users.iter().fold(Stats::new(), |mut combined, user| {
            combined.absorb(user.snapshot(), now);
            combined
        })
    }

    pub(crate) fn total_blocks(&self) -> u64 {
        self.blocks.load(Ordering::Relaxed)
    }

    pub(crate) fn total_sessions(&self) -> usize {
        self.users.iter().map(|user| user.session_count()).sum()
    }

    pub(crate) fn total_disconnected(&self) -> usize {
        self.disconnected.len()
    }

    pub(crate) fn total_idle(&self) -> usize {
        let now = Instant::now();

        self.users
            .iter()
            .map(|user| {
                user.workers()
                    .filter(|worker| {
                        worker
                            .snapshot()
                            .last_share
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

    pub(crate) fn uptime(&self) -> Duration {
        self.started.elapsed()
    }

    pub(crate) fn users(&self) -> &DashMap<Address, Arc<User>> {
        &self.users
    }
}

impl StatusLine for Metatron {
    fn status_line(&self) -> String {
        let now = Instant::now();
        let stats = self.snapshot();
        format!(
            "sps={:.2}  hashrate={:.2}  sessions={}  users={}  workers={}  accepted={}  rejected={}  blocks={}  uptime={}s",
            stats.sps_1m(now),
            stats.hashrate_1m(now),
            self.total_sessions(),
            self.total_users(),
            self.total_workers(),
            stats.accepted_shares,
            stats.rejected_shares,
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
        let metatron = Metatron::new(pool_extranonces(), String::new(), 0);
        let stats = metatron.snapshot();
        assert_eq!(metatron.total_sessions(), 0);
        assert_eq!(stats.accepted_shares, 0);
        assert_eq!(stats.rejected_shares, 0);
        assert_eq!(metatron.total_blocks(), 0);
        assert_eq!(metatron.total_users(), 0);
        assert_eq!(metatron.total_workers(), 0);
    }

    #[test]
    fn session_count_tracks_active_sessions() {
        let metatron = Metatron::new(pool_extranonces(), String::new(), 0);
        assert_eq!(metatron.total_sessions(), 0);

        let s1 = metatron.new_session(test_auth("deadbeef", "foo"));
        let s2 = metatron.new_session(test_auth("cafebabe", "foo"));
        assert_eq!(metatron.total_sessions(), 2);

        metatron.retire_session(s1);
        assert_eq!(metatron.total_sessions(), 1);

        metatron.retire_session(s2);
        assert_eq!(metatron.total_sessions(), 0);
    }

    #[test]
    fn new_session_creates_user_and_worker() {
        let metatron = Metatron::new(pool_extranonces(), String::new(), 0);

        metatron.new_session(test_auth("deadbeef", "rig1"));
        assert_eq!(metatron.total_users(), 1);
        assert_eq!(metatron.total_workers(), 1);

        metatron.new_session(test_auth("cafebabe", "rig2"));
        assert_eq!(metatron.total_users(), 1);
        assert_eq!(metatron.total_workers(), 2);
    }

    #[test]
    fn record_share_updates_stats() {
        let metatron = Metatron::new(pool_extranonces(), String::new(), 0);
        let session = metatron.new_session(test_auth("deadbeef", "foo"));

        session.record_accepted(Difficulty::from(1000.0), Difficulty::from(1500.0));
        session.record_accepted(Difficulty::from(1000.0), Difficulty::from(1500.0));
        session.record_rejected(Difficulty::from(500.0));

        let stats = metatron.snapshot();
        assert_eq!(stats.accepted_shares, 2);
        assert_eq!(stats.rejected_shares, 1);
    }

    #[test]
    fn block_count_increments() {
        let metatron = Metatron::new(pool_extranonces(), String::new(), 0);
        metatron.add_block();
        assert_eq!(metatron.total_blocks(), 1);
    }

    #[test]
    fn pool_enonce1() {
        let metatron = Metatron::new(pool_extranonces(), String::new(), 0);
        let e1 = metatron.next_enonce1();
        let e2 = metatron.next_enonce1();

        assert_eq!(e1.len(), ENONCE1_SIZE);

        let v1 = u32::from_le_bytes(e1.as_bytes().try_into().unwrap());
        let v2 = u32::from_le_bytes(e2.as_bytes().try_into().unwrap());
        assert_eq!(v2, v1 + 1);
    }

    #[test]
    fn next_enonce1_is_unique() {
        let metatron = Metatron::new(pool_extranonces(), String::new(), 0);
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1000 {
            let enonce = metatron.next_enonce1();
            assert!(seen.insert(enonce), "duplicate enonce1 generated");
        }
    }

    #[test]
    fn proxy_enonce1() {
        let upstream_enonce1 = Extranonce::from_bytes(&[0xde, 0xad, 0xbe, 0xef]);
        let extranonces =
            Extranonces::Proxy(ProxyExtranonces::new(upstream_enonce1.clone(), 8, 2).unwrap());
        let metatron = Metatron::new(extranonces, String::new(), 0);

        assert_eq!(metatron.enonce2_size(), 6);

        let e1 = metatron.next_enonce1();
        let e2 = metatron.next_enonce1();

        assert_eq!(e1.len(), 6);
        assert_eq!(&e1.as_bytes()[..4], upstream_enonce1.as_bytes());

        let ext1 = u16::from_le_bytes(e1.as_bytes()[4..6].try_into().unwrap());
        let ext2 = u16::from_le_bytes(e2.as_bytes()[4..6].try_into().unwrap());
        assert_eq!(ext2, ext1.wrapping_add(1));
    }

    #[test]
    fn proxy_mode_extension_size_1() {
        let upstream_enonce1 = Extranonce::from_bytes(&[0xde, 0xad, 0xbe, 0xef]);
        let extranonces =
            Extranonces::Proxy(ProxyExtranonces::new(upstream_enonce1.clone(), 8, 1).unwrap());
        let metatron = Metatron::new(extranonces, String::new(), 0);

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
    fn accepted_work_accumulates() {
        let metatron = Metatron::new(pool_extranonces(), String::new(), 0);
        let pool_diff = Difficulty::from(100.0);
        let expected = TotalWork::from_difficulty(pool_diff);

        assert_eq!(metatron.snapshot().accepted_work, TotalWork::ZERO);

        let foo_session = metatron.new_session(test_auth("deadbeef", "foo"));
        foo_session.record_accepted(pool_diff, Difficulty::from(200.0));
        foo_session.record_accepted(pool_diff, Difficulty::from(50.0));

        assert_eq!(metatron.snapshot().accepted_work, expected + expected);

        let bar_session = metatron.new_session(test_auth("cafebabe", "bar"));
        bar_session.record_accepted(pool_diff, Difficulty::from(400.0));

        assert_eq!(
            metatron.snapshot().accepted_work,
            expected + expected + expected
        );
    }

    #[test]
    fn store_and_take_disconnected() {
        let metatron = Metatron::new(pool_extranonces(), String::new(), 0);

        let enonce1: Extranonce = "deadbeef".parse().unwrap();
        assert!(!metatron.take_disconnected(&enonce1));

        let session = metatron.new_session(test_auth("deadbeef", "foo"));
        metatron.retire_session(session);
        assert_eq!(metatron.total_disconnected(), 1);

        assert!(metatron.take_disconnected(&enonce1));
        assert_eq!(metatron.total_disconnected(), 0);
    }

    #[test]
    fn retire_session_folds_stats() {
        let metatron = Metatron::new(pool_extranonces(), String::new(), 0);
        let session = metatron.new_session(test_auth("deadbeef", "foo"));

        let pool_diff = Difficulty::from(100.0);
        session.record_accepted(pool_diff, Difficulty::from(200.0));
        session.record_accepted(pool_diff, Difficulty::from(50.0));
        session.record_rejected(pool_diff);
        metatron.retire_session(session);

        let stats = metatron.snapshot();
        assert_eq!(metatron.total_sessions(), 0);
        assert_eq!(stats.accepted_shares, 2);
        assert_eq!(stats.rejected_shares, 1);
        assert_eq!(stats.best_share, Some(Difficulty::from(200.0)));
        assert!(stats.last_share.is_some());
        let expected = TotalWork::from_difficulty(pool_diff);
        assert_eq!(stats.accepted_work, expected + expected);
        assert_eq!(stats.rejected_work, expected);
    }

    #[test]
    fn retire_accumulates_across_multiple_sessions() {
        let metatron = Metatron::new(pool_extranonces(), String::new(), 0);
        let s1 = metatron.new_session(test_auth("deadbeef", "foo"));
        let s2 = metatron.new_session(test_auth("cafebabe", "foo"));

        let pool_diff = Difficulty::from(100.0);
        s1.record_accepted(pool_diff, Difficulty::from(50.0));
        s2.record_accepted(pool_diff, Difficulty::from(300.0));
        metatron.retire_session(s1);
        metatron.retire_session(s2);

        let stats = metatron.snapshot();
        assert_eq!(stats.accepted_shares, 2);
        assert_eq!(stats.best_share, Some(Difficulty::from(300.0)));
        let expected = TotalWork::from_difficulty(pool_diff);
        assert_eq!(stats.accepted_work, expected + expected);
    }

    #[test]
    fn stats_combine_active_sessions_and_lifetime() {
        let metatron = Metatron::new(pool_extranonces(), String::new(), 0);
        let s1 = metatron.new_session(test_auth("deadbeef", "foo"));
        let s2 = metatron.new_session(test_auth("cafebabe", "foo"));

        let pool_diff = Difficulty::from(100.0);
        s1.record_accepted(pool_diff, Difficulty::from(50.0));
        s2.record_accepted(pool_diff, Difficulty::from(200.0));
        metatron.retire_session(s1);

        let stats = metatron.snapshot();
        assert_eq!(stats.accepted_shares, 2);
        assert_eq!(stats.best_share, Some(Difficulty::from(200.0)));
        let expected = TotalWork::from_difficulty(pool_diff);
        assert_eq!(stats.accepted_work, expected + expected);
    }

    #[test]
    fn update_extranonces_changes_enonce_derivation() {
        let old_enonce1 = Extranonce::from_bytes(&[0xaa, 0xbb, 0xcc, 0xdd]);
        let extranonces =
            Extranonces::Proxy(ProxyExtranonces::new(old_enonce1.clone(), 8, 2).unwrap());
        let metatron = Metatron::new(extranonces, String::new(), 0);

        let before = metatron.next_enonce1();
        assert_eq!(&before.as_bytes()[..4], old_enonce1.as_bytes());
        assert_eq!(metatron.enonce2_size(), 6);

        let new_enonce1 = Extranonce::from_bytes(&[0x11, 0x22, 0x33, 0x44]);
        let new_extranonces =
            Extranonces::Proxy(ProxyExtranonces::new(new_enonce1.clone(), 8, 2).unwrap());
        metatron.update_extranonces(new_extranonces);

        let after = metatron.next_enonce1();
        assert_eq!(&after.as_bytes()[..4], new_enonce1.as_bytes());
        assert_eq!(metatron.enonce2_size(), 6);
    }

    #[test]
    fn update_extranonces_preserves_stats() {
        let old_enonce1 = Extranonce::from_bytes(&[0xaa, 0xbb, 0xcc, 0xdd]);
        let extranonces = Extranonces::Proxy(ProxyExtranonces::new(old_enonce1, 8, 2).unwrap());
        let metatron = Metatron::new(extranonces, String::new(), 0);

        let session = metatron.new_session(test_auth("deadbeef", "foo"));
        let pool_diff = Difficulty::from(100.0);
        session.record_accepted(pool_diff, Difficulty::from(200.0));
        metatron.retire_session(session);

        let new_enonce1 = Extranonce::from_bytes(&[0x11, 0x22, 0x33, 0x44]);
        let new_extranonces = Extranonces::Proxy(ProxyExtranonces::new(new_enonce1, 8, 2).unwrap());
        metatron.update_extranonces(new_extranonces);

        let stats = metatron.snapshot();
        assert_eq!(stats.accepted_shares, 1);
        assert_eq!(stats.best_share, Some(Difficulty::from(200.0)));
        assert_eq!(stats.accepted_work, TotalWork::from_difficulty(pool_diff));
    }
}
