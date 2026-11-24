use {
    super::*,
    order::{HYSTERESIS_HIGH, Order, SessionDetail, Trim},
    rand::{Rng, SeedableRng, rngs::StdRng},
};

const MAX_OVERFLOW_TRIMS_PER_TICK: usize = 1;
const DEFICIT_PERSIST_TICKS: usize = 2;
const OVERFLOW_PERSIST_TICKS: usize = 3;

#[derive(Default)]
struct Ticks {
    deficit: HashMap<u32, usize>,
    overflow: HashMap<u32, usize>,
}

#[derive(Clone, Copy, Debug)]
struct Demand {
    deficit: HashRate,
    surplus: HashRate,
}

impl Demand {
    fn snapshot(
        orders: &[Arc<Order>],
        now: Instant,
        deficit_ticks: &mut HashMap<u32, usize>,
    ) -> Self {
        deficit_ticks.retain(|id, _| orders.iter().any(|order| order.id == *id));

        let mut deficit = HashRate::ZERO;
        let mut surplus = HashRate::ZERO;

        for order in orders.iter().filter(|order| !order.is_sink()) {
            if order.has_connected_upstream() && !order.is_fulfilled() {
                surplus += order.hashrate_surplus(order.hashrate_1m(now));
            }

            let raw_deficit = order.hashrate_deficit(now);

            if raw_deficit == HashRate::ZERO {
                deficit_ticks.remove(&order.id);

                continue;
            }

            if !order.has_sessions() {
                deficit_ticks.remove(&order.id);
                deficit += raw_deficit;

                continue;
            }

            let ticks = deficit_ticks.entry(order.id).or_insert(0);
            *ticks += 1;

            if *ticks < DEFICIT_PERSIST_TICKS {
                continue;
            }

            deficit += raw_deficit;
        }

        Self { deficit, surplus }
    }

    fn exhausted(self) -> bool {
        self.deficit == HashRate::ZERO
    }

    fn consume(&mut self, hashrate: HashRate) {
        self.deficit -= hashrate;
    }
}

pub(crate) struct Control {
    ticks: Mutex<Ticks>,
    rng: Mutex<StdRng>,
}

impl Default for Control {
    fn default() -> Self {
        Self {
            ticks: Mutex::new(Ticks::default()),
            rng: Mutex::new(StdRng::from_rng(&mut rand::rng())),
        }
    }
}

impl Control {
    #[cfg(test)]
    pub(crate) fn seed_rng(&self, seed: u64) {
        *self.rng.lock() = StdRng::seed_from_u64(seed);
    }

    pub(crate) fn next_order(&self, candidates: &[Arc<Order>], pace: bool) -> Option<Arc<Order>> {
        let now = Instant::now();

        let buckets = candidates
            .iter()
            .filter(|order| !order.is_sink())
            .collect::<Vec<_>>();

        if buckets.is_empty() {
            return Self::select_sink(candidates, now);
        }

        if let Some(bucket) = self.select_deficit_bucket(&buckets, now) {
            return Some(bucket);
        }

        if pace && let Some(sink) = Self::select_sink(candidates, now) {
            return Some(sink);
        }

        Self::select_least_loaded_bucket(&buckets, now)
    }

    fn select_sink(candidates: &[Arc<Order>], now: Instant) -> Option<Arc<Order>> {
        candidates
            .iter()
            .filter(|order| order.is_sink())
            .min_by_key(|order| order.hashrate_1m(now))
            .cloned()
    }

    fn select_deficit_bucket(&self, buckets: &[&Arc<Order>], now: Instant) -> Option<Arc<Order>> {
        let residual = |order: &Arc<Order>| order.residual_deficit(now);

        let total = buckets
            .iter()
            .map(|order| residual(order).as_hps())
            .sum::<f64>();

        if total <= 0.0 {
            return None;
        }

        let mut draw = self.rng.lock().random::<f64>() * total;

        for order in buckets {
            let weight = residual(order).as_hps();

            if draw < weight {
                return Some(Arc::clone(order));
            }

            draw -= weight;
        }

        buckets.last().copied().cloned()
    }

    fn select_least_loaded_bucket(buckets: &[&Arc<Order>], now: Instant) -> Option<Arc<Order>> {
        buckets
            .iter()
            .map(|order| (*order, order.relative_load(order.hashrate_1m(now))))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(order, _)| Arc::clone(order))
    }

    fn overflowing_orders(orders: &[Arc<Order>], now: Instant) -> Vec<&Arc<Order>> {
        let mut overflowing = orders
            .iter()
            .filter(|order| order.is_overflowing(now))
            .map(|order| (order, order.relative_load(order.hashrate_1m(now))))
            .collect::<Vec<_>>();

        overflowing.sort_by(|a, b| b.1.total_cmp(&a.1));

        overflowing.into_iter().map(|(order, _)| order).collect()
    }

    fn persistent_overflow<'a>(
        overflowing: &[&'a Arc<Order>],
        overflow_ticks: &mut HashMap<u32, usize>,
    ) -> Vec<&'a Arc<Order>> {
        overflow_ticks.retain(|id, _| overflowing.iter().any(|order| order.id == *id));

        overflowing
            .iter()
            .copied()
            .filter(|order| {
                let ticks = overflow_ticks.entry(order.id).or_insert(0);
                *ticks += 1;
                *ticks >= OVERFLOW_PERSIST_TICKS
            })
            .collect()
    }

    fn active_sink_sessions(
        orders: &[Arc<Order>],
        now: Instant,
    ) -> Vec<(&Arc<Order>, SessionDetail)> {
        let mut sessions = orders
            .iter()
            .filter(|order| order.is_sink())
            .flat_map(|order| {
                order
                    .session_details(now)
                    .into_iter()
                    .filter(|detail| detail.hashrate > HashRate::ZERO)
                    .map(move |detail| (order, detail))
            })
            .collect::<Vec<_>>();

        sessions.sort_by_key(|(_, detail)| Reverse(detail.hashrate));

        sessions
    }

    fn trim_sinks(
        candidates: &[(&Arc<Order>, SessionDetail)],
        budget: &mut Demand,
        now: Instant,
    ) -> Trim {
        let mut max_trim = budget.deficit * HYSTERESIS_HIGH;
        let mut trimmed = Trim::default();

        for (order, detail) in candidates {
            if budget.exhausted() {
                break;
            }

            if detail.hashrate <= max_trim && order.trim_session(detail.id, now) {
                max_trim -= detail.hashrate;
                budget.consume(detail.hashrate);
                trimmed.sessions.push(*detail);
            }
        }

        trimmed
    }

    fn trim_smallest_sink(
        candidates: &[(&Arc<Order>, SessionDetail)],
        budget: &mut Demand,
        now: Instant,
    ) -> Trim {
        for (order, detail) in candidates.iter().rev() {
            if order.trim_session(detail.id, now) {
                budget.consume(detail.hashrate);

                return Trim {
                    sessions: vec![*detail],
                };
            }
        }

        Trim::default()
    }

    fn trim_overflow(
        orders: &[&Arc<Order>],
        mut budget: Option<&mut Demand>,
        now: Instant,
    ) -> Trim {
        let mut trimmed = Trim::default();

        for order in orders {
            let remaining = MAX_OVERFLOW_TRIMS_PER_TICK - trimmed.sessions.len();

            if remaining == 0 || budget.as_deref().is_some_and(|budget| budget.exhausted()) {
                break;
            }

            let order_trimmed = order.trim(Some(remaining), now);

            if let Some(budget) = budget.as_deref_mut() {
                budget.consume(order_trimmed.hashrate());
            }

            trimmed += order_trimmed;
        }

        trimmed
    }

    fn drain_sinks(orders: &[Arc<Order>], now: Instant) -> Trim {
        let mut sink_trimmed = Trim::default();

        for order in orders.iter().filter(|order| order.is_sink()) {
            for detail in order.session_details(now) {
                if !order.trim_session(detail.id, now) {
                    continue;
                }

                sink_trimmed.sessions.push(detail);
            }
        }

        sink_trimmed
    }

    pub(crate) fn rebalance(&self, orders: &[Arc<Order>], pace: bool) {
        let now = Instant::now();
        let overflowing = Self::overflowing_orders(orders, now);

        let can_shed_to_sink = pace
            && orders
                .iter()
                .any(|order| order.is_sink() && order.is_routable());

        let (demand, persistent_overflow) = {
            let mut ticks = self.ticks.lock();

            let demand = Demand::snapshot(orders, now, &mut ticks.deficit);

            let persistent_overflow = if can_shed_to_sink {
                Self::persistent_overflow(&overflowing, &mut ticks.overflow)
            } else {
                ticks.overflow.clear();

                Vec::new()
            };

            (demand, persistent_overflow)
        };

        let mut budget = demand;

        let (sink_trimmed, overflow_trimmed) = if pace {
            if !demand.exhausted() {
                let sinks = Self::active_sink_sessions(orders, now);
                let sink_trimmed = Self::trim_sinks(&sinks, &mut budget, now);
                let overflow_trimmed = Self::trim_overflow(&overflowing, Some(&mut budget), now);

                let sink_trimmed =
                    if sink_trimmed.sessions.is_empty() && overflow_trimmed.sessions.is_empty() {
                        Self::trim_smallest_sink(&sinks, &mut budget, now)
                    } else {
                        sink_trimmed
                    };

                (sink_trimmed, overflow_trimmed)
            } else {
                (
                    Trim::default(),
                    Self::trim_overflow(&persistent_overflow, None, now),
                )
            }
        } else {
            let sink_trimmed = if orders
                .iter()
                .any(|order| !order.is_sink() && order.is_routable())
            {
                let trimmed = Self::drain_sinks(orders, now);

                budget.consume(trimmed.hashrate());

                trimmed
            } else {
                Trim::default()
            };

            let overflow_trimmed = Self::trim_overflow(&overflowing, Some(&mut budget), now);

            (sink_trimmed, overflow_trimmed)
        };

        if demand.exhausted()
            && sink_trimmed.sessions.is_empty()
            && overflow_trimmed.sessions.is_empty()
        {
            return;
        }

        info!(
            "Rebalance: deficit={} surplus={} overflow_trimmed={} sessions {} sink_trimmed={} sessions {} remaining_deficit={}",
            demand.deficit,
            demand.surplus,
            overflow_trimmed.sessions.len(),
            overflow_trimmed.hashrate(),
            sink_trimmed.sessions.len(),
            sink_trimmed.hashrate(),
            budget.deficit,
        );
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        order::{Bucket, OrderStatus, Payment, Terms},
    };

    struct TestControl {
        control: Control,
        metatron: Arc<Metatron>,
        _dir: tempfile::TempDir,
    }

    impl std::ops::Deref for TestControl {
        type Target = Control;

        fn deref(&self) -> &Control {
            &self.control
        }
    }

    fn test_control() -> TestControl {
        let (metatron, dir) = Metatron::test();
        let metatron = Arc::new(metatron);

        TestControl {
            control: Control::default(),
            metatron,
            _dir: dir,
        }
    }

    fn test_address() -> Address {
        "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc"
            .parse::<Address<NetworkUnchecked>>()
            .unwrap()
            .assume_checked()
    }

    fn test_order(
        id: u32,
        target: Option<HashDays>,
        status: OrderStatus,
        metatron: &Arc<Metatron>,
    ) -> Arc<Order> {
        let terms = target.map(|target| Terms {
            payment: Payment {
                address: test_address(),
                derivation_index: 0,
                amount: Amount::from_sat(1000),
                created_at_height: 0,
                outpoint: None,
                tap_id: None,
            },
            bucket: Bucket {
                target,
                hash_rate: None,
            },
        });
        let order = Order::new(
            id,
            "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.worker@bar:3333"
                .parse()
                .unwrap(),
            terms,
            CancellationToken::new(),
            metatron.clone(),
        );

        order.force_status(status);

        if status == OrderStatus::Active {
            *order.upstream.lock() = Some(Upstream::test(id, metatron.clone()));
            let _ = order.allocator.set(Arc::new(EnonceAllocator::new(
                Extranonces::Pool(PoolExtranonces::new(4, 4).unwrap()),
                id,
            )));
        }

        order
    }

    fn test_authorization(
        enonce1: &str,
        worker: &str,
    ) -> Arc<crate::stratifier::state::Authorization> {
        Arc::new(crate::stratifier::state::Authorization {
            enonce1: enonce1.parse().unwrap(),
            address: test_address(),
            workername: worker.into(),
            username: format!("tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.{worker}")
                .parse()
                .unwrap(),
            version_mask: None,
        })
    }

    fn register_session(
        metatron: &Metatron,
        order: &Order,
        enonce1: &str,
        worker: &str,
        difficulty: f64,
    ) -> CancellationToken {
        let session =
            metatron.new_session(test_authorization(enonce1, worker), order.id, addr(4444));

        if difficulty > 0.0 {
            session.record_accepted(Difficulty::from(difficulty), Difficulty::from(difficulty));
        }

        let cancel = CancellationToken::new();
        order.add_session(session, cancel.clone());
        cancel
    }

    fn addr(port: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], port))
    }

    fn hash_days(value: f64) -> HashDays {
        HashDays::new(value).unwrap()
    }

    fn set_delivered_work(metatron: &Metatron, order: &Order, value: f64) {
        metatron.set_order_delivered_work(order.id, hash_days(value).to_hash_work());
    }

    #[test]
    fn demand_requires_persistent_deficit() {
        let control = test_control();
        let order = test_order(
            0,
            Some(hash_days(100.0)),
            OrderStatus::Active,
            &control.metatron,
        );
        register_session(&control.metatron, &order, "deadbeef", "foo", 0.0);
        let orders = [order];
        let now = Instant::now();
        let mut deficit_ticks = HashMap::new();

        for i in 1..DEFICIT_PERSIST_TICKS {
            let demand = Demand::snapshot(&orders, now, &mut deficit_ticks);
            assert_eq!(demand.deficit, HashRate::ZERO, "tick {i} trimmed early");
        }

        let demand = Demand::snapshot(&orders, now, &mut deficit_ticks);
        assert_eq!(demand.deficit, HashRate::from_hps(100.0));
    }

    #[test]
    fn demand_bypasses_debounce_without_sessions() {
        let control = test_control();
        let order = test_order(
            0,
            Some(hash_days(100.0)),
            OrderStatus::Active,
            &control.metatron,
        );
        let cancel = register_session(&control.metatron, &order, "deadbeef", "foo", 0.0);
        let orders = [order];
        let now = Instant::now();
        let mut deficit_ticks = HashMap::new();

        let demand = Demand::snapshot(&orders, now, &mut deficit_ticks);
        assert_eq!(demand.deficit, HashRate::ZERO);

        cancel.cancel();

        let demand = Demand::snapshot(&orders, now, &mut deficit_ticks);
        assert_eq!(demand.deficit, HashRate::from_hps(100.0));

        register_session(&control.metatron, &orders[0], "cafebabe", "bar", 0.0);

        let demand = Demand::snapshot(&orders, now, &mut deficit_ticks);
        assert_eq!(demand.deficit, HashRate::ZERO);
    }

    #[test]
    fn demand_surplus_skips_debounce_and_fulfilled_orders() {
        let control = test_control();
        let over = test_order(
            0,
            Some(hash_days(1.0)),
            OrderStatus::Active,
            &control.metatron,
        );
        register_session(&control.metatron, &over, "deadbeef", "foo", 10_000.0);

        let orders = [over];

        let demand = Demand::snapshot(&orders, Instant::now(), &mut HashMap::new());
        assert_eq!(demand.deficit, HashRate::ZERO);
        assert!(demand.surplus > HashRate::ZERO);

        set_delivered_work(&control.metatron, &orders[0], 1.0);

        let demand = Demand::snapshot(&orders, Instant::now(), &mut HashMap::new());
        assert_eq!(demand.surplus, HashRate::ZERO);
    }

    #[test]
    fn next_order_none_when_empty() {
        let control = test_control();
        assert!(control.next_order(&[], false).is_none());
        assert!(control.next_order(&[], true).is_none());
    }

    #[test]
    fn next_order_prefers_unserved_over_supplied_bucket() {
        let control = test_control();
        let supplied = test_order(
            0,
            Some(hash_days(100.0)),
            OrderStatus::Active,
            &control.metatron,
        );
        register_session(&control.metatron, &supplied, "deadbeef", "foo", 10_000.0);

        let unserved = test_order(
            1,
            Some(hash_days(100.0)),
            OrderStatus::Active,
            &control.metatron,
        );

        let orders = [supplied, unserved];

        assert_eq!(control.next_order(&orders, false).unwrap().id, 1);
        assert_eq!(control.next_order(&orders, true).unwrap().id, 1);
    }

    #[test]
    fn next_order_never_routes_to_sink_while_bucket_open_in_default_mode() {
        let control = test_control();
        let bucket = test_order(
            0,
            Some(hash_days(100.0)),
            OrderStatus::Active,
            &control.metatron,
        );
        register_session(&control.metatron, &bucket, "deadbeef", "foo", 10_000.0);
        let sink = test_order(1, None, OrderStatus::Active, &control.metatron);

        let orders = [bucket, sink];

        for _ in 1..=3 {
            assert_eq!(control.next_order(&orders, false).unwrap().id, 0);
        }
    }

    #[test]
    fn next_order_pace_spills_to_least_loaded_sink_when_buckets_satisfied() {
        let control = test_control();
        let bucket = test_order(
            0,
            Some(hash_days(100.0)),
            OrderStatus::Active,
            &control.metatron,
        );
        register_session(&control.metatron, &bucket, "deadbeef", "foo", 10_000.0);

        let busy_sink = test_order(1, None, OrderStatus::Active, &control.metatron);
        register_session(&control.metatron, &busy_sink, "cafebabe", "bar", 200.0);
        let quiet_sink = test_order(2, None, OrderStatus::Active, &control.metatron);
        register_session(&control.metatron, &quiet_sink, "feedface", "baz", 100.0);

        let orders = [bucket, busy_sink, quiet_sink];

        assert_eq!(control.next_order(&orders, true).unwrap().id, 2);
    }

    #[test]
    fn next_order_pace_prefers_deficit_bucket_over_sink() {
        let control = test_control();
        let bucket = test_order(
            0,
            Some(hash_days(100.0)),
            OrderStatus::Active,
            &control.metatron,
        );
        let sink = test_order(1, None, OrderStatus::Active, &control.metatron);

        let orders = [bucket, sink];

        assert_eq!(control.next_order(&orders, true).unwrap().id, 0);
    }

    #[test]
    fn next_order_routes_to_supplied_unfulfilled_bucket_when_no_alternative() {
        let control = test_control();
        let bucket = test_order(
            0,
            Some(hash_days(100.0)),
            OrderStatus::Active,
            &control.metatron,
        );
        register_session(&control.metatron, &bucket, "deadbeef", "foo", 10_000.0);

        let orders = [bucket];

        assert_eq!(control.next_order(&orders, false).unwrap().id, 0);
        assert_eq!(control.next_order(&orders, true).unwrap().id, 0);
    }

    #[test]
    fn next_order_unknown_arrival_weighted_draw() {
        let control = test_control();
        control.seed_rng(42);

        let order_a = test_order(
            0,
            Some(hash_days(50.0)),
            OrderStatus::Active,
            &control.metatron,
        );
        let order_b = test_order(
            1,
            Some(hash_days(100.0)),
            OrderStatus::Active,
            &control.metatron,
        );

        let orders = [order_a, order_b];

        let mut counts = [0usize; 2];

        for _ in 1..=30 {
            let picked = control.next_order(&orders, false).unwrap().id;
            counts[picked as usize] += 1;
        }

        assert!(counts[0] > 0, "residual-50 order never picked: {counts:?}");
        assert!(
            counts[1] > counts[0],
            "residual-100 order should win majority: {counts:?}"
        );
    }

    #[test]
    fn next_order_fallback_prefers_lower_relative_load() {
        let control = test_control();
        let overflowing = test_order(
            0,
            Some(hash_days(1e9)),
            OrderStatus::Active,
            &control.metatron,
        );
        register_session(&control.metatron, &overflowing, "deadbeef", "foo", 20.0);

        let barely_over = test_order(
            1,
            Some(hash_days(1e11)),
            OrderStatus::Active,
            &control.metatron,
        );
        register_session(&control.metatron, &barely_over, "cafebabe", "bar", 1467.0);

        let orders = [overflowing, barely_over];

        assert_eq!(control.next_order(&orders, false).unwrap().id, 1);
    }

    #[test]
    fn next_order_prefers_sink_with_least_hashrate() {
        #[track_caller]
        fn case(a_diff: f64, b_diff: f64, expected: u32) {
            let control = test_control();
            let sink_a = test_order(0, None, OrderStatus::Active, &control.metatron);
            let sink_b = test_order(1, None, OrderStatus::Active, &control.metatron);

            if a_diff > 0.0 {
                let session = control.metatron.new_session(
                    test_authorization("deadbeef", "foo"),
                    0,
                    addr(4444),
                );
                sink_a.add_session(session.clone(), CancellationToken::new());
                session.record_accepted(Difficulty::from(a_diff), Difficulty::from(a_diff));
            }

            if b_diff > 0.0 {
                let session = control.metatron.new_session(
                    test_authorization("cafebabe", "bar"),
                    1,
                    addr(4444),
                );
                sink_b.add_session(session.clone(), CancellationToken::new());
                session.record_accepted(Difficulty::from(b_diff), Difficulty::from(b_diff));
            }

            let orders = [sink_a, sink_b];

            assert_eq!(control.next_order(&orders, false).unwrap().id, expected);
        }

        case(100.0, 0.0, 1);
        case(200.0, 100.0, 1);
        case(100.0, 200.0, 0);
    }

    #[test]
    fn rebalance_trims_sinks_to_cover_deficit_when_paced() {
        let control = test_control();
        let bucket = test_order(
            0,
            Some(hash_days(1e9)),
            OrderStatus::Active,
            &control.metatron,
        );

        let sink = test_order(1, None, OrderStatus::Active, &control.metatron);
        let cancel_a = register_session(&control.metatron, &sink, "aaaaaaaa", "foo", 10.0);
        let cancel_b = register_session(&control.metatron, &sink, "bbbbbbbb", "bar", 6.0);
        let cancel_c = register_session(&control.metatron, &sink, "cccccccc", "baz", 2.0);
        let cancel_d = register_session(&control.metatron, &sink, "dddddddd", "qux", 2.0);

        control.rebalance(&[bucket, sink], true);

        assert!(cancel_a.is_cancelled());
        assert!(cancel_b.is_cancelled());
        assert!(!cancel_c.is_cancelled());
        assert!(!cancel_d.is_cancelled());
    }

    #[test]
    fn rebalance_drains_all_sinks_when_pace_off() {
        let control = test_control();
        let over = overflowing_order(&control, 0);
        let sink_a = test_order(1, None, OrderStatus::Active, &control.metatron);
        let sink_b = test_order(2, None, OrderStatus::Active, &control.metatron);
        let cancels = [
            register_session(&control.metatron, &sink_a, "aaaaaaaa", "foo", 10.0),
            register_session(&control.metatron, &sink_a, "bbbbbbbb", "bar", 0.0),
            register_session(&control.metatron, &sink_b, "cccccccc", "baz", 5.0),
        ];

        let orders = [over.order.clone(), sink_a, sink_b];

        control.rebalance(&orders, false);

        assert!(cancels.iter().all(|cancel| cancel.is_cancelled()));
        assert!(!over.cancel_mid.is_cancelled());
    }

    #[test]
    fn rebalance_pace_off_skips_sinks_without_routable_bucket() {
        #[track_caller]
        fn case(fulfilled: bool, connected: bool) {
            let control = test_control();
            let bucket = test_order(
                0,
                Some(hash_days(100.0)),
                OrderStatus::Active,
                &control.metatron,
            );

            if fulfilled {
                set_delivered_work(&control.metatron, &bucket, 100.0);
            }

            if !connected {
                bucket.upstream().unwrap().set_connected(false);
            }

            let sink = test_order(1, None, OrderStatus::Active, &control.metatron);
            let cancel = register_session(&control.metatron, &sink, "deadbeef", "foo", 100.0);

            let orders = [bucket, sink];

            for _ in 0..DEFICIT_PERSIST_TICKS {
                control.rebalance(&orders, false);
            }

            assert!(!cancel.is_cancelled());
        }

        case(true, true);
        case(false, false);
    }

    #[test]
    fn rebalance_pace_off_consumes_deficit_with_drained_sinks() {
        #[track_caller]
        fn case(starving_target: f64, expected_overflow_trimmed: bool) {
            let control = test_control();
            let over = overflowing_order(&control, 0);
            let starving = test_order(
                1,
                Some(hash_days(starving_target)),
                OrderStatus::Active,
                &control.metatron,
            );
            register_session(&control.metatron, &starving, "eeee", "baz", 0.0);
            let sink = test_order(2, None, OrderStatus::Active, &control.metatron);
            let sink_cancel = register_session(&control.metatron, &sink, "dddd", "qux", 100.0);

            let orders = [over.order.clone(), starving, sink];

            for _ in 1..DEFICIT_PERSIST_TICKS {
                control.rebalance(&orders, true);
            }

            assert!(!sink_cancel.is_cancelled());

            control.rebalance(&orders, false);

            assert!(sink_cancel.is_cancelled());
            assert_eq!(over.cancel_mid.is_cancelled(), expected_overflow_trimmed);
        }

        case(1e9, false);
        case(1e20, true);
    }

    #[test]
    fn rebalance_sink_shed_before_overflow_trim() {
        let control = test_control();
        let over = overflowing_order(&control, 0);
        let starving = test_order(
            1,
            Some(hash_days(1e9)),
            OrderStatus::Active,
            &control.metatron,
        );
        register_session(&control.metatron, &starving, "eeee", "qux", 10.0);

        let sink = test_order(2, None, OrderStatus::Active, &control.metatron);
        let sink_cancel = register_session(&control.metatron, &sink, "dddd", "qux", 5.0);

        let orders = [over.order.clone(), starving, sink];

        for _ in 0..DEFICIT_PERSIST_TICKS {
            control.rebalance(&orders, true);
        }

        assert!(sink_cancel.is_cancelled());
        assert!(!over.cancel_mid.is_cancelled());
    }

    #[test]
    fn rebalance_overflow_shed_before_oversize_sink_fallback() {
        let control = test_control();
        let over = overflowing_order(&control, 0);
        let starving = test_order(
            1,
            Some(hash_days(1e9)),
            OrderStatus::Active,
            &control.metatron,
        );
        let sink = test_order(2, None, OrderStatus::Active, &control.metatron);
        let sink_cancel = register_session(&control.metatron, &sink, "dddd", "qux", 100.0);

        control.rebalance(&[over.order.clone(), starving, sink], true);

        assert!(over.cancel_mid.is_cancelled());
        assert!(!sink_cancel.is_cancelled());

        let control = test_control();
        let whale = test_order(
            0,
            Some(hash_days(1e9)),
            OrderStatus::Active,
            &control.metatron,
        );
        let whale_cancel = register_session(&control.metatron, &whale, "aaaa", "foo", 30.0);
        let starving = test_order(
            1,
            Some(hash_days(1e9)),
            OrderStatus::Active,
            &control.metatron,
        );
        let sink = test_order(2, None, OrderStatus::Active, &control.metatron);
        let sink_small = register_session(&control.metatron, &sink, "bbbb", "bar", 50.0);
        let sink_large = register_session(&control.metatron, &sink, "cccc", "baz", 100.0);

        control.rebalance(&[whale, starving, sink], true);

        assert!(!whale_cancel.is_cancelled());
        assert!(sink_small.is_cancelled());
        assert!(!sink_large.is_cancelled());
    }

    #[test]
    fn rebalance_paced_partial_sink_falls_through_to_overflow() {
        let control = test_control();
        let over = overflowing_order(&control, 0);
        let starving = test_order(
            1,
            Some(hash_days(1e9)),
            OrderStatus::Active,
            &control.metatron,
        );
        let sink = test_order(2, None, OrderStatus::Active, &control.metatron);
        let sink_cancel = register_session(&control.metatron, &sink, "dddd", "qux", 5.0);

        control.rebalance(&[over.order.clone(), starving, sink], true);

        assert!(sink_cancel.is_cancelled());
        assert!(over.cancel_mid.is_cancelled());
    }

    #[test]
    fn rebalance_sink_fit_and_oversize_fallback() {
        #[track_caller]
        fn case(target: f64, whale_diff: f64, small_diff: f64) {
            let control = test_control();
            let active = test_order(
                0,
                Some(hash_days(target)),
                OrderStatus::Active,
                &control.metatron,
            );
            let sink_a = test_order(1, None, OrderStatus::Active, &control.metatron);
            let sink_b = test_order(2, None, OrderStatus::Active, &control.metatron);

            let cancel_whale =
                register_session(&control.metatron, &sink_a, "deadbeef", "foo", whale_diff);
            let cancel_small =
                register_session(&control.metatron, &sink_b, "cafebabe", "bar", small_diff);

            control.rebalance(&[active, sink_a, sink_b], true);

            assert!(!cancel_whale.is_cancelled());
            assert!(cancel_small.is_cancelled());
        }

        case(1e9, 25.0, 13.5);
        case(100.0, 200.0, 100.0);
    }

    #[test]
    fn rebalance_sink_skips_session_after_max_trim_shrinks() {
        let control = test_control();
        let bucket = test_order(
            0,
            Some(hash_days(1e9)),
            OrderStatus::Active,
            &control.metatron,
        );
        let sink = test_order(1, None, OrderStatus::Active, &control.metatron);
        let cancel_a = register_session(&control.metatron, &sink, "aaaaaaaa", "foo", 10.0);
        let cancel_b = register_session(&control.metatron, &sink, "bbbbbbbb", "bar", 9.0);
        let cancel_c = register_session(&control.metatron, &sink, "cccccccc", "baz", 5.0);

        control.rebalance(&[bucket, sink], true);

        assert!(cancel_a.is_cancelled());
        assert!(!cancel_b.is_cancelled());
        assert!(cancel_c.is_cancelled());
    }

    #[test]
    fn rebalance_paces_persistent_overflow_to_sink() {
        let control = test_control();
        let over = overflowing_order(&control, 0);
        let satisfied = test_order(
            1,
            Some(hash_days(1.0)),
            OrderStatus::Active,
            &control.metatron,
        );
        register_session(&control.metatron, &satisfied, "feedface", "qux", 1000.0);

        let sink = test_order(2, None, OrderStatus::Active, &control.metatron);
        let sink_cancel = register_session(&control.metatron, &sink, "deadbeef", "foo", 100.0);

        let orders = [over.order.clone(), satisfied, sink];

        for i in 1..OVERFLOW_PERSIST_TICKS {
            control.rebalance(&orders, true);
            assert!(!over.cancel_mid.is_cancelled(), "tick {i} trimmed early");
        }

        control.rebalance(&orders, true);

        assert!(!over.cancel_fat.is_cancelled());
        assert!(over.cancel_mid.is_cancelled());
        assert!(!over.cancel_small.is_cancelled());
        assert!(!sink_cancel.is_cancelled());
    }

    #[test]
    fn rebalance_skips_overflow_pacing_without_routable_sink_or_pace_off() {
        #[track_caller]
        fn case(pace: bool, has_sink: bool, sink_connected: bool) {
            let control = test_control();
            let over = overflowing_order(&control, 0);

            let mut orders = vec![over.order.clone()];

            if has_sink {
                let sink = test_order(1, None, OrderStatus::Active, &control.metatron);
                if !sink_connected {
                    sink.upstream().unwrap().set_connected(false);
                }
                orders.push(sink);
            }

            for _ in 0..OVERFLOW_PERSIST_TICKS {
                control.rebalance(&orders, pace);
            }

            assert!(!over.cancel_mid.is_cancelled());
        }

        case(false, true, true);
        case(true, false, false);
        case(true, true, false);
    }

    #[test]
    fn rebalance_overflow_ticks_reset_on_interruption() {
        #[derive(Clone, Copy)]
        enum Interruption {
            PaceOff,
            SinkDisconnected,
            OrderDisconnected,
        }

        #[track_caller]
        fn case(interruption: Interruption) {
            let control = test_control();
            let over = overflowing_order(&control, 0);
            let sink = test_order(1, None, OrderStatus::Active, &control.metatron);
            let orders = [over.order.clone(), sink.clone()];

            for _ in 1..OVERFLOW_PERSIST_TICKS {
                control.rebalance(&orders, true);
            }

            match interruption {
                Interruption::PaceOff => control.rebalance(&orders, false),
                Interruption::SinkDisconnected => {
                    sink.upstream().unwrap().set_connected(false);
                    control.rebalance(&orders, true);
                    sink.upstream().unwrap().set_connected(true);
                }
                Interruption::OrderDisconnected => {
                    over.order.upstream().unwrap().set_connected(false);
                    control.rebalance(&orders, true);
                    over.order.upstream().unwrap().set_connected(true);
                }
            }

            for i in 1..OVERFLOW_PERSIST_TICKS {
                control.rebalance(&orders, true);
                assert!(!over.cancel_mid.is_cancelled(), "tick {i} trimmed early");
            }

            control.rebalance(&orders, true);
            assert!(over.cancel_mid.is_cancelled());
        }

        case(Interruption::PaceOff);
        case(Interruption::SinkDisconnected);
        case(Interruption::OrderDisconnected);
    }

    #[test]
    fn rebalance_overflow_ticks_accumulate_while_sinks_cover_deficit() {
        let control = test_control();
        let over = overflowing_order(&control, 0);
        let starving = test_order(
            1,
            Some(hash_days(1e9)),
            OrderStatus::Active,
            &control.metatron,
        );
        let sink = test_order(2, None, OrderStatus::Active, &control.metatron);
        let sink_cancels = (1..OVERFLOW_PERSIST_TICKS)
            .map(|i| register_session(&control.metatron, &sink, &format!("{i:08x}"), "qux", 15.0))
            .collect::<Vec<_>>();

        let orders = [over.order.clone(), starving.clone(), sink];

        for i in 1..OVERFLOW_PERSIST_TICKS {
            control.rebalance(&orders, true);
            assert_eq!(
                sink_cancels
                    .iter()
                    .filter(|cancel| cancel.is_cancelled())
                    .count(),
                i,
            );
            assert!(!over.cancel_mid.is_cancelled(), "tick {i} trimmed early");
        }

        register_session(&control.metatron, &starving, "ffffffff", "baz", 15.0);

        control.rebalance(&orders, true);

        assert!(over.cancel_mid.is_cancelled());
    }

    #[test]
    fn rebalance_persistent_overflow_prefers_heaviest_and_skips_untrimmable() {
        let control = test_control();
        let lighter = overflowing_order(&control, 0);
        let heavier = overflowing_order(&control, 1);
        register_session(&control.metatron, &heavier.order, "dddd", "qux", 2.0);
        let sink = test_order(2, None, OrderStatus::Active, &control.metatron);
        let orders = [lighter.order.clone(), heavier.order.clone(), sink];

        for _ in 0..OVERFLOW_PERSIST_TICKS {
            control.rebalance(&orders, true);
        }

        assert!(heavier.cancel_mid.is_cancelled());
        assert!(!lighter.cancel_mid.is_cancelled());

        let control = test_control();
        let whale = test_order(
            0,
            Some(hash_days(1e9)),
            OrderStatus::Active,
            &control.metatron,
        );
        let whale_cancel = register_session(&control.metatron, &whale, "dddd", "qux", 30.0);
        let over = overflowing_order(&control, 1);
        let sink = test_order(2, None, OrderStatus::Active, &control.metatron);
        let orders = [whale, over.order.clone(), sink];

        for _ in 0..OVERFLOW_PERSIST_TICKS {
            control.rebalance(&orders, true);
        }

        assert!(!whale_cancel.is_cancelled());
        assert!(over.cancel_mid.is_cancelled());
    }

    #[test]
    fn rebalance_skips_disconnected_or_fulfilled_overflowing_order() {
        #[track_caller]
        fn case(disconnected: bool, pace: bool, has_starving: bool) {
            let control = test_control();
            let over = overflowing_order(&control, 0);

            if disconnected {
                over.order.upstream().unwrap().set_connected(false);
            } else {
                set_delivered_work(&control.metatron, &over.order, 1e9);
            }

            let mut orders = vec![over.order.clone()];

            if has_starving {
                orders.push(test_order(
                    1,
                    Some(hash_days(1e20)),
                    OrderStatus::Active,
                    &control.metatron,
                ));
            } else {
                orders.push(test_order(2, None, OrderStatus::Active, &control.metatron));
            }

            for _ in 0..OVERFLOW_PERSIST_TICKS {
                control.rebalance(&orders, pace);
            }

            assert!(!over.cancel_mid.is_cancelled());
        }

        case(true, false, true);
        case(true, true, true);
        case(true, true, false);
        case(false, false, true);
        case(false, true, true);
        case(false, true, false);
    }

    #[test]
    fn rebalance_ignores_zero_rate_sink_sessions_when_paced() {
        let control = test_control();
        let bucket = test_order(
            0,
            Some(hash_days(100.0)),
            OrderStatus::Active,
            &control.metatron,
        );

        let sink = test_order(1, None, OrderStatus::Active, &control.metatron);
        let cancel_a = register_session(&control.metatron, &sink, "deadbeef", "foo", 0.0);
        let cancel_b = register_session(&control.metatron, &sink, "cafebabe", "bar", 0.0);

        control.rebalance(&[bucket, sink], true);

        assert!(!cancel_a.is_cancelled());
        assert!(!cancel_b.is_cancelled());
    }

    #[test]
    fn rebalance_does_not_count_an_already_cancelled_session() {
        let control = test_control();
        let bucket = test_order(
            0,
            Some(hash_days(100.0)),
            OrderStatus::Active,
            &control.metatron,
        );

        let sink = test_order(1, None, OrderStatus::Active, &control.metatron);
        let live = register_session(&control.metatron, &sink, "deadbeef", "foo", 200.0);
        let already_cancelled =
            register_session(&control.metatron, &sink, "cafebabe", "bar", 100.0);
        already_cancelled.cancel();

        control.rebalance(&[bucket, sink], true);

        assert!(
            live.is_cancelled(),
            "cancelled session consumed trim budget"
        );
    }

    struct OverflowingOrder {
        order: Arc<Order>,
        cancel_fat: CancellationToken,
        cancel_mid: CancellationToken,
        cancel_small: CancellationToken,
    }

    fn overflowing_order(control: &TestControl, id: u32) -> OverflowingOrder {
        let order = test_order(
            id,
            Some(hash_days(1e9)),
            OrderStatus::Active,
            &control.metatron,
        );

        let cancel_fat = register_session(&control.metatron, &order, "aaaa", "foo", 13.0);
        let cancel_mid = register_session(&control.metatron, &order, "bbbb", "bar", 7.0);
        let cancel_small = register_session(&control.metatron, &order, "cccc", "baz", 4.0);

        OverflowingOrder {
            order,
            cancel_fat,
            cancel_mid,
            cancel_small,
        }
    }

    #[test]
    fn rebalance_sheds_overflow_when_another_order_starving() {
        #[track_caller]
        fn case(pace: bool) {
            let control = test_control();
            let over = overflowing_order(&control, 0);
            let starving = test_order(
                1,
                Some(hash_days(1e20)),
                OrderStatus::Active,
                &control.metatron,
            );

            control.rebalance(&[over.order.clone(), starving], pace);

            assert!(!over.cancel_fat.is_cancelled());
            assert!(over.cancel_mid.is_cancelled());
            assert!(!over.cancel_small.is_cancelled());
        }

        case(false);
        case(true);
    }

    #[test]
    fn rebalance_trims_heaviest_overflow_first() {
        #[track_caller]
        fn case(pace: bool) {
            let control = test_control();
            let lighter = overflowing_order(&control, 0);
            let heavier = overflowing_order(&control, 1);
            register_session(&control.metatron, &heavier.order, "dddd", "qux", 2.0);
            let starving = test_order(
                2,
                Some(hash_days(1.0)),
                OrderStatus::Active,
                &control.metatron,
            );

            control.rebalance(
                &[lighter.order.clone(), heavier.order.clone(), starving],
                pace,
            );

            assert!(heavier.cancel_mid.is_cancelled());
            assert!(!lighter.cancel_mid.is_cancelled());
        }

        case(false);
        case(true);
    }

    #[test]
    fn rebalance_skips_starving_order_with_disconnected_upstream() {
        #[track_caller]
        fn case(pace: bool) {
            let control = test_control();
            let zombie = test_order(
                0,
                Some(hash_days(100.0)),
                OrderStatus::Active,
                &control.metatron,
            );
            zombie.upstream().unwrap().set_connected(false);

            let sink = test_order(1, None, OrderStatus::Active, &control.metatron);
            let cancel = register_session(&control.metatron, &sink, "deadbeef", "foo", 100.0);

            let orders = [zombie, sink];

            control.rebalance(&orders, pace);

            assert!(!cancel.is_cancelled());
        }

        case(false);
        case(true);
    }
}
