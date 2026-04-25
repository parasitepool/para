use super::*;

const RAMP_UP_SHARES: u64 = 3;
const HYSTERESIS_LOW: f64 = 0.95;
const HYSTERESIS_HIGH: f64 = 1.5;

pub(crate) const PAYMENT_TIMEOUT: u32 = 6;
pub(crate) const RECLAIM_WINDOW: u32 = 144;

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

pub struct Payment {
    pub(crate) address: Address,
    pub(crate) derivation_index: u32,
    pub(crate) amount: Amount,
    pub(crate) outpoints: Mutex<HashSet<OutPoint>>,
}

impl Payment {
    pub(crate) fn new(address: Address, derivation_index: u32, amount: Amount) -> Self {
        Self {
            address,
            derivation_index,
            amount,
            outpoints: Mutex::new(HashSet::new()),
        }
    }

    pub(crate) fn record_outpoints(&self, new: &[OutPoint]) -> bool {
        let mut outpoints = self.outpoints.lock();
        let before = outpoints.len();
        outpoints.extend(new);
        outpoints.len() > before
    }
}

pub struct Order {
    pub(crate) id: u32,
    pub(crate) upstream_target: UpstreamTarget,
    pub(crate) kind: OrderKind,
    pub(crate) upstream: Mutex<Option<Arc<Upstream>>>,
    pub(crate) allocator: OnceLock<Arc<EnonceAllocator>>,
    pub(crate) status: Mutex<OrderStatus>,
    pub(crate) payment: Option<Payment>,
    pub(crate) created_at: Instant,
    pub(crate) created_at_height: u32,
    pub(crate) last_updated: Mutex<Instant>,
    pub(crate) cancel: CancellationToken,
    pub(crate) metatron: Arc<Metatron>,
    pub(crate) assigned: AtomicUsize,
    pub(crate) sessions: Mutex<HashMap<SessionId, (Arc<Session>, CancellationToken)>>,
}

impl Order {
    pub(crate) fn new(
        id: u32,
        upstream_target: UpstreamTarget,
        kind: OrderKind,
        payment: Option<Payment>,
        created_at_height: u32,
        cancel: CancellationToken,
        metatron: Arc<Metatron>,
    ) -> Arc<Self> {
        let now = Instant::now();

        Arc::new(Self {
            id,
            upstream_target,
            kind,
            upstream: Mutex::new(None),
            allocator: OnceLock::new(),
            status: Mutex::new(OrderStatus::Pending),
            payment,
            created_at: now,
            created_at_height,
            last_updated: Mutex::new(now),
            cancel,
            metatron,
            assigned: AtomicUsize::new(0),
            sessions: Mutex::new(HashMap::new()),
        })
    }

    pub(crate) fn touch(&self) {
        *self.last_updated.lock() = Instant::now();
    }

    pub(crate) fn assign(&self) {
        self.assigned.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn unassign(&self) {
        self.assigned.fetch_sub(1, Ordering::Relaxed);
    }

    pub(crate) fn add_session(&self, session: Arc<Session>, cancel: CancellationToken) {
        self.sessions.lock().insert(session.id(), (session, cancel));
        self.touch();
    }

    pub(crate) fn remove_session(&self, id: SessionId) {
        self.sessions.lock().remove(&id);
        self.touch();
    }

    pub(crate) fn cancel_sessions(&self) {
        let tokens: Vec<CancellationToken> = self
            .sessions
            .lock()
            .values()
            .map(|(_, cancel)| cancel.clone())
            .collect();

        for cancel in tokens {
            cancel.cancel();
        }
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

    pub(crate) fn upstream(&self) -> Option<Arc<Upstream>> {
        self.upstream.lock().clone()
    }

    pub(crate) fn allocator(&self) -> Option<&Arc<EnonceAllocator>> {
        self.allocator.get()
    }

    pub(crate) fn status(&self) -> OrderStatus {
        *self.status.lock()
    }

    pub(crate) fn set_status(&self, status: OrderStatus) {
        *self.status.lock() = status;
        self.touch();
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
        let (upstream, extranonces) = self
            .connect_upstream(timeout, enonce1_extension_size, tasks)
            .await?;

        let allocator = Arc::new(EnonceAllocator::new(extranonces, self.id));

        *self.upstream.lock() = Some(upstream);

        self.allocator
            .set(allocator)
            .map_err(|_| anyhow!("activate called twice"))?;

        info!("Upstream {} connected", self.upstream_target);

        Ok(())
    }

    pub(crate) async fn reconnect(
        &self,
        timeout: Duration,
        enonce1_extension_size: usize,
        tasks: &TaskTracker,
    ) -> Result {
        let (upstream, extranonces) = self
            .connect_upstream(timeout, enonce1_extension_size, tasks)
            .await?;

        self.allocator
            .get()
            .expect("reconnect called before activate")
            .update_extranonces(extranonces);

        *self.upstream.lock() = Some(upstream);

        info!("Upstream {} reconnected", self.upstream_target);

        Ok(())
    }

    async fn connect_upstream(
        &self,
        timeout: Duration,
        enonce1_extension_size: usize,
        tasks: &TaskTracker,
    ) -> Result<(Arc<Upstream>, Extranonces)> {
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

        Ok((upstream, Extranonces::Proxy(proxy_extranonces)))
    }

    pub(crate) fn stats(&self) -> Stats {
        self.metatron.order_stats(self.id)
    }

    pub(crate) fn accepted_work(&self) -> TotalWork {
        self.metatron.order_accepted_work(self.id)
    }

    pub(crate) fn is_fulfilled(&self) -> bool {
        let OrderKind::Bucket(target) = self.kind else {
            return false;
        };

        self.accepted_work().to_hash_days() >= target
    }

    pub(crate) fn remaining_work(&self) -> TotalWork {
        match self.kind {
            OrderKind::Bucket(target) => target.to_total_work() - self.accepted_work(),
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

        let current = self.hashrate_1m(now);
        let target = hashdays.target_hashrate();
        let ceiling = HashRate(target.0 * HYSTERESIS_HIGH);

        if current <= ceiling {
            return;
        }

        let mut min_trim = current - ceiling;
        let mut max_trim = current - target;

        let mut candidates: Vec<(SessionId, HashRate)> = self
            .sessions
            .lock()
            .values()
            .map(|(session, _)| (session.id(), session.hashrate_1m(now)))
            .collect();

        candidates.sort_by(|a, b| a.1.total_cmp(&b.1));
        candidates.reverse();

        for (id, hashrate) in candidates {
            if min_trim <= HashRate::ZERO {
                break;
            }

            if hashrate > max_trim {
                continue;
            }

            self.trim_session(id, now);
            min_trim -= hashrate;
            max_trim -= hashrate;
        }
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

        self.touch();
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

        self.touch();
        self.cancel.cancel();

        previous
    }

    pub(crate) fn transition(&self, from: OrderStatus, to: OrderStatus) -> bool {
        let mut status = self.status.lock();
        if *status == from {
            *status = to;
            drop(status);
            self.touch();
            true
        } else {
            false
        }
    }

    pub(crate) fn ready_for_activation(&self, received: Amount, tip: u32) -> bool {
        if tip >= self.created_at_height + PAYMENT_TIMEOUT {
            self.transition(OrderStatus::Pending, OrderStatus::Expired);
        }

        if received < self.payment.as_ref().expect("bucket has payment").amount {
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
        let payment = match kind {
            OrderKind::Sink => None,
            OrderKind::Bucket(_) => Some(Payment::new(test_address(), 0, Amount::from_sat(1000))),
        };
        Order::new(
            0,
            "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.worker@bar:3333"
                .parse()
                .unwrap(),
            kind,
            payment,
            0,
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
    fn trim_sheds_many_small_sessions() {
        let metatron = Arc::new(Metatron::new());
        let bucket = test_order(&metatron, OrderKind::Bucket(HashDays::new(1e9).unwrap()));

        let cancels: Vec<CancellationToken> = (0..10)
            .map(|i| {
                let enonce = format!("{i:08x}");
                register_session(&metatron, &bucket, &enonce, 3.0)
            })
            .collect();

        bucket.trim();

        let cancelled = cancels.iter().filter(|c| c.is_cancelled()).count();
        assert!(
            cancelled >= 2,
            "expected multiple small sessions trimmed, got {cancelled}",
        );
    }

    #[test]
    fn trim_stops_at_hysteresis_ceiling() {
        let metatron = Arc::new(Metatron::new());
        let bucket = test_order(&metatron, OrderKind::Bucket(HashDays::new(1e9).unwrap()));

        let cancels: Vec<CancellationToken> = (0..8)
            .map(|i| {
                let enonce = format!("{i:08x}");
                register_session(&metatron, &bucket, &enonce, 3.0)
            })
            .collect();

        bucket.trim();

        let surviving = cancels.iter().filter(|c| !c.is_cancelled()).count();
        assert!(
            surviving >= 1,
            "trim drained below ceiling instead of stopping at it",
        );
    }

    #[test]
    fn trim_skips_sessions_larger_than_headroom() {
        let metatron = Arc::new(Metatron::new());
        let bucket = test_order(&metatron, OrderKind::Bucket(HashDays::new(1e9).unwrap()));

        let cancel_huge = register_session(&metatron, &bucket, "aaaa", 20.0);
        let cancel_a = register_session(&metatron, &bucket, "bbbb", 3.0);
        let cancel_b = register_session(&metatron, &bucket, "cccc", 3.0);
        let cancel_c = register_session(&metatron, &bucket, "dddd", 3.0);
        let cancel_d = register_session(&metatron, &bucket, "eeee", 3.0);

        bucket.trim();

        assert!(!cancel_huge.is_cancelled());
        let trimmed = [&cancel_a, &cancel_b, &cancel_c, &cancel_d]
            .iter()
            .filter(|c| c.is_cancelled())
            .count();
        assert!(trimmed >= 1);
    }

    #[test]
    fn trim_skips_session_after_max_trim_shrinks() {
        let metatron = Arc::new(Metatron::new());
        let bucket = test_order(&metatron, OrderKind::Bucket(HashDays::new(1e9).unwrap()));

        let cancel_a = register_session(&metatron, &bucket, "aaaa", 18.0);
        let cancel_b = register_session(&metatron, &bucket, "bbbb", 17.0);
        let cancel_c = register_session(&metatron, &bucket, "cccc", 7.0);

        bucket.trim();

        assert!(cancel_a.is_cancelled());
        assert!(!cancel_b.is_cancelled());
        assert!(cancel_c.is_cancelled());
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

    #[test]
    fn record_outpoints() {
        let payment = Payment::new(test_address(), 0, Amount::from_sat(1000));
        let a = OutPoint::new(Txid::from_byte_array([1; 32]), 0);
        let b = OutPoint::new(Txid::from_byte_array([2; 32]), 0);

        assert!(!payment.record_outpoints(&[]));
        assert!(payment.record_outpoints(&[a]));
        assert!(!payment.record_outpoints(&[a]));
        assert!(payment.record_outpoints(&[a, b]));
        assert!(!payment.record_outpoints(&[a, b]));
        assert_eq!(payment.outpoints.lock().len(), 2);
    }

    #[test]
    fn cancel_sessions_cancels_every_token() {
        let metatron = Arc::new(Metatron::new());
        let order = test_order(&metatron, OrderKind::Sink);

        let cancel_a = register_session(&metatron, &order, "aaaa", 1.0);
        let cancel_b = register_session(&metatron, &order, "bbbb", 1.0);
        let cancel_c = register_session(&metatron, &order, "cccc", 1.0);

        order.cancel_sessions();

        assert!(cancel_a.is_cancelled());
        assert!(cancel_b.is_cancelled());
        assert!(cancel_c.is_cancelled());
    }
}
