use {
    super::*,
    order::{Order, Trim},
    rand::{Rng, SeedableRng, rngs::StdRng},
};

const MAX_TRIMS_PER_TICK: usize = 1;
const DEFICIT_PERSIST_TICKS: usize = 2;

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

    fn consume(&mut self, trimmed: &Trim) {
        self.deficit -= trimmed.hashrate();
    }
}

pub(crate) struct Control {
    deficit_ticks: Mutex<HashMap<u32, usize>>,
    rng: Mutex<StdRng>,
}

impl Default for Control {
    fn default() -> Self {
        Self {
            deficit_ticks: Mutex::new(HashMap::new()),
            rng: Mutex::new(StdRng::from_rng(&mut rand::rng())),
        }
    }
}

impl Control {
    #[cfg(test)]
    pub(crate) fn seed_rng(&self, seed: u64) {
        *self.rng.lock() = StdRng::seed_from_u64(seed);
    }

    pub(crate) fn next_order(&self, candidates: &[Arc<Order>]) -> Option<Arc<Order>> {
        let now = Instant::now();

        let buckets = candidates
            .iter()
            .filter(|order| !order.is_sink())
            .collect::<Vec<_>>();

        if buckets.is_empty() {
            candidates
                .iter()
                .filter(|order| order.is_sink())
                .min_by_key(|order| order.hashrate_1m(now))
                .cloned()
        } else {
            Some(self.select_bucket(&buckets, now))
        }
    }

    fn select_bucket(&self, buckets: &[&Arc<Order>], now: Instant) -> Arc<Order> {
        let residual = |order: &Arc<Order>| order.residual_deficit(now);

        let total = buckets
            .iter()
            .map(|order| residual(order).as_hps())
            .sum::<f64>();

        if total <= 0.0 {
            let loads = buckets
                .iter()
                .map(|order| (*order, order.relative_load(order.hashrate_1m(now))))
                .collect::<Vec<_>>();

            let (least_loaded, _) = loads
                .iter()
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .expect("buckets is non-empty");

            return Arc::clone(least_loaded);
        }

        let mut draw = self.rng.lock().random::<f64>() * total;

        for order in buckets {
            let weight = residual(order).as_hps();

            if draw < weight {
                return Arc::clone(order);
            }

            draw -= weight;
        }

        Arc::clone(buckets.last().expect("buckets is non-empty"))
    }

    fn trim_budget(&self, boost: bool) -> usize {
        if boost {
            usize::MAX
        } else {
            MAX_TRIMS_PER_TICK
        }
    }

    pub(crate) fn rebalance(&self, orders: &[Arc<Order>], boost: bool) {
        let now = Instant::now();

        let demand = Demand::snapshot(orders, now, &mut self.deficit_ticks.lock());

        if demand.exhausted() {
            return;
        }

        let mut budget = demand;
        let mut session_budget = self.trim_budget(boost);
        let mut overflow_trimmed = Trim::default();
        let mut sink_trimmed = Trim::default();

        for order in orders.iter().filter(|order| order.is_overflowing(now)) {
            if budget.exhausted() || session_budget == 0 {
                break;
            }

            let trimmed = order.trim(Some(session_budget), now);
            session_budget -= trimmed.sessions.len();
            budget.consume(&trimmed);
            overflow_trimmed += trimmed;
        }

        let mut candidates = Vec::new();

        for order in orders.iter().filter(|order| order.is_sink()) {
            for detail in order.session_details(now) {
                candidates.push((order, detail));
            }
        }

        candidates.sort_by_key(|(_, detail)| Reverse(detail.hashrate));

        for (order, detail) in &candidates {
            if budget.exhausted() || session_budget == 0 {
                break;
            }

            if detail.hashrate == HashRate::ZERO && !sink_trimmed.sessions.is_empty() {
                break;
            }

            if !order.trim_session(detail.id, now) {
                continue;
            }

            let trimmed = Trim {
                sessions: vec![*detail],
            };

            session_budget -= 1;
            budget.consume(&trimmed);
            sink_trimmed += trimmed;
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
        order::{Bucket, OrderStatus, Payment},
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
        let bucket = target.map(|target| Bucket {
            target,
            payment: Payment::new(test_address(), 0, Amount::from_sat(1000), 0),
        });
        let order = Order::new(
            id,
            "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.worker@bar:3333"
                .parse()
                .unwrap(),
            bucket,
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
        assert!(control.next_order(&[]).is_none());
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

        assert_eq!(control.next_order(&orders).unwrap().id, 1);
    }

    #[test]
    fn next_order_never_routes_to_sink_while_bucket_open() {
        let control = test_control();
        let bucket = test_order(
            0,
            Some(hash_days(100.0)),
            OrderStatus::Active,
            &control.metatron,
        );
        let sink = test_order(1, None, OrderStatus::Active, &control.metatron);

        let orders = [bucket, sink];

        for _ in 1..=3 {
            assert_eq!(control.next_order(&orders).unwrap().id, 0);
        }
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

        assert_eq!(control.next_order(&orders).unwrap().id, 0);
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
            let picked = control.next_order(&orders).unwrap().id;
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

        assert_eq!(control.next_order(&orders).unwrap().id, 1);
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

            assert_eq!(control.next_order(&orders).unwrap().id, expected);
        }

        case(100.0, 0.0, 1);
        case(200.0, 100.0, 1);
        case(100.0, 200.0, 0);
    }

    #[test]
    fn rebalance_trim_budget() {
        #[track_caller]
        fn case(boost: bool, expected_trimmed: usize) {
            let control = test_control();
            let bucket = test_order(
                0,
                Some(hash_days(1e9)),
                OrderStatus::Active,
                &control.metatron,
            );
            register_session(&control.metatron, &bucket, "eeeeeeee", "qux", 10.0);

            let sink = test_order(1, None, OrderStatus::Active, &control.metatron);
            let cancels = [
                register_session(&control.metatron, &sink, "aaaaaaaa", "foo", 1.0),
                register_session(&control.metatron, &sink, "bbbbbbbb", "bar", 1.0),
                register_session(&control.metatron, &sink, "cccccccc", "baz", 1.0),
            ];

            let orders = [bucket, sink];

            for _ in 0..DEFICIT_PERSIST_TICKS {
                control.rebalance(&orders, boost);
            }

            assert_eq!(
                cancels
                    .iter()
                    .filter(|cancel| cancel.is_cancelled())
                    .count(),
                expected_trimmed,
            );
        }

        case(false, 1);
        case(true, 3);
    }

    #[test]
    fn rebalance_caps_trims_for_fully_starved_bucket_without_boost() {
        let control = test_control();
        let bucket = test_order(
            0,
            Some(hash_days(1e9)),
            OrderStatus::Active,
            &control.metatron,
        );

        let sink = test_order(1, None, OrderStatus::Active, &control.metatron);
        let cancels = [
            register_session(&control.metatron, &sink, "aaaaaaaa", "foo", 1.0),
            register_session(&control.metatron, &sink, "bbbbbbbb", "bar", 1.0),
            register_session(&control.metatron, &sink, "cccccccc", "baz", 1.0),
        ];

        let orders = [bucket, sink];

        for _ in 0..DEFICIT_PERSIST_TICKS {
            control.rebalance(&orders, false);
        }

        assert_eq!(
            cancels
                .iter()
                .filter(|cancel| cancel.is_cancelled())
                .count(),
            1,
            "fully-starved bucket must not unlock unlimited trims without boost",
        );
    }

    #[test]
    fn rebalance_overflow_trimmed_before_sink_shed() {
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
        let sink_cancel = register_session(&control.metatron, &sink, "dddd", "qux", 100.0);

        let orders = [over.order.clone(), starving, sink];

        for _ in 0..DEFICIT_PERSIST_TICKS {
            control.rebalance(&orders, false);
        }

        assert!(over.cancel_mid.is_cancelled());
        assert!(!sink_cancel.is_cancelled());
    }

    #[test]
    fn rebalance_trims_fattest_sink_when_bucket_starving() {
        let control = test_control();
        let active = test_order(
            0,
            Some(hash_days(100.0)),
            OrderStatus::Active,
            &control.metatron,
        );
        let sink_a = test_order(1, None, OrderStatus::Active, &control.metatron);
        let sink_b = test_order(2, None, OrderStatus::Active, &control.metatron);

        let cancel_a = CancellationToken::new();
        let cancel_b = CancellationToken::new();
        let session_a =
            control
                .metatron
                .new_session(test_authorization("deadbeef", "foo"), 1, addr(4444));
        let session_b =
            control
                .metatron
                .new_session(test_authorization("cafebabe", "bar"), 2, addr(4444));
        sink_a.add_session(session_a.clone(), cancel_a.clone());
        sink_b.add_session(session_b.clone(), cancel_b.clone());
        session_a.record_accepted(Difficulty::from(200.0), Difficulty::from(200.0));
        session_b.record_accepted(Difficulty::from(100.0), Difficulty::from(100.0));

        let orders = [active, sink_a, sink_b];

        for _ in 0..DEFICIT_PERSIST_TICKS {
            control.rebalance(&orders, false);
        }

        assert!(cancel_a.is_cancelled());
        assert!(!cancel_b.is_cancelled());
    }

    #[test]
    fn rebalance_noop_without_starving_demand() {
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

        control.rebalance(&orders, false);

        assert!(!over.cancel_mid.is_cancelled());
        assert!(!sink_cancel.is_cancelled());
    }

    #[test]
    fn rebalance_falls_back_to_zero_rate_sink_session() {
        let control = test_control();
        let bucket = test_order(
            0,
            Some(hash_days(100.0)),
            OrderStatus::Active,
            &control.metatron,
        );

        let sink = test_order(1, None, OrderStatus::Active, &control.metatron);
        let cancel = register_session(&control.metatron, &sink, "deadbeef", "foo", 0.0);

        let orders = [bucket, sink];

        for _ in 0..DEFICIT_PERSIST_TICKS {
            control.rebalance(&orders, false);
        }

        assert!(cancel.is_cancelled());
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
        let already_cancelled =
            register_session(&control.metatron, &sink, "deadbeef", "foo", 200.0);
        let live = register_session(&control.metatron, &sink, "cafebabe", "bar", 100.0);
        already_cancelled.cancel();

        for _ in 0..DEFICIT_PERSIST_TICKS {
            control.rebalance(&[bucket.clone(), sink.clone()], false);
        }

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
        let control = test_control();
        let over = overflowing_order(&control, 0);
        let starving = test_order(
            1,
            Some(hash_days(1e20)),
            OrderStatus::Active,
            &control.metatron,
        );

        let orders = [over.order.clone(), starving];

        for _ in 0..DEFICIT_PERSIST_TICKS {
            control.rebalance(&orders, false);
        }

        assert!(!over.cancel_fat.is_cancelled());
        assert!(over.cancel_mid.is_cancelled());
        assert!(!over.cancel_small.is_cancelled());
    }

    #[test]
    fn rebalance_does_not_shed_sink_after_overflow_covers_deficit() {
        let control = test_control();
        let over = overflowing_order(&control, 0);
        let starving = test_order(
            1,
            Some(hash_days(1.0)),
            OrderStatus::Active,
            &control.metatron,
        );
        let sink = test_order(2, None, OrderStatus::Active, &control.metatron);
        let sink_cancel = register_session(&control.metatron, &sink, "dddd", "qux", 100.0);

        let orders = [over.order.clone(), starving, sink];

        for _ in 0..DEFICIT_PERSIST_TICKS {
            control.rebalance(&orders, false);
        }

        assert!(over.cancel_mid.is_cancelled());
        assert!(!sink_cancel.is_cancelled());
    }

    #[test]
    fn rebalance_stops_trimming_overflow_after_deficit_is_covered() {
        let control = test_control();
        let first = overflowing_order(&control, 0);
        let second = overflowing_order(&control, 1);
        let starving = test_order(
            2,
            Some(hash_days(1.0)),
            OrderStatus::Active,
            &control.metatron,
        );

        let orders = [first.order.clone(), second.order.clone(), starving];

        for _ in 0..DEFICIT_PERSIST_TICKS {
            control.rebalance(&orders, false);
        }

        assert!(first.cancel_mid.is_cancelled());
        assert!(!second.cancel_mid.is_cancelled());
    }

    #[test]
    fn rebalance_skips_starving_order_with_disconnected_upstream() {
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

        control.rebalance(&orders, false);

        assert!(!cancel.is_cancelled());
    }

    #[test]
    fn rebalance_boost_noop_without_connected_unfulfilled_bucket() {
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
            control.rebalance(&orders, true);

            assert!(!cancel.is_cancelled());
        }

        case(true, true);
        case(false, false);
    }
}
