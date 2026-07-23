use {super::*, control::TRIM_COOLDOWN, epoch};

pub(crate) const HYSTERESIS_LOW: f64 = 0.95;
pub(crate) const HYSTERESIS_HIGH: f64 = 1.3;
pub(crate) const SEVERE_STARVATION: f64 = 0.5;

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
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Review {
    #[default]
    Clean,
    Flagged,
    Cleared,
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

pub(crate) const PLACEMENT_TTL: Duration = Duration::from_secs(90);

#[derive(Clone, Debug, Default)]
pub(crate) struct Trim {
    pub(crate) hashrate: HashRate,
    pub(crate) sessions: Vec<SessionDetail>,
}

impl AddAssign for Trim {
    fn add_assign(&mut self, rhs: Self) {
        self.hashrate += rhs.hashrate;
        self.sessions.extend(rhs.sessions);
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SessionDetail {
    pub(crate) id: SessionId,
    pub(crate) enonce1: Extranonce,
    pub(crate) ip: IpAddr,
    pub(crate) hashrate: HashRate,
}

type SessionRegistration = (Arc<Session>, CancellationToken, SocketAddr);

pub struct Order {
    pub(crate) id: u32,
    pub(crate) upstream_target: UpstreamTarget,
    pub(crate) bucket: Option<Bucket>,
    pub(crate) upstream: Mutex<Option<Arc<Upstream>>>,
    pub(crate) allocator: OnceLock<Arc<EnonceAllocator>>,
    pub(crate) status: Mutex<OrderStatus>,
    pub(crate) review: Mutex<Review>,
    pub(crate) created_at: Instant,
    pub(crate) cancel: CancellationToken,
    pub(crate) metatron: Arc<Metatron>,
    pub(crate) sessions: Mutex<HashMap<SessionId, SessionRegistration>>,
    pub(crate) placements: Mutex<HashMap<SocketAddr, (HashRate, Instant)>>,
    pub(crate) expected_placements: AtomicU64,
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
            status: Mutex::new(OrderStatus::Pending),
            review: Mutex::new(Review::Clean),
            created_at: now,
            cancel,
            metatron,
            sessions: Mutex::new(HashMap::new()),
            placements: Mutex::new(HashMap::new()),
            expected_placements: AtomicU64::new(0),
        })
    }

    pub(crate) fn to_entry(&self) -> entry::OrderEntry {
        let now = Instant::now();

        entry::OrderEntry {
            status: self.status(),
            review: self.review(),
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
        metatron.restore_order_stats(id, stats);

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

        Ok(Arc::new(Self {
            id,
            upstream_target: order_entry.upstream_target,
            bucket,
            upstream: Mutex::new(None),
            allocator: OnceLock::new(),
            status: Mutex::new(order_entry.status),
            review: Mutex::new(order_entry.review),
            created_at: epoch::epoch_secs_to_instant(order_entry.created_at_secs),
            cancel,
            metatron,
            sessions: Mutex::new(HashMap::new()),
            placements: Mutex::new(HashMap::new()),
            expected_placements: AtomicU64::new(0),
        }))
    }

    pub(crate) fn place(&self, addr: SocketAddr, expected: HashRate) {
        let mut placements = self.placements.lock();
        placements.insert(addr, (expected, Instant::now()));
        self.store_expected(&placements);
    }

    pub(crate) fn release_placement(&self, addr: &SocketAddr) {
        let mut placements = self.placements.lock();
        if placements.remove(addr).is_some() {
            self.store_expected(&placements);
        }
    }

    pub(crate) fn sweep_placements(&self, now: Instant) {
        let mut placements = self.placements.lock();
        placements.retain(|_, (_, created)| now.duration_since(*created) < PLACEMENT_TTL);
        self.store_expected(&placements);
    }

    fn store_expected(&self, placements: &HashMap<SocketAddr, (HashRate, Instant)>) {
        let total = placements
            .values()
            .map(|(rate, _)| rate.as_hps())
            .sum::<f64>();

        self.expected_placements
            .store(total.to_bits(), Ordering::Relaxed);
    }

    pub(crate) fn expected_incoming(&self) -> HashRate {
        HashRate::from_hps(f64::from_bits(
            self.expected_placements.load(Ordering::Relaxed),
        ))
    }

    pub(crate) fn add_session(
        &self,
        session: Arc<Session>,
        cancel: CancellationToken,
        addr: SocketAddr,
    ) {
        self.release_placement(&addr);

        self.sessions
            .lock()
            .insert(session.id(), (session, cancel, addr));
    }

    pub(crate) fn remove_session(&self, id: SessionId) {
        self.sessions.lock().remove(&id);
    }

    pub(crate) fn cancel_all_sessions(&self) {
        self.sessions
            .lock()
            .values()
            .for_each(|(_, cancel, _)| cancel.cancel());
    }

    pub(crate) fn terminate(&self, status: OrderStatus) {
        if !status.is_terminal() {
            return;
        }

        let previous = {
            let mut current = self.status.lock();

            if current.is_terminal() {
                return;
            }

            let previous = *current;
            *current = status;
            previous
        };

        info!(
            "Order {} at {} transitioned from {:?} to {:?}",
            self.id, self.upstream_target, previous, status,
        );

        self.cancel.cancel();
    }

    #[cfg(test)]
    pub(crate) fn is_flagged(&self) -> bool {
        *self.review.lock() == Review::Flagged
    }

    #[cfg(test)]
    pub(crate) fn is_cleared(&self) -> bool {
        *self.review.lock() == Review::Cleared
    }

    pub(crate) fn review(&self) -> Review {
        *self.review.lock()
    }

    pub(crate) fn set_flagged(&self) {
        let mut review = self.review.lock();

        if *review != Review::Clean {
            return;
        }

        *review = Review::Flagged;
        warn!(
            "Order {} at {} flagged for review",
            self.id, self.upstream_target,
        );
    }

    pub(crate) fn set_cleared(&self) -> bool {
        let mut review = self.review.lock();

        if *review != Review::Flagged {
            return false;
        }

        *review = Review::Cleared;

        info!("Order {} at {} cleared", self.id, self.upstream_target);

        true
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

    pub(crate) fn has_connected_upstream(&self) -> bool {
        self.upstream
            .lock()
            .as_ref()
            .is_some_and(|upstream| upstream.is_connected())
    }

    pub(crate) fn allocator(&self) -> Option<&Arc<EnonceAllocator>> {
        self.allocator.get()
    }

    pub(crate) fn status(&self) -> OrderStatus {
        *self.status.lock()
    }

    pub(crate) fn hashrate_1m(&self, now: Instant) -> HashRate {
        self.metatron
            .downstream_stats(self.id, now)
            .hashrate_1m(now)
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

    pub(crate) fn supplied(&self, now: Instant, intents_expected: HashRate) -> HashRate {
        self.hashrate_1m(now) + self.expected_incoming() + intents_expected
    }

    pub(crate) fn hashrate_deficit(&self, now: Instant, intents_expected: HashRate) -> HashRate {
        let Some(bucket) = &self.bucket else {
            return HashRate::ZERO;
        };

        if !self.has_connected_upstream() || self.is_fulfilled() {
            return HashRate::ZERO;
        }

        let supplied = self.supplied(now, intents_expected);

        if self.is_starving(supplied) {
            bucket.target.target_hashrate() - supplied
        } else {
            HashRate::ZERO
        }
    }

    pub(crate) fn residual_deficit(&self, now: Instant, intents_expected: HashRate) -> HashRate {
        let Some(bucket) = &self.bucket else {
            return HashRate::ZERO;
        };

        if !self.has_connected_upstream() || self.is_fulfilled() {
            return HashRate::ZERO;
        }

        let supplied = self.supplied(now, intents_expected);
        let target = bucket.target.target_hashrate();

        if supplied >= target {
            HashRate::ZERO
        } else {
            target - supplied
        }
    }

    pub(crate) fn is_severely_starving(&self, now: Instant, intents_expected: HashRate) -> bool {
        let Some(bucket) = &self.bucket else {
            return false;
        };

        self.has_connected_upstream()
            && !self.is_fulfilled()
            && self.supplied(now, intents_expected)
                < bucket.target.target_hashrate() * SEVERE_STARVATION
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
            .map(|(session, _, addr)| SessionDetail {
                id: session.id(),
                enonce1: session.enonce1().clone(),
                ip: addr.ip(),
                hashrate: session.hashrate_1m(now),
            })
            .collect()
    }

    pub(super) fn trim(
        &self,
        max_sessions: Option<usize>,
        now: Instant,
        cooldowns: &HashMap<Extranonce, Instant>,
    ) -> Trim {
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

            if cooldowns
                .get(&detail.enonce1)
                .is_some_and(|since| now.duration_since(*since) < TRIM_COOLDOWN)
            {
                continue;
            }

            self.trim_session(detail.id, now);
            min_trim -= detail.hashrate;
            max_trim -= detail.hashrate;
            trimmed.hashrate += detail.hashrate;
            trimmed.sessions.push(detail);
        }

        trimmed
    }

    pub(crate) fn trim_session(&self, id: SessionId, now: Instant) {
        let sessions = self.sessions.lock();

        let Some((session, cancel, _)) = sessions.get(&id) else {
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

            let mut status = self.status.lock();

            if !matches!(
                *status,
                OrderStatus::Pending | OrderStatus::InMempool | OrderStatus::Active
            ) {
                bail!("order in unexpected status {:?} during activation", *status);
            }

            *status = OrderStatus::Active;

            info!("Order {} activated", self.id);
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
        let session = metatron.new_session(test_authorization(enonce1), 0);
        session.record_accepted(Difficulty::from(difficulty), Difficulty::from(difficulty));
        let cancel = CancellationToken::new();
        bucket.add_session(
            session,
            cancel.clone(),
            SocketAddr::from(([127, 0, 0, 1], 4444)),
        );
        cancel
    }

    fn no_cooldowns() -> HashMap<Extranonce, Instant> {
        HashMap::new()
    }

    #[test]
    fn trim_sink_is_noop() {
        let (metatron, _dir) = Metatron::test();
        let metatron = Arc::new(metatron);
        let sink = test_order(&metatron, None);
        sink.trim(None, Instant::now(), &no_cooldowns());
    }

    #[test]
    fn trim_noop_when_not_overshooting() {
        let (metatron, _dir) = Metatron::test();
        let metatron = Arc::new(metatron);
        let bucket = test_order(&metatron, Some(HashDays::new(1e9).unwrap()));

        let cancel = register_session(&metatron, &bucket, "deadbeef", 1.0);

        bucket.trim(None, Instant::now(), &no_cooldowns());
        assert!(!cancel.is_cancelled());
    }

    #[test]
    fn trim_noop_when_single_session_overshoots() {
        let (metatron, _dir) = Metatron::test();
        let metatron = Arc::new(metatron);
        let bucket = test_order(&metatron, Some(HashDays::new(1.0).unwrap()));

        let cancel = register_session(&metatron, &bucket, "deadbeef", 10_000.0);

        bucket.trim(None, Instant::now(), &no_cooldowns());
        assert!(!cancel.is_cancelled());
    }

    #[test]
    fn trim_noop_when_all_sessions_too_fat() {
        let (metatron, _dir) = Metatron::test();
        let metatron = Arc::new(metatron);
        let bucket = test_order(&metatron, Some(HashDays::new(1e9).unwrap()));

        let cancel_a = register_session(&metatron, &bucket, "aaaa", 12.0);
        let cancel_b = register_session(&metatron, &bucket, "bbbb", 12.0);

        bucket.trim(None, Instant::now(), &no_cooldowns());
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

        bucket.trim(None, Instant::now(), &no_cooldowns());
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

        bucket.trim(None, Instant::now(), &no_cooldowns());

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

        let trimmed = bucket.trim(Some(1), Instant::now(), &no_cooldowns());

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

        bucket.trim(None, Instant::now(), &no_cooldowns());

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

        bucket.trim(None, Instant::now(), &no_cooldowns());

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

        bucket.trim(None, Instant::now(), &no_cooldowns());

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

        bucket.trim(None, Instant::now(), &no_cooldowns());

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

            assert_eq!(
                order
                    .hashrate_deficit(Instant::now(), HashRate::ZERO)
                    .as_hps(),
                expected
            );
        }

        case(None, None, None, 0.0);
        case(Some(100.0), None, None, 100.0);
        case(Some(1.0), Some(10_000.0), None, 0.0);
        case(Some(100.0), None, Some(100.0), 0.0);

        let (metatron, _dir) = Metatron::test();
        let metatron = Arc::new(metatron);
        let starving = test_order(&metatron, Some(HashDays::new(100.0).unwrap()));
        assert_eq!(
            starving.hashrate_deficit(Instant::now(), HashRate::ZERO),
            HashRate::ZERO
        );
        connect_upstream(&starving, &metatron);
        assert!(starving.hashrate_deficit(Instant::now(), HashRate::ZERO) > HashRate::ZERO);
        starving.upstream().unwrap().set_connected(false);
        assert_eq!(
            starving.hashrate_deficit(Instant::now(), HashRate::ZERO),
            HashRate::ZERO
        );

        let (metatron, _dir) = Metatron::test();
        let metatron = Arc::new(metatron);
        let order = test_order(&metatron, Some(HashDays::new(100.0).unwrap()));
        connect_upstream(&order, &metatron);
        order.place(
            SocketAddr::from(([127, 0, 0, 1], 4444)),
            HashRate::from_hps(30.0),
        );
        assert_eq!(
            order
                .hashrate_deficit(Instant::now(), HashRate::ZERO)
                .as_hps(),
            70.0
        );
        assert_eq!(
            order
                .hashrate_deficit(Instant::now(), HashRate::from_hps(70.0))
                .as_hps(),
            0.0
        );
    }

    #[test]
    fn residual_deficit() {
        let (metatron, _dir) = Metatron::test();
        let metatron = Arc::new(metatron);
        let order = test_order(&metatron, Some(HashDays::new(100.0).unwrap()));
        connect_upstream(&order, &metatron);

        assert_eq!(
            order.residual_deficit(Instant::now(), HashRate::ZERO),
            HashRate::from_hps(100.0)
        );

        order.place(
            SocketAddr::from(([127, 0, 0, 1], 4444)),
            HashRate::from_hps(30.0),
        );

        assert_eq!(
            order.residual_deficit(Instant::now(), HashRate::ZERO),
            HashRate::from_hps(70.0)
        );

        assert_eq!(
            order.residual_deficit(Instant::now(), HashRate::from_hps(70.0)),
            HashRate::ZERO
        );

        order.upstream().unwrap().set_connected(false);
        assert_eq!(
            order.residual_deficit(Instant::now(), HashRate::ZERO),
            HashRate::ZERO
        );
    }

    #[test]
    fn is_severely_starving() {
        let (metatron, _dir) = Metatron::test();
        let metatron = Arc::new(metatron);
        let order = test_order(&metatron, Some(HashDays::new(100.0).unwrap()));
        connect_upstream(&order, &metatron);

        assert!(order.is_severely_starving(Instant::now(), HashRate::ZERO));

        order.place(
            SocketAddr::from(([127, 0, 0, 1], 4444)),
            HashRate::from_hps(60.0),
        );

        assert!(!order.is_severely_starving(Instant::now(), HashRate::ZERO));
        assert!(order.is_starving(order.supplied(Instant::now(), HashRate::ZERO)));

        order.upstream().unwrap().set_connected(false);
        assert!(!order.is_severely_starving(Instant::now(), HashRate::ZERO));

        let sink = test_order(&metatron, None);
        assert!(!sink.is_severely_starving(Instant::now(), HashRate::ZERO));
    }

    #[test]
    fn placements_debit_expected_incoming() {
        let (metatron, _dir) = Metatron::test();
        let metatron = Arc::new(metatron);
        let order = test_order(&metatron, Some(HashDays::new(100.0).unwrap()));
        let addr = SocketAddr::from(([127, 0, 0, 1], 4444));

        assert_eq!(order.expected_incoming(), HashRate::ZERO);

        order.place(addr, HashRate::from_hps(10.0));
        order.place(
            SocketAddr::from(([127, 0, 0, 2], 4444)),
            HashRate::from_hps(20.0),
        );
        assert_eq!(order.expected_incoming(), HashRate::from_hps(30.0));

        order.release_placement(&addr);
        assert_eq!(order.expected_incoming(), HashRate::from_hps(20.0));

        order.sweep_placements(Instant::now() + PLACEMENT_TTL + Duration::from_secs(1));
        assert_eq!(order.expected_incoming(), HashRate::ZERO);
    }

    #[test]
    fn add_session_releases_placement() {
        let (metatron, _dir) = Metatron::test();
        let metatron = Arc::new(metatron);
        let order = test_order(&metatron, Some(HashDays::new(100.0).unwrap()));
        let addr = SocketAddr::from(([127, 0, 0, 1], 4444));

        order.place(addr, HashRate::from_hps(10.0));
        assert_eq!(order.expected_incoming(), HashRate::from_hps(10.0));

        register_session(&metatron, &order, "deadbeef", 1.0);

        let now = Instant::now();
        assert_eq!(order.expected_incoming(), HashRate::ZERO);
        assert_eq!(order.supplied(now, HashRate::ZERO), order.hashrate_1m(now));
    }

    #[test]
    fn trim_skips_cooled_sessions() {
        let (metatron, _dir) = Metatron::test();
        let metatron = Arc::new(metatron);
        let bucket = test_order(&metatron, Some(HashDays::new(1e9).unwrap()));

        let cancel_fat = register_session(&metatron, &bucket, "aaaa", 13.0);
        let cancel_mid = register_session(&metatron, &bucket, "bbbb", 7.0);
        let cancel_small = register_session(&metatron, &bucket, "cccc", 4.0);

        let cooldowns = HashMap::from([("bbbb".parse().unwrap(), Instant::now())]);

        bucket.trim(None, Instant::now(), &cooldowns);

        assert!(!cancel_fat.is_cancelled());
        assert!(!cancel_mid.is_cancelled());
        assert!(cancel_small.is_cancelled());
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
