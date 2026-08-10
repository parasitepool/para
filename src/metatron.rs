use {
    super::*,
    bdk_wallet::ChangeSet,
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

struct OrderSlot {
    stats: Mutex<Stats>,
    sessions: DashMap<SessionId, Arc<Session>>,
}

impl OrderSlot {
    fn new() -> Self {
        Self {
            stats: Mutex::new(Stats::new()),
            sessions: DashMap::new(),
        }
    }

    fn with_stats(stats: Stats) -> Self {
        Self {
            stats: Mutex::new(stats),
            sessions: DashMap::new(),
        }
    }
}

pub(crate) struct Metatron {
    store: Arc<Store>,
    blocks: RwLock<Vec<BlockHash>>,
    counter: AtomicU32,
    disconnected: DashMap<Extranonce, (Arc<Session>, Instant, Arc<EnonceAllocator>)>,
    started: Instant,
    orders: DashMap<u32, OrderSlot>,
    users: DashMap<Address, Arc<User>>,
    persist_lock: Mutex<()>,
}

pub(crate) struct DownstreamSnapshot {
    pub(crate) traffic: Stats,
    pub(crate) total: Stats,
    pub(crate) users: usize,
    pub(crate) workers: usize,
    pub(crate) sessions: usize,
    pub(crate) idle: usize,
    pub(crate) total_workers: usize,
}

impl Metatron {
    pub(crate) fn open(store: Arc<Store>) -> Result<Self> {
        let users = store
            .read_users()?
            .into_iter()
            .map(|(address, entry)| {
                let user = Arc::new(User::from_entry(address.clone(), entry)?);
                Ok((address, user))
            })
            .collect::<Result<_>>()?;

        let blocks = store.read_blocks()?;

        Ok(Self {
            store,
            blocks: RwLock::new(blocks),
            counter: AtomicU32::new(0),
            disconnected: DashMap::new(),
            started: Instant::now(),
            orders: DashMap::new(),
            users,
            persist_lock: Mutex::new(()),
        })
    }

    pub(crate) fn store(&self) -> &Arc<Store> {
        &self.store
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
                        metatron.cleanup_expired(Instant::now());
                        info!("{}", metatron.status_line());
                    }
                }
            }
        });
    }

    fn cleanup_expired(&self, now: Instant) {
        self.disconnected
            .retain(|_, (session, disconnected_at, allocator)| {
                let keep = now.duration_since(*disconnected_at) < SESSION_TTL;
                if !keep {
                    allocator.release_enonce1(session.enonce1());
                }
                keep
            });

        self.users
            .retain(|_, user| user.session_count() > 0 || user.has_accepted());
    }

    pub(crate) fn new_session(&self, auth: Arc<Authorization>, order_id: u32) -> Arc<Session> {
        let id = SessionId::new(order_id, self.counter.fetch_add(1, Ordering::Relaxed));

        let session = {
            let user = self
                .users
                .entry(auth.address.clone())
                .or_insert_with(|| Arc::new(User::new(auth.address.clone())));

            let session = Arc::new(Session::new(
                id,
                auth.enonce1.clone(),
                auth.address.clone(),
                auth.workername.clone(),
                auth.username.clone(),
                auth.version_mask,
                user.dirty.clone(),
            ));

            user.new_session(&auth.workername, session.clone());

            session
        };

        self.orders
            .entry(order_id)
            .or_insert_with(OrderSlot::new)
            .sessions
            .insert(id, session.clone());

        session
    }

    pub(crate) fn retire_session(&self, session: Arc<Session>, allocator: Arc<EnonceAllocator>) {
        if let Some(user) = self.users.get(session.address())
            && let Some(worker) = user.workers.get(session.workername())
            && worker.retire_session(session.id())
        {
            user.mark_dirty();
        }

        if let Some(slot) = self.orders.get(&session.id().order_id()) {
            slot.sessions.remove(&session.id());
        }

        self.disconnected.insert(
            session.enonce1().clone(),
            (session, Instant::now(), allocator),
        );
    }

    pub(crate) fn resume_session(&self, enonce1: &Extranonce, order_id: u32) -> bool {
        self.disconnected
            .remove_if(enonce1, |_, (session, _, _)| {
                session.id().order_id() == order_id
            })
            .is_some()
    }

    pub(crate) fn disconnected_info(
        &self,
        enonce1: &Extranonce,
        now: Instant,
    ) -> Option<(u32, HashRate)> {
        self.disconnected.get(enonce1).map(|entry| {
            let (session, _, _) = entry.value();
            (session.id().order_id(), session.hashrate_1m(now))
        })
    }

    pub(crate) fn evict_oldest_disconnected(&self, order_id: u32) -> bool {
        let oldest_key = self
            .disconnected
            .iter()
            .filter(|entry| entry.value().0.id().order_id() == order_id)
            .min_by_key(|entry| entry.value().1)
            .map(|entry| entry.key().clone());

        let Some(key) = oldest_key else {
            return false;
        };

        if let Some((_, (session, _, allocator))) = self.disconnected.remove(&key) {
            allocator.release_enonce1(session.enonce1());
            true
        } else {
            false
        }
    }

    pub(crate) fn record_block(&self, blockhash: BlockHash) {
        self.blocks.write().push(blockhash);
    }

    pub(crate) fn snapshot(&self) -> Stats {
        let now = Instant::now();

        self.users.iter().fold(Stats::new(), |mut combined, user| {
            combined.absorb(user.snapshot(), now);
            combined
        })
    }

    pub(crate) fn downstream(&self, now: Instant) -> DownstreamSnapshot {
        let mut traffic = Stats::new();
        let mut total = Stats::new();
        let mut addresses = HashSet::new();
        let mut workers = HashSet::new();
        let mut sessions = 0;
        let mut idle = 0;
        let mut total_workers = 0;

        for user in self.users.iter() {
            let live_workers = workers.len();

            for worker in user.workers() {
                total_workers += 1;
                total.absorb(worker.lifetime(), now);

                if worker.session_count() == 0 {
                    continue;
                }

                workers.insert((user.address.clone(), worker.workername().to_string()));

                for session in worker.sessions() {
                    let snapshot = session.snapshot();

                    sessions += 1;

                    if session.is_idle(now) {
                        idle += 1;
                    }

                    traffic.absorb(snapshot.clone(), now);
                    total.absorb(snapshot, now);
                }
            }

            if workers.len() > live_workers {
                addresses.insert(user.address.clone());
            }
        }

        for entry in self.disconnected.iter() {
            let session = &entry.value().0;
            traffic.absorb(session.snapshot(), now);
            addresses.insert(session.address().clone());
            workers.insert((session.address().clone(), session.workername().to_string()));
        }

        DownstreamSnapshot {
            traffic,
            total,
            users: addresses.len(),
            workers: workers.len(),
            sessions,
            idle,
            total_workers,
        }
    }

    pub(crate) fn block_count(&self) -> usize {
        self.blocks.read().len()
    }

    pub(crate) fn recent_blocks(&self, n: usize) -> Vec<BlockHash> {
        self.blocks.read().iter().rev().take(n).copied().collect()
    }

    pub(crate) fn total_sessions(&self) -> usize {
        self.users.iter().map(|user| user.session_count()).sum()
    }

    pub(crate) fn total_disconnected(&self) -> usize {
        self.disconnected.len()
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
        if let Some(slot) = self.orders.get(&order_id) {
            slot.stats
                .lock()
                .record_accepted(upstream_diff, share_diff, now);
            return;
        }

        self.orders
            .entry(order_id)
            .or_insert_with(OrderSlot::new)
            .stats
            .lock()
            .record_accepted(upstream_diff, share_diff, now);
    }

    pub(crate) fn record_order_rejected(&self, order_id: u32, upstream_diff: Difficulty) {
        if let Some(slot) = self.orders.get(&order_id) {
            slot.stats.lock().record_rejected(upstream_diff);
            return;
        }

        self.orders
            .entry(order_id)
            .or_insert_with(OrderSlot::new)
            .stats
            .lock()
            .record_rejected(upstream_diff);
    }

    pub(crate) fn order_stats(&self, order_id: u32) -> Stats {
        self.orders
            .get(&order_id)
            .map(|slot| slot.stats.lock().clone())
            .unwrap_or_default()
    }

    pub(crate) fn restore_order_stats(&self, order_id: u32, stats: Stats) {
        self.orders.insert(order_id, OrderSlot::with_stats(stats));
    }

    pub(crate) fn remove_order(&self, order_id: u32) {
        self.orders.remove(&order_id);
    }

    pub(crate) fn persist(
        &self,
        orders: &[(u32, store::entry::OrderEntry)],
        wallet_delta: &ChangeSet,
    ) -> Result {
        let _guard = self.persist_lock.lock();

        let start = Instant::now();

        let mut users = Vec::new();

        for user in self.users.iter() {
            let user = user.value();

            if !user.take_dirty() {
                continue;
            }

            let entry = user.to_entry(start);

            if entry
                .workers
                .iter()
                .any(|worker| worker.stats.accepted_shares > 0)
            {
                users.push((user.clone(), entry));
            }
        }

        let result = (|| {
            let txn = self.store.begin()?;

            for (id, order) in orders {
                txn.insert_order(*id, order)?;
            }

            txn.merge_wallet(wallet_delta)?;

            for (user, entry) in &users {
                txn.upsert_user(&user.address, entry)?;
            }

            txn.write_blocks(&self.blocks.read())?;
            txn.commit()
        })();

        if let Err(err) = result {
            for (user, _) in &users {
                user.mark_dirty();
            }

            return Err(err);
        }

        debug!("persist took {:?}", start.elapsed());

        Ok(())
    }

    pub(crate) fn persist_order(
        &self,
        id: u32,
        order: &store::entry::OrderEntry,
        wallet_delta: &ChangeSet,
    ) -> Result {
        let txn = self.store.begin()?;

        txn.insert_order(id, order)?;
        txn.merge_wallet(wallet_delta)?;

        txn.commit()
    }

    pub(crate) fn order_delivered_work(&self, order_id: u32) -> HashWork {
        self.orders
            .get(&order_id)
            .map(|slot| slot.stats.lock().delivered_work())
            .unwrap_or(HashWork::ZERO)
    }

    pub(crate) fn downstream_stats(&self, order_id: u32, now: Instant) -> Stats {
        let Some(slot) = self.orders.get(&order_id) else {
            return Stats::new();
        };

        slot.sessions
            .iter()
            .fold(Stats::new(), |mut combined, entry| {
                combined.absorb(entry.value().snapshot(), now);
                combined
            })
    }

    pub(crate) fn downstream_snapshot(
        &self,
        order_id: u32,
        now: Instant,
    ) -> (Vec<Arc<Session>>, Stats) {
        let Some(slot) = self.orders.get(&order_id) else {
            return (Vec::new(), Stats::new());
        };

        let mut sessions = Vec::new();
        let mut stats = Stats::new();

        for entry in slot.sessions.iter() {
            stats.absorb(entry.value().snapshot(), now);
            sessions.push(entry.value().clone());
        }

        (sessions, stats)
    }

    #[cfg(test)]
    pub(crate) fn set_order_delivered_work(&self, order_id: u32, work: HashWork) {
        let rejected = HashWork::new(1.0).unwrap();
        let slot = self.orders.entry(order_id).or_insert_with(OrderSlot::new);
        let mut stats = slot.stats.lock();
        stats.accepted_work = work - rejected;
        stats.rejected_work = rejected;
    }

    #[cfg(test)]
    pub(crate) fn test() -> (Self, tempfile::TempDir) {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(&directory.path().join("test.redb"), Chain::Regtest).unwrap();
        let metatron = Self::open(Arc::new(store)).unwrap();
        (metatron, directory)
    }

    #[cfg(test)]
    pub(crate) fn test_with_store(store: Arc<Store>) -> Self {
        Self::open(store).unwrap()
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
            self.block_count(),
            self.uptime().as_secs()
        )
    }
}

#[cfg(test)]
mod tests {
    use {super::*, std::thread};

    fn test_address() -> Address {
        "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc"
            .parse::<Address<bitcoin::address::NetworkUnchecked>>()
            .unwrap()
            .assume_checked()
    }

    fn test_address_2() -> Address {
        "tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx"
            .parse::<Address<bitcoin::address::NetworkUnchecked>>()
            .unwrap()
            .assume_checked()
    }

    fn test_allocator() -> Arc<EnonceAllocator> {
        Arc::new(EnonceAllocator::new(
            Extranonces::Pool(PoolExtranonces::new(4, 8).unwrap()),
            0,
        ))
    }

    fn auth(address: Address, enonce1: &str, workername: &str) -> Arc<Authorization> {
        Arc::new(Authorization {
            enonce1: enonce1.parse().unwrap(),
            username: format!("{address}.{workername}").parse().unwrap(),
            address,
            workername: workername.into(),
            version_mask: None,
        })
    }

    fn test_auth(enonce1: &str, workername: &str) -> Arc<Authorization> {
        auth(test_address(), enonce1, workername)
    }

    fn test_auth_2(enonce1: &str, workername: &str) -> Arc<Authorization> {
        auth(test_address_2(), enonce1, workername)
    }

    #[test]
    fn new_metatron_starts_at_zero() {
        let (metatron, _dir) = Metatron::test();
        let stats = metatron.snapshot();
        assert_eq!(metatron.total_sessions(), 0);
        assert_eq!(stats.accepted_shares, 0);
        assert_eq!(stats.rejected_shares, 0);
        assert_eq!(metatron.block_count(), 0);
        assert_eq!(metatron.total_users(), 0);
        assert_eq!(metatron.total_workers(), 0);
    }

    #[test]
    fn session_count_tracks_active_sessions() {
        let (metatron, _dir) = Metatron::test();
        assert_eq!(metatron.total_sessions(), 0);

        let s1 = metatron.new_session(test_auth("deadbeef", "foo"), 0);
        let s2 = metatron.new_session(test_auth("cafebabe", "foo"), 0);
        assert_eq!(metatron.total_sessions(), 2);

        metatron.retire_session(s1, test_allocator());
        assert_eq!(metatron.total_sessions(), 1);

        metatron.retire_session(s2, test_allocator());
        assert_eq!(metatron.total_sessions(), 0);
    }

    #[test]
    fn new_session_creates_user_and_worker() {
        let (metatron, _dir) = Metatron::test();

        metatron.new_session(test_auth("deadbeef", "rig1"), 0);
        assert_eq!(metatron.total_users(), 1);
        assert_eq!(metatron.total_workers(), 1);

        metatron.new_session(test_auth("cafebabe", "rig2"), 0);
        assert_eq!(metatron.total_users(), 1);
        assert_eq!(metatron.total_workers(), 2);
    }

    #[test]
    fn record_share_updates_stats() {
        let (metatron, _dir) = Metatron::test();
        let session = metatron.new_session(test_auth("deadbeef", "foo"), 0);

        session.record_accepted(Difficulty::from(1000.0), Difficulty::from(1500.0));
        session.record_accepted(Difficulty::from(1000.0), Difficulty::from(1500.0));
        session.record_rejected(Difficulty::from(500.0));

        let stats = metatron.snapshot();
        assert_eq!(stats.accepted_shares, 2);
        assert_eq!(stats.rejected_shares, 1);
    }

    #[test]
    fn record_block_stores_hash() {
        let (metatron, _dir) = Metatron::test();

        let h1 = BlockHash::from_byte_array([1u8; 32]);
        let h2 = BlockHash::from_byte_array([2u8; 32]);

        metatron.record_block(h1);
        assert_eq!(metatron.block_count(), 1);
        assert_eq!(metatron.recent_blocks(1), vec![h1]);

        metatron.record_block(h2);
        assert_eq!(metatron.block_count(), 2);
        assert_eq!(metatron.recent_blocks(1), vec![h2]);
        assert_eq!(metatron.recent_blocks(2), vec![h2, h1]);
    }

    #[test]
    fn accepted_work_accumulates() {
        let (metatron, _dir) = Metatron::test();
        let pool_diff = Difficulty::from(100.0);
        let expected = HashWork::from_difficulty(pool_diff);

        assert_eq!(metatron.snapshot().accepted_work, HashWork::ZERO);

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
        let (metatron, _dir) = Metatron::test();
        let enonce1: Extranonce = "deadbeef".parse().unwrap();
        assert!(!metatron.resume_session(&enonce1, 0));

        let session = metatron.new_session(test_auth("deadbeef", "foo"), 0);
        metatron.retire_session(session, test_allocator());
        assert_eq!(metatron.total_disconnected(), 1);

        assert!(metatron.resume_session(&enonce1, 0));
        assert_eq!(metatron.total_disconnected(), 0);
    }

    #[test]
    fn retire_session_folds_stats() {
        let (metatron, _dir) = Metatron::test();
        let session = metatron.new_session(test_auth("deadbeef", "foo"), 0);

        let pool_diff = Difficulty::from(100.0);
        session.record_accepted(pool_diff, Difficulty::from(200.0));
        session.record_accepted(pool_diff, Difficulty::from(50.0));
        session.record_rejected(pool_diff);
        metatron.retire_session(session, test_allocator());

        let stats = metatron.snapshot();
        assert_eq!(metatron.total_sessions(), 0);
        assert_eq!(stats.accepted_shares, 2);
        assert_eq!(stats.rejected_shares, 1);
        assert_eq!(stats.best_share, Some(Difficulty::from(200.0)));
        assert!(stats.last_share.is_some());
        let expected = HashWork::from_difficulty(pool_diff);
        assert_eq!(stats.accepted_work, expected + expected);
        assert_eq!(stats.rejected_work, expected);
    }

    #[test]
    fn downstream_excludes_retired_lifetime() {
        let (metatron, _dir) = Metatron::test();
        let now = Instant::now();

        let s1 = metatron.new_session(test_auth("deadbeef", "foo"), 0);
        let s2 = metatron.new_session(test_auth("cafebabe", "bar"), 0);

        let pool_diff = Difficulty::from(100.0);
        s1.record_accepted(pool_diff, Difficulty::from(200.0));
        s2.record_accepted(pool_diff, Difficulty::from(300.0));

        assert_eq!(metatron.downstream(now).traffic.accepted_shares, 2);

        metatron.retire_session(s1, test_allocator());
        metatron.retire_session(s2, test_allocator());

        assert_eq!(metatron.downstream(now).traffic.accepted_shares, 2);
        assert_eq!(metatron.downstream(now).total.accepted_shares, 2);

        metatron.cleanup_expired(Instant::now() + SESSION_TTL + Duration::from_secs(1));

        assert_eq!(metatron.downstream(now).traffic.accepted_shares, 0);
        assert_eq!(metatron.downstream(now).total.accepted_shares, 2);
    }

    #[test]
    fn downstream_counts_track_live_and_disconnected() {
        let (metatron, _dir) = Metatron::test();
        let now = Instant::now();

        metatron.new_session(test_auth("deadbeef", "foo"), 0);
        let s2 = metatron.new_session(test_auth_2("cafebabe", "bar"), 0);

        let downstream = metatron.downstream(now);
        assert_eq!(downstream.users, 2);
        assert_eq!(downstream.workers, 2);

        s2.record_accepted(Difficulty::from(100.0), Difficulty::from(200.0));
        metatron.retire_session(s2, test_allocator());

        let downstream = metatron.downstream(now);
        assert_eq!(downstream.users, 2);
        assert_eq!(downstream.workers, 2);

        metatron.cleanup_expired(Instant::now() + SESSION_TTL + Duration::from_secs(1));

        let downstream = metatron.downstream(now);
        assert_eq!(downstream.users, 1);
        assert_eq!(downstream.workers, 1);
        assert_eq!(metatron.total_users(), 2);
        assert_eq!(metatron.total_workers(), 2);
    }

    #[test]
    fn retire_accumulates_across_multiple_sessions() {
        let (metatron, _dir) = Metatron::test();
        let s1 = metatron.new_session(test_auth("deadbeef", "foo"), 0);
        let s2 = metatron.new_session(test_auth("cafebabe", "foo"), 0);

        let pool_diff = Difficulty::from(100.0);
        s1.record_accepted(pool_diff, Difficulty::from(50.0));
        s2.record_accepted(pool_diff, Difficulty::from(300.0));
        metatron.retire_session(s1, test_allocator());
        metatron.retire_session(s2, test_allocator());

        let stats = metatron.snapshot();
        assert_eq!(stats.accepted_shares, 2);
        assert_eq!(stats.best_share, Some(Difficulty::from(300.0)));
        let expected = HashWork::from_difficulty(pool_diff);
        assert_eq!(stats.accepted_work, expected + expected);
    }

    #[test]
    fn stats_combine_active_sessions_and_lifetime() {
        let (metatron, _dir) = Metatron::test();
        let s1 = metatron.new_session(test_auth("deadbeef", "foo"), 0);
        let s2 = metatron.new_session(test_auth("cafebabe", "foo"), 0);

        let pool_diff = Difficulty::from(100.0);
        s1.record_accepted(pool_diff, Difficulty::from(50.0));
        s2.record_accepted(pool_diff, Difficulty::from(200.0));
        metatron.retire_session(s1, test_allocator());

        let stats = metatron.snapshot();
        assert_eq!(stats.accepted_shares, 2);
        assert_eq!(stats.best_share, Some(Difficulty::from(200.0)));
        let expected = HashWork::from_difficulty(pool_diff);
        assert_eq!(stats.accepted_work, expected + expected);
    }

    #[test]
    fn take_disconnected_rejects_wrong_order() {
        let (metatron, _dir) = Metatron::test();

        let enonce1: Extranonce = "deadbeef".parse().unwrap();
        let session = metatron.new_session(test_auth("deadbeef", "foo"), 1);
        metatron.retire_session(session, test_allocator());

        assert!(!metatron.resume_session(&enonce1, 0));
        assert!(metatron.resume_session(&enonce1, 1));
    }

    #[test]
    fn evict_oldest_disconnected_picks_oldest() {
        let upstream_enonce1 = Extranonce::from_bytes(&[0xde, 0xad, 0xbe, 0xef]);
        let allocator = Arc::new(EnonceAllocator::new(
            Extranonces::Proxy(ProxyExtranonces::new(upstream_enonce1, 8, 1).unwrap()),
            0,
        ));

        let (metatron, _dir) = Metatron::test();

        let mut enonces = Vec::new();
        for name in ["foo", "bar", "baz"] {
            let enonce1 = allocator.next_enonce1().unwrap();
            enonces.push(enonce1.clone());
            let session = metatron.new_session(test_auth(&enonce1.to_string(), name), 0);
            metatron.retire_session(session, allocator.clone());
            thread::sleep(Duration::from_millis(1));
        }

        assert_eq!(allocator.allocated_count(), 3);

        assert!(metatron.evict_oldest_disconnected(0));

        assert_eq!(allocator.allocated_count(), 2);
        assert_eq!(metatron.total_disconnected(), 2);
        assert!(!metatron.resume_session(&enonces[0], 0));
        assert!(metatron.resume_session(&enonces[1], 0));
    }

    #[test]
    fn evict_oldest_disconnected_respects_order() {
        let upstream_enonce1 = Extranonce::from_bytes(&[0xde, 0xad, 0xbe, 0xef]);
        let allocator = Arc::new(EnonceAllocator::new(
            Extranonces::Proxy(ProxyExtranonces::new(upstream_enonce1, 8, 2).unwrap()),
            0,
        ));

        let (metatron, _dir) = Metatron::test();

        let e0 = allocator.next_enonce1().unwrap();
        let s0 = metatron.new_session(test_auth(&e0.to_string(), "foo"), 0);
        metatron.retire_session(s0, allocator.clone());

        thread::sleep(Duration::from_millis(1));

        let e1 = allocator.next_enonce1().unwrap();
        let s1 = metatron.new_session(test_auth(&e1.to_string(), "bar"), 1);
        metatron.retire_session(s1, allocator.clone());

        assert!(metatron.evict_oldest_disconnected(1));

        assert_eq!(allocator.allocated_count(), 1);
        assert!(metatron.resume_session(&e0, 0));
        assert!(!metatron.resume_session(&e1, 1));
    }

    #[test]
    fn evict_oldest_disconnected_none_returns_false() {
        let (metatron, _dir) = Metatron::test();

        assert!(!metatron.evict_oldest_disconnected(0));

        let session = metatron.new_session(test_auth("deadbeef", "foo"), 0);
        metatron.retire_session(session, test_allocator());

        assert!(!metatron.evict_oldest_disconnected(1));
        assert_eq!(metatron.total_disconnected(), 1);
    }

    #[test]
    fn cleanup_releases_expired_enonce1() {
        let upstream_enonce1 = Extranonce::from_bytes(&[0xde, 0xad, 0xbe, 0xef]);
        let allocator = Arc::new(EnonceAllocator::new(
            Extranonces::Proxy(ProxyExtranonces::new(upstream_enonce1, 8, 1).unwrap()),
            0,
        ));

        let enonce1 = allocator.next_enonce1().unwrap();

        let (metatron, _dir) = Metatron::test();

        let session = metatron.new_session(test_auth(&enonce1.to_string(), "foo"), 0);
        metatron.retire_session(session, allocator.clone());

        assert_eq!(allocator.allocated_count(), 1);
        assert_eq!(metatron.total_disconnected(), 1);

        metatron.cleanup_expired(Instant::now());
        assert_eq!(metatron.total_disconnected(), 1);
        assert_eq!(allocator.allocated_count(), 1);

        metatron.cleanup_expired(Instant::now() + SESSION_TTL + Duration::from_secs(1));
        assert_eq!(metatron.total_disconnected(), 0);
        assert_eq!(allocator.allocated_count(), 0);
    }

    #[test]
    fn order_sessions_are_isolated() {
        let (metatron, _dir) = Metatron::test();
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
        let (metatron, _dir) = Metatron::test();
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
        let (metatron, _dir) = Metatron::test();
        let now = Instant::now();

        let session = metatron.new_session(test_auth("deadbeef", "foo"), 0);
        let (sessions, _) = metatron.downstream_snapshot(0, now);
        assert_eq!(sessions.len(), 1);

        metatron.retire_session(session, test_allocator());

        let (sessions, _) = metatron.downstream_snapshot(0, now);
        assert!(sessions.is_empty());
    }

    #[test]
    fn order_stats_record_accepted_accumulates() {
        let (metatron, _dir) = Metatron::test();
        let upstream_diff = Difficulty::from(100.0);

        metatron.record_order_accepted(0, upstream_diff, Difficulty::from(150.0));
        metatron.record_order_accepted(0, upstream_diff, Difficulty::from(400.0));

        let stats = metatron.order_stats(0);
        assert_eq!(stats.accepted_shares, 2);
        assert_eq!(stats.rejected_shares, 0);
        let expected = HashWork::from_difficulty(upstream_diff);
        assert_eq!(stats.accepted_work, expected + expected);
        assert_eq!(stats.best_share, Some(Difficulty::from(400.0)));
        assert!(stats.last_share.is_some());
    }

    #[test]
    fn order_stats_record_rejected_accumulates() {
        let (metatron, _dir) = Metatron::test();
        let upstream_diff = Difficulty::from(100.0);

        metatron.record_order_rejected(0, upstream_diff);
        metatron.record_order_rejected(0, upstream_diff);

        let stats = metatron.order_stats(0);
        assert_eq!(stats.accepted_shares, 0);
        assert_eq!(stats.rejected_shares, 2);
        let expected = HashWork::from_difficulty(upstream_diff);
        assert_eq!(stats.rejected_work, expected + expected);
    }

    #[test]
    fn order_stats_isolated_between_orders() {
        let (metatron, _dir) = Metatron::test();
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
    fn order_delivered_work_matches_recorded_diff() {
        let (metatron, _dir) = Metatron::test();
        let upstream_diff = Difficulty::from(250.0);

        assert_eq!(metatron.order_delivered_work(0), HashWork::ZERO);

        metatron.record_order_accepted(0, upstream_diff, Difficulty::from(300.0));
        let expected = HashWork::from_difficulty(upstream_diff);
        assert_eq!(metatron.order_delivered_work(0), expected);

        metatron.record_order_accepted(0, upstream_diff, Difficulty::from(300.0));
        assert_eq!(metatron.order_delivered_work(0), expected + expected);

        metatron.record_order_rejected(0, upstream_diff);
        assert_eq!(
            metatron.order_delivered_work(0),
            expected + expected + expected
        );
    }

    #[test]
    fn order_stats_unknown_id_returns_default() {
        let (metatron, _dir) = Metatron::test();
        let stats = metatron.order_stats(999);
        assert_eq!(stats.accepted_shares, 0);
        assert_eq!(stats.rejected_shares, 0);
        assert_eq!(stats.accepted_work, HashWork::ZERO);
        assert_eq!(stats.best_share, None);
    }

    #[test]
    fn order_best_share_keeps_max() {
        let (metatron, _dir) = Metatron::test();

        metatron.record_order_accepted(0, Difficulty::from(50.0), Difficulty::from(200.0));
        metatron.record_order_accepted(0, Difficulty::from(50.0), Difficulty::from(50.0));
        metatron.record_order_accepted(0, Difficulty::from(50.0), Difficulty::from(800.0));

        assert_eq!(
            metatron.order_stats(0).best_share,
            Some(Difficulty::from(800.0))
        );
    }

    #[test]
    fn record_marks_user_dirty() {
        let (metatron, _dir) = Metatron::test();
        let session = metatron.new_session(test_auth("deadbeef", "foo"), 0);

        let user = metatron.users().get(&test_address()).unwrap().clone();
        assert!(user.is_dirty());
        assert!(!user.has_accepted());

        assert!(user.take_dirty());
        session.record_rejected(Difficulty::from(100.0));
        assert!(user.is_dirty());
        assert!(!user.has_accepted());

        assert!(user.take_dirty());
        session.record_accepted(Difficulty::from(100.0), Difficulty::from(150.0));
        assert!(user.is_dirty());
        assert!(user.has_accepted());
    }

    #[test]
    fn persist_skips_users_without_accepted_work() {
        let (metatron, _dir) = Metatron::test();
        let session = metatron.new_session(test_auth("deadbeef", "foo"), 0);

        metatron.persist(&[], &ChangeSet::default()).unwrap();
        assert!(metatron.store().read_users().unwrap().is_empty());

        session.record_rejected(Difficulty::from(100.0));
        metatron.persist(&[], &ChangeSet::default()).unwrap();
        assert!(metatron.store().read_users().unwrap().is_empty());

        session.record_accepted(Difficulty::from(100.0), Difficulty::from(150.0));
        metatron.persist(&[], &ChangeSet::default()).unwrap();

        let users = metatron.store().read_users().unwrap();
        assert_eq!(users.len(), 1);
        assert_eq!(users[0].1.workers.len(), 1);
        assert_eq!(users[0].1.workers[0].stats.accepted_shares, 1);
        assert!(!metatron.users().get(&test_address()).unwrap().is_dirty());
    }

    #[test]
    fn persist_upserts_only_dirty_users() {
        let (metatron, _dir) = Metatron::test();
        let foo = metatron.new_session(test_auth("deadbeef", "foo"), 0);
        metatron.new_session(test_auth_2("cafebabe", "bar"), 0);

        foo.record_accepted(Difficulty::from(100.0), Difficulty::from(200.0));
        metatron.persist(&[], &ChangeSet::default()).unwrap();

        let users = metatron.store().read_users().unwrap();
        assert_eq!(users.len(), 1);
        assert_eq!(users[0].0, test_address());

        for user in metatron.users().iter() {
            assert!(!user.is_dirty());
        }

        foo.record_accepted(Difficulty::from(100.0), Difficulty::from(300.0));
        metatron.persist(&[], &ChangeSet::default()).unwrap();

        let users = metatron.store().read_users().unwrap();
        assert_eq!(users.len(), 1);
        assert_eq!(users[0].1.workers[0].stats.accepted_shares, 2);
        assert_eq!(
            users[0].1.workers[0].stats.best_share,
            Some(Difficulty::from(300.0))
        );
    }

    #[test]
    fn zero_work_user_pruned_after_sessions_end() {
        let (metatron, _dir) = Metatron::test();
        let session = metatron.new_session(test_auth("deadbeef", "foo"), 0);

        metatron.cleanup_expired(Instant::now());
        assert_eq!(metatron.total_users(), 1);

        metatron.retire_session(session, test_allocator());
        metatron.cleanup_expired(Instant::now());
        assert_eq!(metatron.total_users(), 0);

        metatron.persist(&[], &ChangeSet::default()).unwrap();
        assert!(metatron.store().read_users().unwrap().is_empty());
    }

    #[test]
    fn pruned_then_resurrected_user_keeps_stats() {
        let (metatron, _dir) = Metatron::test();
        let session = metatron.new_session(test_auth("deadbeef", "foo"), 0);
        metatron.retire_session(session, test_allocator());
        metatron.cleanup_expired(Instant::now());
        assert_eq!(metatron.total_users(), 0);

        let session = metatron.new_session(test_auth("cafebabe", "foo"), 0);
        session.record_accepted(Difficulty::from(100.0), Difficulty::from(200.0));
        metatron.persist(&[], &ChangeSet::default()).unwrap();

        let users = metatron.store().read_users().unwrap();
        assert_eq!(users.len(), 1);
        assert_eq!(users[0].1.workers[0].stats.accepted_shares, 1);
        assert_eq!(
            users[0].1.workers[0].stats.best_share,
            Some(Difficulty::from(200.0))
        );
    }

    #[test]
    fn worked_user_survives_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let store =
            Arc::new(Store::open(&directory.path().join("test.redb"), Chain::Regtest).unwrap());

        {
            let metatron = Metatron::test_with_store(store.clone());
            let session = metatron.new_session(test_auth("deadbeef", "foo"), 0);
            session.record_accepted(Difficulty::from(100.0), Difficulty::from(200.0));
            metatron.retire_session(session, test_allocator());
            metatron.persist(&[], &ChangeSet::default()).unwrap();
        }

        let metatron = Metatron::test_with_store(store);

        let stats = metatron.snapshot();
        assert_eq!(stats.accepted_shares, 1);
        assert_eq!(stats.best_share, Some(Difficulty::from(200.0)));

        let user = metatron.users().get(&test_address()).unwrap();
        assert!(user.has_accepted());
        assert!(!user.is_dirty());
    }
}
