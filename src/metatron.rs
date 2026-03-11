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
    counter: AtomicU32,
}

impl Metatron {
    pub(crate) fn new() -> Self {
        Self {
            blocks: AtomicU64::new(0),
            started: Instant::now(),
            users: DashMap::new(),
            disconnected: DashMap::new(),
            counter: AtomicU32::new(0),
        }
    }

    pub(crate) fn spawn(self: Arc<Self>, cancel: CancellationToken, tasks: &TaskTracker) {
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

    pub(crate) fn new_session(&self, auth: Arc<Authorization>, upstream_id: u32) -> Arc<Session> {
        let id = SessionId::new(upstream_id, self.counter.fetch_add(1, Ordering::Relaxed));

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

    pub(crate) fn resume_session(&self, enonce1: &Extranonce, upstream_id: u32) -> bool {
        self.disconnected
            .remove_if(enonce1, |_, (session, _)| {
                session.id().upstream_id() == upstream_id
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

    pub(crate) fn disconnected(&self) -> &DashMap<Extranonce, (Arc<Session>, Instant)> {
        &self.disconnected
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
            username: Username::new(format!(
                "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.{workername}"
            )),
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
    fn take_disconnected_rejects_wrong_upstream() {
        let metatron = Metatron::new();

        let enonce1: Extranonce = "deadbeef".parse().unwrap();
        let session = metatron.new_session(test_auth("deadbeef", "foo"), 1);
        metatron.retire_session(session);

        assert!(!metatron.resume_session(&enonce1, 0));
        assert!(metatron.resume_session(&enonce1, 1));
    }
}
