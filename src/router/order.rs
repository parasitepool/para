use super::*;

const RAMP_UP_SHARES: u64 = 3;
const HYSTERESIS_LOW: f64 = 0.95;
const HYSTERESIS_HIGH: f64 = 1.5;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderStatus {
    Pending,
    Active,
    Fulfilled,
    Cancelled,
    Disconnected,
    Expired,
    PaidLate,
}

#[derive(Copy, Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderKind {
    Sink,
    Bucket(HashDays),
}

impl Display for OrderKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sink => f.write_str("sink"),
            Self::Bucket(_) => f.write_str("bucket"),
        }
    }
}

pub struct Order {
    pub(crate) id: u32,
    pub(crate) upstream_target: UpstreamTarget,
    pub(crate) kind: OrderKind,
    pub(crate) upstream: OnceLock<Arc<Upstream>>,
    pub(crate) allocator: OnceLock<Arc<EnonceAllocator>>,
    pub(crate) status: Mutex<OrderStatus>,
    pub(crate) payment_address: Address,
    pub(crate) payment_derivation_index: u32,
    pub(crate) payment_amount: Amount,
    pub(crate) payment_timeout: Duration,
    pub(crate) created_at: Instant,
    pub(crate) cancel: CancellationToken,
    pub(crate) metatron: Arc<Metatron>,
    pub(crate) assigned: AtomicUsize,
    pub(crate) sessions: Mutex<HashMap<SessionId, (Arc<Session>, CancellationToken)>>,
}

impl Order {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        id: u32,
        upstream_target: UpstreamTarget,
        kind: OrderKind,
        payment_address: Address,
        payment_derivation_index: u32,
        payment_amount: Amount,
        payment_timeout: Duration,
        cancel: CancellationToken,
        metatron: Arc<Metatron>,
    ) -> Arc<Self> {
        Arc::new(Self {
            id,
            upstream_target,
            kind,
            upstream: OnceLock::new(),
            allocator: OnceLock::new(),
            status: Mutex::new(OrderStatus::Pending),
            payment_address,
            payment_derivation_index,
            payment_amount,
            payment_timeout,
            created_at: Instant::now(),
            cancel,
            metatron,
            assigned: AtomicUsize::new(0),
            sessions: Mutex::new(HashMap::new()),
        })
    }

    pub(crate) fn assign(&self) {
        self.assigned.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn unassign(&self) {
        self.assigned.fetch_sub(1, Ordering::Relaxed);
    }

    pub(crate) fn add_session(&self, session: Arc<Session>, cancel: CancellationToken) {
        self.sessions.lock().insert(session.id(), (session, cancel));
    }

    pub(crate) fn remove_session(&self, id: SessionId) {
        self.sessions.lock().remove(&id);
    }

    pub(crate) fn is_sink(&self) -> bool {
        matches!(self.kind, OrderKind::Sink)
    }

    pub(crate) fn is_starving(&self, now: Instant) -> bool {
        let OrderKind::Bucket(hashdays) = self.kind else {
            return false;
        };

        self.hashrate_1m(now).0 < hashdays.target_hashrate().0 * HYSTERESIS_LOW
    }

    pub(crate) fn upstream(&self) -> Option<&Arc<Upstream>> {
        self.upstream.get()
    }

    pub(crate) fn allocator(&self) -> Option<&Arc<EnonceAllocator>> {
        self.allocator.get()
    }

    pub(crate) fn status(&self) -> OrderStatus {
        *self.status.lock()
    }

    pub(crate) fn set_status(&self, status: OrderStatus) {
        *self.status.lock() = status;
    }

    pub(crate) fn hashrate_1m(&self, now: Instant) -> HashRate {
        self.metatron
            .downstream_stats(self.id, now)
            .hashrate_1m(now)
    }

    pub(crate) async fn activate(
        &self,
        timeout: Duration,
        enonce1_extension_size: usize,
        tasks: &TaskTracker,
    ) -> Result {
        let upstream = Upstream::connect(
            self.id,
            &self.upstream_target,
            timeout,
            self.cancel.clone(),
            tasks,
            self.metatron.clone(),
        )
        .await?;

        let proxy_extranonces = ProxyExtranonces::new(
            upstream.enonce1().clone(),
            upstream.enonce2_size(),
            enonce1_extension_size,
        )?;

        let allocator = Arc::new(EnonceAllocator::new(
            Extranonces::Proxy(proxy_extranonces),
            self.id,
        ));

        self.upstream
            .set(upstream)
            .map_err(|_| anyhow!("activate called twice"))?;

        self.allocator
            .set(allocator)
            .map_err(|_| anyhow!("activate called twice"))?;

        info!("Upstream {} connected", self.upstream_target);

        Ok(())
    }

    pub(crate) fn is_fulfilled(&self) -> bool {
        let OrderKind::Bucket(target) = self.kind else {
            return false;
        };

        self.metatron.upstream_accepted_work(self.id).to_hash_days() >= target
    }

    pub(crate) fn remaining_work(&self) -> TotalWork {
        match self.kind {
            OrderKind::Bucket(target) => {
                target.to_total_work() - self.metatron.upstream_accepted_work(self.id)
            }
            OrderKind::Sink => TotalWork::ZERO,
        }
    }

    pub(crate) fn is_ramping_up(&self) -> bool {
        let active = self.assigned.load(Ordering::Relaxed);

        if active == 0 {
            return false;
        }

        let sessions = self.sessions.lock();

        sessions.len() < active
            || sessions
                .values()
                .any(|(session, _)| session.accepted_shares() < RAMP_UP_SHARES)
    }

    pub(crate) fn trim(&self) {
        let now = Instant::now();

        let OrderKind::Bucket(hashdays) = self.kind else {
            return;
        };

        let target = hashdays.target_hashrate();
        let current = self.hashrate_1m(now);

        if current.0 <= target.0 * HYSTERESIS_HIGH {
            return;
        }

        let min = current - HashRate(target.0 * HYSTERESIS_HIGH);
        let max = current - target;

        let mut best = None;

        for (session, _) in self.sessions.lock().values() {
            let hashrate = session.hashrate_1m(now);

            if hashrate < min || hashrate > max {
                continue;
            }

            let replace = match best {
                None => true,
                Some((_, best_hashrate)) => hashrate > best_hashrate,
            };

            if replace {
                best = Some((session.id(), hashrate));
            }
        }

        let Some((id, _)) = best else {
            return;
        };

        self.trim_session(id, now);
    }

    pub(crate) fn trim_session(&self, id: SessionId, now: Instant) {
        let sessions = self.sessions.lock();

        let Some((session, cancel)) = sessions.get(&id) else {
            return;
        };

        info!(
            "Trimming session {id} ({}) from order {} at {}",
            session.hashrate_1m(now),
            self.id,
            self.upstream_target,
        );

        cancel.cancel();
    }

    pub(crate) fn terminate(&self, to: OrderStatus) -> Option<OrderStatus> {
        let previous = {
            let mut status = self.status.lock();

            if !matches!(*status, OrderStatus::Pending | OrderStatus::Active) {
                return None;
            }

            let previous = *status;

            *status = to;

            previous
        };

        self.cancel.cancel();

        Some(previous)
    }

    pub(crate) fn force_cancel(&self) -> OrderStatus {
        let previous = {
            let mut status = self.status.lock();
            let previous = *status;
            *status = OrderStatus::Cancelled;
            previous
        };

        self.cancel.cancel();

        previous
    }

    pub(crate) fn transition(&self, from: OrderStatus, to: OrderStatus) -> bool {
        let mut status = self.status.lock();
        if *status == from {
            *status = to;

            true
        } else {
            false
        }
    }

    pub(crate) fn ready_for_activation(&self, received: Amount, elapsed: Duration) -> bool {
        if elapsed >= self.payment_timeout {
            self.transition(OrderStatus::Pending, OrderStatus::Expired);
        }

        if received < self.payment_amount {
            return false;
        }

        match self.status() {
            OrderStatus::Pending => true,
            OrderStatus::Expired => {
                self.transition(OrderStatus::Expired, OrderStatus::PaidLate);
                false
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_address() -> Address {
        "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc"
            .parse::<Address<NetworkUnchecked>>()
            .unwrap()
            .assume_checked()
    }

    fn test_order(metatron: &Arc<Metatron>, kind: OrderKind) -> Arc<Order> {
        Order::new(
            0,
            "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.worker@bar:3333"
                .parse()
                .unwrap(),
            kind,
            test_address(),
            0,
            Amount::from_sat(1000),
            Duration::from_secs(3600),
            CancellationToken::new(),
            metatron.clone(),
        )
    }

    fn test_authorization(enonce1: &str) -> Arc<crate::stratifier::state::Authorization> {
        Arc::new(crate::stratifier::state::Authorization {
            enonce1: enonce1.parse().unwrap(),
            address: test_address(),
            workername: "foo".into(),
            username: "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.foo"
                .parse()
                .unwrap(),
            version_mask: None,
        })
    }

    fn register_session(
        metatron: &Metatron,
        bucket: &Order,
        enonce1: &str,
        difficulty: f64,
    ) -> CancellationToken {
        let session = metatron.new_session(test_authorization(enonce1), 0);
        session.record_accepted(Difficulty::from(difficulty), Difficulty::from(difficulty));
        let cancel = CancellationToken::new();
        bucket.add_session(session, cancel.clone());
        cancel
    }

    #[test]
    fn trim_sink_is_noop() {
        let metatron = Arc::new(Metatron::new());
        let sink = test_order(&metatron, OrderKind::Sink);
        sink.trim();
    }

    #[test]
    fn trim_noop_when_not_overshooting() {
        let metatron = Arc::new(Metatron::new());
        let bucket = test_order(&metatron, OrderKind::Bucket(HashDays::new(1e9).unwrap()));

        let cancel = register_session(&metatron, &bucket, "deadbeef", 1.0);

        bucket.trim();
        assert!(!cancel.is_cancelled());
    }

    #[test]
    fn trim_noop_when_single_session_overshoots() {
        let metatron = Arc::new(Metatron::new());
        let bucket = test_order(&metatron, OrderKind::Bucket(HashDays::new(1.0).unwrap()));

        let cancel = register_session(&metatron, &bucket, "deadbeef", 10_000.0);

        bucket.trim();
        assert!(!cancel.is_cancelled());
    }

    #[test]
    fn trim_noop_when_all_sessions_too_fat() {
        let metatron = Arc::new(Metatron::new());
        let bucket = test_order(&metatron, OrderKind::Bucket(HashDays::new(1e9).unwrap()));

        let cancel_a = register_session(&metatron, &bucket, "aaaa", 12.0);
        let cancel_b = register_session(&metatron, &bucket, "bbbb", 12.0);

        bucket.trim();
        assert!(!cancel_a.is_cancelled());
        assert!(!cancel_b.is_cancelled());
    }

    #[test]
    fn trim_picks_fattest_in_band() {
        let metatron = Arc::new(Metatron::new());
        let bucket = test_order(&metatron, OrderKind::Bucket(HashDays::new(1e9).unwrap()));

        let cancel_fat = register_session(&metatron, &bucket, "aaaa", 13.0);
        let cancel_mid = register_session(&metatron, &bucket, "bbbb", 7.0);
        let cancel_small = register_session(&metatron, &bucket, "cccc", 4.0);

        bucket.trim();
        assert!(!cancel_fat.is_cancelled());
        assert!(cancel_mid.is_cancelled());
        assert!(!cancel_small.is_cancelled());
    }

    #[test]
    fn is_starving_false_for_sink() {
        let metatron = Arc::new(Metatron::new());
        let sink = test_order(&metatron, OrderKind::Sink);
        assert!(!sink.is_starving(Instant::now()));
    }

    #[test]
    fn is_starving_true_for_bucket_below_low_threshold() {
        let metatron = Arc::new(Metatron::new());
        let bucket = test_order(&metatron, OrderKind::Bucket(HashDays::new(1e20).unwrap()));
        assert!(bucket.is_starving(Instant::now()));
    }

    #[test]
    fn is_starving_false_for_bucket_above_low_threshold() {
        let metatron = Arc::new(Metatron::new());
        let bucket = test_order(&metatron, OrderKind::Bucket(HashDays::new(1.0).unwrap()));
        register_session(&metatron, &bucket, "deadbeef", 10_000.0);
        assert!(!bucket.is_starving(Instant::now()));
    }

    #[test]
    fn is_ramping_up_false_when_idle() {
        let metatron = Arc::new(Metatron::new());
        let bucket = test_order(&metatron, OrderKind::Bucket(HashDays::new(100.0).unwrap()));
        assert!(!bucket.is_ramping_up());
    }

    #[test]
    fn is_ramping_up_true_when_assign_outpaces_session() {
        let metatron = Arc::new(Metatron::new());
        let bucket = test_order(&metatron, OrderKind::Bucket(HashDays::new(100.0).unwrap()));

        bucket.assign();

        assert!(bucket.is_ramping_up());
    }

    #[test]
    fn is_ramping_up_until_n_shares() {
        let metatron = Arc::new(Metatron::new());
        let bucket = test_order(&metatron, OrderKind::Bucket(HashDays::new(1e20).unwrap()));

        bucket.assign();

        let session = metatron.new_session(test_authorization("deadbeef"), 0);
        bucket.add_session(session.clone(), CancellationToken::new());

        assert!(bucket.is_ramping_up());

        for _ in 0..RAMP_UP_SHARES {
            session.record_accepted(Difficulty::from(1.0), Difficulty::from(1.0));
        }

        assert!(!bucket.is_ramping_up());
    }
}
