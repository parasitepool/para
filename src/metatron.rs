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
    counter: AtomicU32,
    disconnected: DashMap<Extranonce, (Arc<Session>, Instant)>,
    started: Instant,
    orders: DashMap<u32, Mutex<Stats>>,
    users: DashMap<Address, Arc<User>>,
}

impl Metatron {
    pub(crate) fn new() -> Self {
        Self {
            blocks: AtomicU64::new(0),
            counter: AtomicU32::new(0),
            disconnected: DashMap::new(),
            started: Instant::now(),
            orders: DashMap::new(),
            users: DashMap::new(),
        }
    }

    pub(crate) fn spawn(self: &Arc<Self>, cancel: CancellationToken, tasks: &TaskTracker) {
        info!("Spawning metatron session cleanup task");

        let metatron = self.clone();

        tasks.spawn(async move {
            let mut cleanup_interval = ticker(Duration::from_secs(60));

            loop {
                tokio::select! {
                    biased;

                    _ = cancel.cancelled() => {
                        info!("Shutting down metatron");
                        break;
                    }

                    _ = cleanup_interval.tick() => {
                        metatron.disconnected.retain(|_, (_, disconnected_at)| {
                            disconnected_at.elapsed() < SESSION_TTL
                        });

                        info!("{}", metatron.status_line());
                    }
                }
            }
        });
    }

    pub(crate) fn new_session(&self, auth: Arc<Authorization>, order_id: u32) -> Arc<Session> {
        let id = SessionId::new(order_id, self.counter.fetch_add(1, Ordering::Relaxed));

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

    pub(crate) fn resume_session(&self, enonce1: &Extranonce, order_id: u32) -> bool {
        self.disconnected
            .remove_if(enonce1, |_, (session, _)| {
                session.id().order_id() == order_id
            })
            .is_some()
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
            .flat_map(|user| user.sessions())
            .filter(|session| session.is_idle(now))
            .count()
    }

    pub(crate) fn users(&self) -> &DashMap<Address, Arc<User>> {
        &self.users
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

    pub(crate) fn record_order_accepted(
        &self,
        order_id: u32,
        upstream_diff: Difficulty,
        share_diff: Difficulty,
    ) {
        let now = Instant::now();
        if let Some(entry) = self.orders.get(&order_id) {
            entry.lock().record_accepted(upstream_diff, share_diff, now);
            return;
        }

        self.orders
            .entry(order_id)
            .or_insert_with(|| Mutex::new(Stats::new()))
            .lock()
            .record_accepted(upstream_diff, share_diff, now);
    }

    pub(crate) fn record_order_rejected(&self, order_id: u32, upstream_diff: Difficulty) {
        if let Some(entry) = self.orders.get(&order_id) {
            entry.lock().record_rejected(upstream_diff);
            return;
        }

        self.orders
            .entry(order_id)
            .or_insert_with(|| Mutex::new(Stats::new()))
            .lock()
            .record_rejected(upstream_diff);
    }

    pub(crate) fn order_stats(&self, order_id: u32) -> Stats {
        self.orders
            .get(&order_id)
            .map(|entry| entry.lock().clone())
            .unwrap_or_default()
    }

    pub(crate) fn order_accepted_work(&self, order_id: u32) -> TotalWork {
        self.orders
            .get(&order_id)
            .map(|entry| entry.lock().accepted_work)
            .unwrap_or(TotalWork::ZERO)
    }

    pub(crate) fn downstream_stats(&self, order_id: u32, now: Instant) -> Stats {
        self.users
            .iter()
            .flat_map(|user| user.sessions())
            .filter(|session| session.id().order_id() == order_id)
            .fold(Stats::new(), |mut combined, session| {
                combined.absorb(session.snapshot(), now);
                combined
            })
    }

    pub(crate) fn downstream_snapshot(
        &self,
        order_id: u32,
        now: Instant,
    ) -> (Vec<Arc<Session>>, Stats) {
        let mut sessions = Vec::new();
        let mut stats = Stats::new();

        for session in self.users.iter().flat_map(|user| user.sessions()) {
            if session.id().order_id() != order_id {
                continue;
            }

            stats.absorb(session.snapshot(), now);
            sessions.push(session);
        }

        (sessions, stats)
    }

    #[cfg(test)]
    pub(crate) fn set_order_accepted_work(&self, order_id: u32, work: TotalWork) {
        self.orders
            .entry(order_id)
            .or_insert_with(|| Mutex::new(Stats::new()))
            .lock()
            .accepted_work = work;
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

    fn test_auth(enonce1: &str, workername: &str) -> Arc<Authorization> {
        Arc::new(Authorization {
            enonce1: enonce1.parse().unwrap(),
            address: test_address(),
            workername: workername.into(),
            username: format!("tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.{workername}")
                .parse()
                .unwrap(),
            version_mask: None,
        })
    }

    #[test]
    fn new_metatron_starts_at_zero() {
        let metatron = Metatron::new();
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
        let metatron = Metatron::new();
        assert_eq!(metatron.total_sessions(), 0);

        let s1 = metatron.new_session(test_auth("deadbeef", "foo"), 0);
        let s2 = metatron.new_session(test_auth("cafebabe", "foo"), 0);
        assert_eq!(metatron.total_sessions(), 2);

        metatron.retire_session(s1);
        assert_eq!(metatron.total_sessions(), 1);

        metatron.retire_session(s2);
        assert_eq!(metatron.total_sessions(), 0);
    }

    #[test]
    fn new_session_creates_user_and_worker() {
        let metatron = Metatron::new();

        metatron.new_session(test_auth("deadbeef", "rig1"), 0);
        assert_eq!(metatron.total_users(), 1);
        assert_eq!(metatron.total_workers(), 1);

        metatron.new_session(test_auth("cafebabe", "rig2"), 0);
        assert_eq!(metatron.total_users(), 1);
        assert_eq!(metatron.total_workers(), 2);
    }

    #[test]
    fn record_share_updates_stats() {
        let metatron = Metatron::new();
        let session = metatron.new_session(test_auth("deadbeef", "foo"), 0);

        session.record_accepted(Difficulty::from(1000.0), Difficulty::from(1500.0));
        session.record_accepted(Difficulty::from(1000.0), Difficulty::from(1500.0));
        session.record_rejected(Difficulty::from(500.0));

        let stats = metatron.snapshot();
        assert_eq!(stats.accepted_shares, 2);
        assert_eq!(stats.rejected_shares, 1);
    }

    #[test]
    fn block_count_increments() {
        let metatron = Metatron::new();
        metatron.add_block();
        assert_eq!(metatron.total_blocks(), 1);
    }

    #[test]
    fn accepted_work_accumulates() {
        let metatron = Metatron::new();
        let pool_diff = Difficulty::from(100.0);
        let expected = TotalWork::from_difficulty(pool_diff);

        assert_eq!(metatron.snapshot().accepted_work, TotalWork::ZERO);

        let foo_session = metatron.new_session(test_auth("deadbeef", "foo"), 0);
        foo_session.record_accepted(pool_diff, Difficulty::from(200.0));
        foo_session.record_accepted(pool_diff, Difficulty::from(50.0));

        assert_eq!(metatron.snapshot().accepted_work, expected + expected);

        let bar_session = metatron.new_session(test_auth("cafebabe", "bar"), 0);
        bar_session.record_accepted(pool_diff, Difficulty::from(400.0));

        assert_eq!(
            metatron.snapshot().accepted_work,
            expected + expected + expected
        );
    }

    #[test]
    fn store_and_take_disconnected() {
        let metatron = Metatron::new();
        let enonce1: Extranonce = "deadbeef".parse().unwrap();
        assert!(!metatron.resume_session(&enonce1, 0));

        let session = metatron.new_session(test_auth("deadbeef", "foo"), 0);
        metatron.retire_session(session);
        assert_eq!(metatron.total_disconnected(), 1);

        assert!(metatron.resume_session(&enonce1, 0));
        assert_eq!(metatron.total_disconnected(), 0);
    }

    #[test]
    fn retire_session_folds_stats() {
        let metatron = Metatron::new();
        let session = metatron.new_session(test_auth("deadbeef", "foo"), 0);

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
        let metatron = Metatron::new();
        let s1 = metatron.new_session(test_auth("deadbeef", "foo"), 0);
        let s2 = metatron.new_session(test_auth("cafebabe", "foo"), 0);

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
        let metatron = Metatron::new();
        let s1 = metatron.new_session(test_auth("deadbeef", "foo"), 0);
        let s2 = metatron.new_session(test_auth("cafebabe", "foo"), 0);

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
    fn take_disconnected_rejects_wrong_order() {
        let metatron = Metatron::new();

        let enonce1: Extranonce = "deadbeef".parse().unwrap();
        let session = metatron.new_session(test_auth("deadbeef", "foo"), 1);
        metatron.retire_session(session);

        assert!(!metatron.resume_session(&enonce1, 0));
        assert!(metatron.resume_session(&enonce1, 1));
    }

    #[test]
    fn order_sessions_are_isolated() {
        let metatron = Metatron::new();
        let now = Instant::now();

        let s1 = metatron.new_session(test_auth("deadbeef", "foo"), 0);
        let s2 = metatron.new_session(test_auth("cafebabe", "bar"), 1);

        let (sessions0, _) = metatron.downstream_snapshot(0, now);
        let (sessions1, _) = metatron.downstream_snapshot(1, now);

        assert_eq!(sessions0.len(), 1);
        assert_eq!(sessions1.len(), 1);
        assert_eq!(sessions0[0].id(), s1.id());
        assert_eq!(sessions1[0].id(), s2.id());
    }

    #[test]
    fn downstream_snapshot_stats_only_include_requested_order() {
        let metatron = Metatron::new();
        let now = Instant::now();

        let s1 = metatron.new_session(test_auth("deadbeef", "foo"), 0);
        let s2 = metatron.new_session(test_auth("cafebabe", "bar"), 1);

        s1.record_accepted(Difficulty::from(100.0), Difficulty::from(200.0));
        s2.record_rejected(Difficulty::from(300.0));

        let (_, stats0) = metatron.downstream_snapshot(0, now);
        let (_, stats1) = metatron.downstream_snapshot(1, now);

        assert_eq!(stats0.accepted_shares, 1);
        assert_eq!(stats0.rejected_shares, 0);
        assert_eq!(stats1.accepted_shares, 0);
        assert_eq!(stats1.rejected_shares, 1);
    }

    #[test]
    fn retire_removes_session_from_downstream_queries() {
        let metatron = Metatron::new();
        let now = Instant::now();

        let session = metatron.new_session(test_auth("deadbeef", "foo"), 0);
        let (sessions, _) = metatron.downstream_snapshot(0, now);
        assert_eq!(sessions.len(), 1);

        metatron.retire_session(session);

        let (sessions, _) = metatron.downstream_snapshot(0, now);
        assert!(sessions.is_empty());
    }

    #[test]
    fn order_stats_record_accepted_accumulates() {
        let metatron = Metatron::new();
        let upstream_diff = Difficulty::from(100.0);

        metatron.record_order_accepted(0, upstream_diff, Difficulty::from(150.0));
        metatron.record_order_accepted(0, upstream_diff, Difficulty::from(400.0));

        let stats = metatron.order_stats(0);
        assert_eq!(stats.accepted_shares, 2);
        assert_eq!(stats.rejected_shares, 0);
        let expected = TotalWork::from_difficulty(upstream_diff);
        assert_eq!(stats.accepted_work, expected + expected);
        assert_eq!(stats.best_share, Some(Difficulty::from(400.0)));
        assert!(stats.last_share.is_some());
    }

    #[test]
    fn order_stats_record_rejected_accumulates() {
        let metatron = Metatron::new();
        let upstream_diff = Difficulty::from(100.0);

        metatron.record_order_rejected(0, upstream_diff);
        metatron.record_order_rejected(0, upstream_diff);

        let stats = metatron.order_stats(0);
        assert_eq!(stats.accepted_shares, 0);
        assert_eq!(stats.rejected_shares, 2);
        let expected = TotalWork::from_difficulty(upstream_diff);
        assert_eq!(stats.rejected_work, expected + expected);
    }

    #[test]
    fn order_stats_isolated_between_orders() {
        let metatron = Metatron::new();
        let upstream_diff = Difficulty::from(100.0);

        metatron.record_order_accepted(0, upstream_diff, Difficulty::from(200.0));
        metatron.record_order_rejected(1, upstream_diff);

        let stats0 = metatron.order_stats(0);
        let stats1 = metatron.order_stats(1);

        assert_eq!(stats0.accepted_shares, 1);
        assert_eq!(stats0.rejected_shares, 0);
        assert_eq!(stats1.accepted_shares, 0);
        assert_eq!(stats1.rejected_shares, 1);
    }

    #[test]
    fn order_accepted_work_matches_recorded_diff() {
        let metatron = Metatron::new();
        let upstream_diff = Difficulty::from(250.0);

        assert_eq!(metatron.order_accepted_work(0), TotalWork::ZERO);

        metatron.record_order_accepted(0, upstream_diff, Difficulty::from(300.0));
        let expected = TotalWork::from_difficulty(upstream_diff);
        assert_eq!(metatron.order_accepted_work(0), expected);

        metatron.record_order_accepted(0, upstream_diff, Difficulty::from(300.0));
        assert_eq!(metatron.order_accepted_work(0), expected + expected);
    }

    #[test]
    fn order_stats_unknown_id_returns_default() {
        let metatron = Metatron::new();
        let stats = metatron.order_stats(999);
        assert_eq!(stats.accepted_shares, 0);
        assert_eq!(stats.rejected_shares, 0);
        assert_eq!(stats.accepted_work, TotalWork::ZERO);
        assert_eq!(stats.best_share, None);
    }

    #[test]
    fn order_best_share_keeps_max() {
        let metatron = Metatron::new();

        metatron.record_order_accepted(0, Difficulty::from(50.0), Difficulty::from(200.0));
        metatron.record_order_accepted(0, Difficulty::from(50.0), Difficulty::from(50.0));
        metatron.record_order_accepted(0, Difficulty::from(50.0), Difficulty::from(800.0));

        assert_eq!(
            metatron.order_stats(0).best_share,
            Some(Difficulty::from(800.0))
        );
    }
}
