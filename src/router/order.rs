use {super::*, epoch};

pub(crate) const HYSTERESIS_LOW: f64 = 0.95;
pub(crate) const HYSTERESIS_HIGH: f64 = 1.3;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderStatus {
    Pending,
    InMempool,
    Active,
    Fulfilled,
    Cancelled,
    Disconnected,
    Expired,
}

impl OrderStatus {
    pub(crate) fn is_terminal(self) -> bool {
        matches!(
            self,
            OrderStatus::Fulfilled
                | OrderStatus::Cancelled
                | OrderStatus::Disconnected
                | OrderStatus::Expired
        )
    }

    pub(crate) fn awaiting_payment(self) -> bool {
        matches!(self, OrderStatus::Pending | OrderStatus::InMempool)
    }
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Review {
    #[default]
    Clean,
    Flagged,
    Cleared,
}

#[derive(Copy, Clone)]
pub(crate) struct Lifecycle {
    pub(crate) status: OrderStatus,
    pub(crate) review: Review,
    pub(crate) dirty: bool,
}

pub struct Payment {
    pub(crate) address: Address,
    pub(crate) derivation_index: u32,
    pub(crate) amount: Amount,
    pub(crate) created_at_height: u32,
}

impl Payment {
    pub(crate) fn new(
        address: Address,
        derivation_index: u32,
        amount: Amount,
        created_at_height: u32,
    ) -> Self {
        Self {
            address,
            derivation_index,
            amount,
            created_at_height,
        }
    }
}

pub struct Bucket {
    pub(crate) target: HashDays,
    pub(crate) payment: Payment,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Trim {
    pub(crate) sessions: Vec<SessionDetail>,
}

impl Trim {
    pub(crate) fn hashrate(&self) -> HashRate {
        self.sessions
            .iter()
            .fold(HashRate::ZERO, |sum, detail| sum + detail.hashrate)
    }
}

impl AddAssign for Trim {
    fn add_assign(&mut self, rhs: Self) {
        self.sessions.extend(rhs.sessions);
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SessionDetail {
    pub(crate) id: SessionId,
    pub(crate) hashrate: HashRate,
}

pub(crate) struct SessionRegistration {
    pub(crate) session: Arc<Session>,
    pub(crate) cancel: CancellationToken,
}

pub struct Order {
    pub(crate) id: u32,
    pub(crate) upstream_target: UpstreamTarget,
    pub(crate) bucket: Option<Bucket>,
    pub(crate) upstream: Mutex<Option<Arc<Upstream>>>,
    pub(crate) allocator: OnceLock<Arc<EnonceAllocator>>,
    pub(crate) lifecycle: Mutex<Lifecycle>,
    pub(crate) created_at: Instant,
    pub(crate) cancel: CancellationToken,
    pub(crate) metatron: Arc<Metatron>,
    pub(crate) sessions: Mutex<HashMap<SessionId, SessionRegistration>>,
}

impl Order {
    pub(crate) fn new(
        id: u32,
        upstream_target: UpstreamTarget,
        bucket: Option<Bucket>,
        cancel: CancellationToken,
        metatron: Arc<Metatron>,
    ) -> Arc<Self> {
        let now = Instant::now();

        Arc::new(Self {
            id,
            upstream_target,
            bucket,
            upstream: Mutex::new(None),
            allocator: OnceLock::new(),
            lifecycle: Mutex::new(Lifecycle {
                status: OrderStatus::Pending,
                review: Review::Clean,
                dirty: true,
            }),
            created_at: now,
            cancel,
            metatron,
            sessions: Mutex::new(HashMap::new()),
        })
    }

    pub(crate) fn to_entry(&self) -> entry::OrderEntry {
        let now = Instant::now();
        let lifecycle = self.lifecycle();

        entry::OrderEntry {
            status: lifecycle.status,
            review: lifecycle.review,
            upstream_target: self.upstream_target.clone(),
            bucket: self.bucket.as_ref().map(|bucket| entry::BucketEntry {
                target: bucket.target,
                address: bucket.payment.address.as_unchecked().clone(),
                derivation_index: bucket.payment.derivation_index,
                amount_sat: bucket.payment.amount.to_sat(),
                created_at_height: bucket.payment.created_at_height,
            }),
            created_at_secs: epoch::instant_to_epoch_secs(self.created_at, now),
            stats: self.stats().to_entry(now),
        }
    }

    pub(crate) fn restore(
        id: u32,
        order_entry: entry::OrderEntry,
        network: Network,
        cancel: CancellationToken,
        metatron: Arc<Metatron>,
    ) -> Result<Arc<Self>> {
        let stats = Stats::from_entry(order_entry.stats)?;

        let bucket = order_entry
            .bucket
            .map(|bucket| -> Result<Bucket> {
                let address = bucket
                    .address
                    .require_network(network)
                    .with_context(|| format!("restore order {id} payment address"))?;

                Ok(Bucket {
                    target: bucket.target,
                    payment: Payment::new(
                        address,
                        bucket.derivation_index,
                        Amount::from_sat(bucket.amount_sat),
                        bucket.created_at_height,
                    ),
                })
            })
            .transpose()?;

        metatron.restore_order_stats(id, stats);

        Ok(Arc::new(Self {
            id,
            upstream_target: order_entry.upstream_target,
            bucket,
            upstream: Mutex::new(None),
            allocator: OnceLock::new(),
            lifecycle: Mutex::new(Lifecycle {
                status: order_entry.status,
                review: order_entry.review,
                dirty: false,
            }),
            created_at: epoch::epoch_secs_to_instant(order_entry.created_at_secs),
            cancel,
            metatron,
            sessions: Mutex::new(HashMap::new()),
        }))
    }

    pub(crate) fn add_session(&self, session: Arc<Session>, cancel: CancellationToken) {
        self.sessions
            .lock()
            .insert(session.id(), SessionRegistration { session, cancel });
    }

    pub(crate) fn remove_session(&self, id: SessionId) {
        self.sessions.lock().remove(&id);
    }

    pub(crate) fn cancel_all_sessions(&self) {
        self.sessions
            .lock()
            .values()
            .for_each(|registration| registration.cancel.cancel());
    }

    pub(crate) fn lifecycle(&self) -> Lifecycle {
        *self.lifecycle.lock()
    }

    pub(crate) fn clear_dirty(&self) {
        self.lifecycle.lock().dirty = false;
    }

    pub(crate) fn mark_dirty(&self) {
        self.lifecycle.lock().dirty = true;
    }

    pub(crate) fn note_payment_seen(&self, seen: bool) {
        let mut lifecycle = self.lifecycle.lock();

        match (lifecycle.status, seen) {
            (OrderStatus::Pending, true) => lifecycle.status = OrderStatus::InMempool,
            (OrderStatus::InMempool, false) => lifecycle.status = OrderStatus::Pending,
            _ => {}
        }
    }

    pub(crate) fn activate(&self) -> Result {
        let mut lifecycle = self.lifecycle.lock();

        if lifecycle.status.is_terminal() {
            bail!(
                "order in unexpected status {:?} during activation",
                lifecycle.status
            );
        }

        lifecycle.status = OrderStatus::Active;

        info!("Order {} activated", self.id);

        Ok(())
    }

    pub(crate) fn terminate(&self, status: OrderStatus) {
        if !status.is_terminal() {
            return;
        }

        let previous = {
            let mut lifecycle = self.lifecycle.lock();

            if lifecycle.status.is_terminal() {
                return;
            }

            let previous = lifecycle.status;
            lifecycle.status = status;
            lifecycle.dirty = true;
            previous
        };

        info!(
            "Order {} at {} transitioned from {:?} to {:?}",
            self.id, self.upstream_target, previous, status,
        );

        self.cancel.cancel();
    }

    #[cfg(test)]
    pub(crate) fn force_status(&self, status: OrderStatus) {
        self.lifecycle.lock().status = status;
    }

    #[cfg(test)]
    pub(crate) fn is_flagged(&self) -> bool {
        self.lifecycle.lock().review == Review::Flagged
    }

    #[cfg(test)]
    pub(crate) fn is_cleared(&self) -> bool {
        self.lifecycle.lock().review == Review::Cleared
    }

    pub(crate) fn review(&self) -> Review {
        self.lifecycle.lock().review
    }

    pub(crate) fn set_flagged(&self) -> bool {
        {
            let mut lifecycle = self.lifecycle.lock();

            if lifecycle.review != Review::Clean {
                return false;
            }

            lifecycle.review = Review::Flagged;
            lifecycle.dirty = true;
        }

        warn!(
            "Order {} at {} flagged for review",
            self.id, self.upstream_target,
        );

        true
    }

    pub(crate) fn set_cleared(&self) -> bool {
        let cleared = {
            let mut lifecycle = self.lifecycle.lock();

            if lifecycle.review != Review::Flagged {
                return false;
            }

            lifecycle.review = Review::Cleared;
            lifecycle.dirty = true;

            true
        };

        info!("Order {} at {} cleared", self.id, self.upstream_target);

        cleared
    }

    pub(crate) fn is_sink(&self) -> bool {
        self.bucket.is_none()
    }

    pub(crate) fn is_starving(&self, hashrate: HashRate) -> bool {
        self.bucket
            .as_ref()
            .is_some_and(|bucket| hashrate < bucket.target.target_hashrate() * HYSTERESIS_LOW)
    }

    pub(crate) fn upstream(&self) -> Option<Arc<Upstream>> {
        self.upstream.lock().clone()
    }

    pub(crate) fn upstream_route(&self) -> Option<(Arc<Upstream>, Arc<EnonceAllocator>)> {
        let upstream = self.upstream.lock().clone()?;
        let allocator = self.allocator.get()?.clone();

        Some((upstream, allocator))
    }

    pub(crate) fn has_connected_upstream(&self) -> bool {
        self.upstream
            .lock()
            .as_ref()
            .is_some_and(|upstream| upstream.is_connected())
    }

    pub(crate) fn status(&self) -> OrderStatus {
        self.lifecycle.lock().status
    }

    pub(crate) fn hashrate_1m(&self, now: Instant) -> HashRate {
        self.metatron
            .downstream_stats(self.id, now)
            .hashrate_1m(now)
    }

    pub(crate) fn hashrate_shortfall(&self, supplied: HashRate) -> HashRate {
        let Some(bucket) = &self.bucket else {
            return HashRate::ZERO;
        };

        let target = bucket.target.target_hashrate();

        if supplied >= target {
            HashRate::ZERO
        } else {
            target - supplied
        }
    }

    pub(crate) fn hashrate_surplus(&self, supplied: HashRate) -> HashRate {
        let Some(bucket) = &self.bucket else {
            return HashRate::ZERO;
        };

        let target = bucket.target.target_hashrate();

        if supplied > target {
            supplied - target
        } else {
            HashRate::ZERO
        }
    }

    pub(crate) fn relative_load(&self, supplied: HashRate) -> f64 {
        let Some(bucket) = &self.bucket else {
            return f64::INFINITY;
        };

        let target = bucket.target.target_hashrate();

        if target == HashRate::ZERO {
            return f64::INFINITY;
        }

        supplied.as_hps() / target.as_hps()
    }

    pub(crate) fn stats(&self) -> Stats {
        self.metatron.order_stats(self.id)
    }

    pub(crate) fn delivered_work(&self) -> HashWork {
        self.metatron.order_delivered_work(self.id)
    }

    pub(crate) fn is_fulfilled(&self) -> bool {
        let Some(bucket) = &self.bucket else {
            return false;
        };

        self.delivered_work() >= bucket.target.to_hash_work()
    }

    pub(crate) fn hashrate_deficit(&self, now: Instant) -> HashRate {
        if self.bucket.is_none() {
            return HashRate::ZERO;
        }

        if !self.has_connected_upstream() || self.is_fulfilled() {
            return HashRate::ZERO;
        }

        let measured = self.hashrate_1m(now);

        if self.is_starving(measured) {
            self.hashrate_shortfall(measured)
        } else {
            HashRate::ZERO
        }
    }

    pub(crate) fn residual_deficit(&self, now: Instant) -> HashRate {
        if self.bucket.is_none() {
            return HashRate::ZERO;
        }

        if !self.has_connected_upstream() || self.is_fulfilled() {
            return HashRate::ZERO;
        }

        self.hashrate_shortfall(self.hashrate_1m(now))
    }

    pub(crate) fn is_overflowing(&self, now: Instant) -> bool {
        self.bucket.as_ref().is_some_and(|bucket| {
            self.hashrate_1m(now) > bucket.target.target_hashrate() * HYSTERESIS_HIGH
        })
    }

    pub(crate) fn session_details(&self, now: Instant) -> Vec<SessionDetail> {
        self.sessions
            .lock()
            .values()
            .map(|registration| SessionDetail {
                id: registration.session.id(),
                hashrate: registration.session.hashrate_1m(now),
            })
            .collect()
    }

    pub(super) fn trim(&self, max_sessions: Option<usize>, now: Instant) -> Trim {
        let Some(bucket) = &self.bucket else {
            return Trim::default();
        };

        let current = self.hashrate_1m(now);
        let target = bucket.target.target_hashrate();
        let ceiling = target * HYSTERESIS_HIGH;

        if current <= ceiling {
            return Trim::default();
        }

        let mut min_trim = current - ceiling;
        let mut max_trim = current - target;
        let mut trimmed = Trim::default();

        let mut sessions = self.session_details(now);
        sessions.sort_by_key(|detail| Reverse(detail.hashrate));

        for detail in sessions {
            if min_trim <= HashRate::ZERO || Some(trimmed.sessions.len()) == max_sessions {
                break;
            }

            if detail.hashrate == HashRate::ZERO {
                break;
            }

            if detail.hashrate > max_trim {
                continue;
            }

            if !self.trim_session(detail.id, now) {
                continue;
            }

            min_trim -= detail.hashrate;
            max_trim -= detail.hashrate;
            trimmed.sessions.push(detail);
        }

        trimmed
    }

    pub(crate) fn trim_session(&self, id: SessionId, now: Instant) -> bool {
        let sessions = self.sessions.lock();

        let Some(registration) = sessions.get(&id) else {
            return false;
        };

        if registration.cancel.is_cancelled() {
            return false;
        }

        info!(
            "Trimming session {id} ({}) from order {} at {}",
            registration.session.hashrate_1m(now),
            self.id,
            self.upstream_target,
        );

        registration.session.mark_trimmed();
        registration.cancel.cancel();
        true
    }

    pub(crate) async fn connect(
        &self,
        timeout: Duration,
        enonce1_extension_size: usize,
        tasks: &TaskTracker,
    ) -> Result {
        let upstream = Upstream::connect(
            self.id,
            &self.upstream_target,
            timeout,
            NOTIFY_TIMEOUT,
            self.cancel.clone(),
            tasks,
            self.metatron.clone(),
        )
        .await?;

        let extranonces = Extranonces::Proxy(ProxyExtranonces::new(
            upstream.enonce1().clone(),
            upstream.enonce2_size(),
            enonce1_extension_size,
        )?);

        if let Some(allocator) = self.allocator.get() {
            allocator.update_extranonces(extranonces);
        } else {
            let allocator = Arc::new(EnonceAllocator::new(extranonces, self.id));

            self.allocator
                .set(allocator)
                .map_err(|_| anyhow!("allocator already initialized"))?;

            self.activate()?;
        }

        *self.upstream.lock() = Some(upstream);

        info!("Upstream {} connected", self.upstream_target);

        Ok(())
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

    fn test_order(metatron: &Arc<Metatron>, target: Option<HashDays>) -> Arc<Order> {
        let bucket = target.map(|target| Bucket {
            target,
            payment: Payment::new(test_address(), 0, Amount::from_sat(1000), 0),
        });
        Order::new(
            0,
            "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.worker@bar:3333"
                .parse()
                .unwrap(),
            bucket,
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
        let session = metatron.new_session(
            test_authorization(enonce1),
            0,
            SocketAddr::from(([127, 0, 0, 1], 4444)),
        );
        session.record_accepted(Difficulty::from(difficulty), Difficulty::from(difficulty));
        let cancel = CancellationToken::new();
        bucket.add_session(session, cancel.clone());
        cancel
    }

    #[test]
    fn trim_session_marks_the_session_trimmed() {
        let (metatron, _dir) = Metatron::test();
        let metatron = Arc::new(metatron);
        let bucket = test_order(&metatron, Some(HashDays::new(1e9).unwrap()));

        let session = metatron.new_session(
            test_authorization("deadbeef"),
            0,
            SocketAddr::from(([127, 0, 0, 1], 4444)),
        );
        session.record_accepted(Difficulty::from(1000.0), Difficulty::from(1000.0));

        let cancel = CancellationToken::new();
        bucket.add_session(session.clone(), cancel.clone());

        assert!(!session.is_trimmed());
        assert!(bucket.trim_session(session.id(), Instant::now()));
        assert!(session.is_trimmed());
        assert!(cancel.is_cancelled());
    }

    #[test]
    fn trim_sink_is_noop() {
        let (metatron, _dir) = Metatron::test();
        let metatron = Arc::new(metatron);
        let sink = test_order(&metatron, None);
        sink.trim(None, Instant::now());
    }

    #[test]
    fn trim_noop_when_not_overshooting() {
        let (metatron, _dir) = Metatron::test();
        let metatron = Arc::new(metatron);
        let bucket = test_order(&metatron, Some(HashDays::new(1e9).unwrap()));

        let cancel = register_session(&metatron, &bucket, "deadbeef", 1.0);

        bucket.trim(None, Instant::now());
        assert!(!cancel.is_cancelled());
    }

    #[test]
    fn trim_noop_when_single_session_overshoots() {
        let (metatron, _dir) = Metatron::test();
        let metatron = Arc::new(metatron);
        let bucket = test_order(&metatron, Some(HashDays::new(1.0).unwrap()));

        let cancel = register_session(&metatron, &bucket, "deadbeef", 10_000.0);

        bucket.trim(None, Instant::now());
        assert!(!cancel.is_cancelled());
    }

    #[test]
    fn trim_noop_when_all_sessions_too_fat() {
        let (metatron, _dir) = Metatron::test();
        let metatron = Arc::new(metatron);
        let bucket = test_order(&metatron, Some(HashDays::new(1e9).unwrap()));

        let cancel_a = register_session(&metatron, &bucket, "aaaa", 12.0);
        let cancel_b = register_session(&metatron, &bucket, "bbbb", 12.0);

        bucket.trim(None, Instant::now());
        assert!(!cancel_a.is_cancelled());
        assert!(!cancel_b.is_cancelled());
    }

    #[test]
    fn trim_picks_fattest_in_band() {
        let (metatron, _dir) = Metatron::test();
        let metatron = Arc::new(metatron);
        let bucket = test_order(&metatron, Some(HashDays::new(1e9).unwrap()));

        let cancel_fat = register_session(&metatron, &bucket, "aaaa", 13.0);
        let cancel_mid = register_session(&metatron, &bucket, "bbbb", 7.0);
        let cancel_small = register_session(&metatron, &bucket, "cccc", 4.0);

        bucket.trim(None, Instant::now());
        assert!(!cancel_fat.is_cancelled());
        assert!(cancel_mid.is_cancelled());
        assert!(!cancel_small.is_cancelled());
    }

    #[test]
    fn trim_sheds_many_small_sessions() {
        let (metatron, _dir) = Metatron::test();
        let metatron = Arc::new(metatron);
        let bucket = test_order(&metatron, Some(HashDays::new(1e9).unwrap()));

        let cancels: Vec<CancellationToken> = (0..10)
            .map(|i| {
                let enonce = format!("{i:08x}");
                register_session(&metatron, &bucket, &enonce, 3.0)
            })
            .collect();

        bucket.trim(None, Instant::now());

        let cancelled = cancels.iter().filter(|c| c.is_cancelled()).count();
        assert!(
            cancelled >= 2,
            "expected multiple small sessions trimmed, got {cancelled}",
        );
    }

    #[test]
    fn trim_respects_session_limit() {
        let (metatron, _dir) = Metatron::test();
        let metatron = Arc::new(metatron);
        let bucket = test_order(&metatron, Some(HashDays::new(1e9).unwrap()));

        let cancels: Vec<CancellationToken> = (0..10)
            .map(|i| {
                let enonce = format!("{i:08x}");
                register_session(&metatron, &bucket, &enonce, 3.0)
            })
            .collect();

        let trimmed = bucket.trim(Some(1), Instant::now());

        assert_eq!(trimmed.sessions.len(), 1);
        assert_eq!(cancels.iter().filter(|c| c.is_cancelled()).count(), 1);
    }

    #[test]
    fn trim_sheds_overflow_above_lowered_ceiling() {
        let (metatron, _dir) = Metatron::test();
        let metatron = Arc::new(metatron);
        let bucket = test_order(&metatron, Some(HashDays::new(1e9).unwrap()));

        let cancels = [
            register_session(&metatron, &bucket, "aaaa", 5.0),
            register_session(&metatron, &bucket, "bbbb", 5.0),
            register_session(&metatron, &bucket, "cccc", 5.0),
            register_session(&metatron, &bucket, "dddd", 4.5),
        ];

        bucket.trim(None, Instant::now());

        let cancelled = cancels.iter().filter(|c| c.is_cancelled()).count();
        assert_eq!(cancelled, 1);
    }

    #[test]
    fn trim_stops_at_hysteresis_ceiling() {
        let (metatron, _dir) = Metatron::test();
        let metatron = Arc::new(metatron);
        let bucket = test_order(&metatron, Some(HashDays::new(1e9).unwrap()));

        let cancels: Vec<CancellationToken> = (0..8)
            .map(|i| {
                let enonce = format!("{i:08x}");
                register_session(&metatron, &bucket, &enonce, 3.0)
            })
            .collect();

        bucket.trim(None, Instant::now());

        let surviving = cancels.iter().filter(|c| !c.is_cancelled()).count();
        assert!(
            surviving >= 1,
            "trim drained below ceiling instead of stopping at it",
        );
    }

    #[test]
    fn trim_skips_sessions_larger_than_headroom() {
        let (metatron, _dir) = Metatron::test();
        let metatron = Arc::new(metatron);
        let bucket = test_order(&metatron, Some(HashDays::new(1e9).unwrap()));

        let cancel_huge = register_session(&metatron, &bucket, "aaaa", 20.0);
        let cancel_a = register_session(&metatron, &bucket, "bbbb", 3.0);
        let cancel_b = register_session(&metatron, &bucket, "cccc", 3.0);
        let cancel_c = register_session(&metatron, &bucket, "dddd", 3.0);
        let cancel_d = register_session(&metatron, &bucket, "eeee", 3.0);

        bucket.trim(None, Instant::now());

        assert!(!cancel_huge.is_cancelled());
        let trimmed = [&cancel_a, &cancel_b, &cancel_c, &cancel_d]
            .iter()
            .filter(|c| c.is_cancelled())
            .count();
        assert!(trimmed >= 1);
    }

    #[test]
    fn trim_skips_session_after_max_trim_shrinks() {
        let (metatron, _dir) = Metatron::test();
        let metatron = Arc::new(metatron);
        let bucket = test_order(&metatron, Some(HashDays::new(1e9).unwrap()));

        let cancel_a = register_session(&metatron, &bucket, "aaaa", 18.0);
        let cancel_b = register_session(&metatron, &bucket, "bbbb", 17.0);
        let cancel_c = register_session(&metatron, &bucket, "cccc", 7.0);

        bucket.trim(None, Instant::now());

        assert!(cancel_a.is_cancelled());
        assert!(!cancel_b.is_cancelled());
        assert!(cancel_c.is_cancelled());
    }

    #[test]
    fn is_starving() {
        fn starving(order: &Order) -> bool {
            order.is_starving(order.hashrate_1m(Instant::now()))
        }

        let (metatron, _dir) = Metatron::test();
        let metatron = Arc::new(metatron);
        assert!(!starving(&test_order(&metatron, None)));
        assert!(starving(&test_order(
            &metatron,
            Some(HashDays::new(1e20).unwrap())
        )));

        let (metatron, _dir) = Metatron::test();
        let metatron = Arc::new(metatron);
        let fed = test_order(&metatron, Some(HashDays::new(1.0).unwrap()));
        register_session(&metatron, &fed, "deadbeef", 10_000.0);
        assert!(!starving(&fed));
    }

    #[test]
    fn is_fulfilled_compares_hash_work_to_hash_days_target() {
        #[track_caller]
        fn case(target: Option<f64>, delivered: Option<f64>, expected: bool) {
            let (metatron, _dir) = Metatron::test();
            let metatron = Arc::new(metatron);
            let order = test_order(
                &metatron,
                target.map(|target| HashDays::new(target).unwrap()),
            );

            if let Some(delivered) = delivered {
                metatron.set_order_delivered_work(
                    order.id,
                    HashDays::new(delivered).unwrap().to_hash_work(),
                );
            }

            assert_eq!(order.is_fulfilled(), expected);
        }

        case(None, None, false);
        case(Some(1e15), None, false);
        case(Some(1e12), Some(1e12), true);
        case(Some(1e12), Some(2e12), true);
    }

    #[test]
    fn hashrate_shortfall_sums_without_netting_surplus() {
        let (metatron, _dir) = Metatron::test();
        let metatron = Arc::new(metatron);
        let order_a = test_order(&metatron, Some(HashDays::new(1e15).unwrap()));
        let order_b = test_order(&metatron, Some(HashDays::new(3e15).unwrap()));

        let deficit = order_a.hashrate_shortfall(HashRate::from_hps(1.5e15))
            + order_b.hashrate_shortfall(HashRate::from_hps(2e15));

        assert_eq!(deficit, HashRate::from_hps(1e15));
    }

    #[test]
    fn hashrate_shortfall_does_not_apply_starvation_hysteresis() {
        let (metatron, _dir) = Metatron::test();
        let metatron = Arc::new(metatron);
        let order = test_order(&metatron, Some(HashDays::new(100.0).unwrap()));
        let measured = HashRate::from_hps(97.0);

        assert!(!order.is_starving(measured));
        assert_eq!(order.hashrate_shortfall(measured), HashRate::from_hps(3.0));
    }

    #[test]
    fn hashrate_surplus() {
        #[track_caller]
        fn case(target: Option<f64>, supplied: f64, expected: f64) {
            let (metatron, _dir) = Metatron::test();
            let metatron = Arc::new(metatron);
            let order = test_order(
                &metatron,
                target.map(|target| HashDays::new(target).unwrap()),
            );

            assert_eq!(
                order.hashrate_surplus(HashRate::from_hps(supplied)),
                HashRate::from_hps(expected),
            );
        }

        case(None, 200.0, 0.0);
        case(Some(100.0), 50.0, 0.0);
        case(Some(100.0), 100.0, 0.0);
        case(Some(100.0), 150.0, 50.0);
    }

    #[test]
    fn relative_load() {
        #[track_caller]
        fn case(target: Option<f64>, supplied: f64, expected: f64) {
            let (metatron, _dir) = Metatron::test();
            let metatron = Arc::new(metatron);
            let order = test_order(
                &metatron,
                target.map(|target| HashDays::new(target).unwrap()),
            );

            assert_eq!(order.relative_load(HashRate::from_hps(supplied)), expected,);
        }

        case(None, 200.0, f64::INFINITY);
        case(Some(0.0), 0.0, f64::INFINITY);
        case(Some(0.0), 200.0, f64::INFINITY);
        case(Some(100.0), 50.0, 0.5);
        case(Some(100.0), 100.0, 1.0);
        case(Some(100.0), 200.0, 2.0);
    }

    #[test]
    fn hashrate_deficit() {
        #[track_caller]
        fn case(
            target: Option<f64>,
            session_diff: Option<f64>,
            delivered: Option<f64>,
            expected: f64,
        ) {
            let (metatron, _dir) = Metatron::test();
            let metatron = Arc::new(metatron);
            let order = test_order(
                &metatron,
                target.map(|target| HashDays::new(target).unwrap()),
            );
            connect_upstream(&order, &metatron);

            if let Some(diff) = session_diff {
                register_session(&metatron, &order, "deadbeef", diff);
            }

            if let Some(delivered) = delivered {
                metatron.set_order_delivered_work(
                    order.id,
                    HashDays::new(delivered).unwrap().to_hash_work(),
                );
            }

            assert_eq!(order.hashrate_deficit(Instant::now()).as_hps(), expected);
        }

        case(None, None, None, 0.0);
        case(Some(100.0), None, None, 100.0);
        case(Some(1.0), Some(10_000.0), None, 0.0);
        case(Some(100.0), None, Some(100.0), 0.0);

        let (metatron, _dir) = Metatron::test();
        let metatron = Arc::new(metatron);
        let starving = test_order(&metatron, Some(HashDays::new(100.0).unwrap()));
        assert_eq!(starving.hashrate_deficit(Instant::now()), HashRate::ZERO);
        connect_upstream(&starving, &metatron);
        assert!(starving.hashrate_deficit(Instant::now()) > HashRate::ZERO);
        starving.upstream().unwrap().set_connected(false);
        assert_eq!(starving.hashrate_deficit(Instant::now()), HashRate::ZERO);
    }

    #[test]
    fn residual_deficit() {
        let (metatron, _dir) = Metatron::test();
        let metatron = Arc::new(metatron);
        let sink = test_order(&metatron, None);
        connect_upstream(&sink, &metatron);
        assert_eq!(sink.residual_deficit(Instant::now()), HashRate::ZERO);

        let (metatron, _dir) = Metatron::test();
        let metatron = Arc::new(metatron);
        let order = test_order(&metatron, Some(HashDays::new(100.0).unwrap()));
        connect_upstream(&order, &metatron);

        assert_eq!(
            order.residual_deficit(Instant::now()),
            HashRate::from_hps(100.0)
        );

        order.upstream().unwrap().set_connected(false);
        assert_eq!(order.residual_deficit(Instant::now()), HashRate::ZERO);

        let (metatron, _dir) = Metatron::test();
        let metatron = Arc::new(metatron);
        let fulfilled = test_order(&metatron, Some(HashDays::new(100.0).unwrap()));
        connect_upstream(&fulfilled, &metatron);
        metatron
            .set_order_delivered_work(fulfilled.id, HashDays::new(100.0).unwrap().to_hash_work());
        assert_eq!(fulfilled.residual_deficit(Instant::now()), HashRate::ZERO);
    }

    fn connect_upstream(order: &Order, metatron: &Arc<Metatron>) {
        *order.upstream.lock() = Some(Upstream::test(order.id, metatron.clone()));
    }

    #[test]
    fn cancel_all_sessions() {
        let (metatron, _dir) = Metatron::test();
        let metatron = Arc::new(metatron);
        let order = test_order(&metatron, None);

        let cancel_a = register_session(&metatron, &order, "aaaa", 1.0);
        let cancel_b = register_session(&metatron, &order, "bbbb", 1.0);
        let cancel_c = register_session(&metatron, &order, "cccc", 1.0);

        order.cancel_all_sessions();

        assert!(cancel_a.is_cancelled());
        assert!(cancel_b.is_cancelled());
        assert!(cancel_c.is_cancelled());
    }
}
