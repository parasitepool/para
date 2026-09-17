use {
    super::*,
    order::{Order, Trim},
    rand::{Rng, SeedableRng, rngs::StdRng},
};

pub(crate) const TRIM_COOLDOWN: Duration = Duration::from_secs(600);
const MAX_TRIMS_PER_TICK: usize = 1;
const DEFICIT_PERSIST_TICKS: usize = 2;

#[derive(Clone, Copy, Debug)]
struct Demand {
    has_unfulfilled_bucket: bool,
    deficit: HashRate,
    severe: bool,
}

impl Demand {
    fn snapshot(
        orders: &[Arc<Order>],
        now: Instant,
        deficit_ticks: &mut HashMap<u32, usize>,
    ) -> Self {
        deficit_ticks.retain(|id, _| orders.iter().any(|order| order.id == *id));

        let mut has_unfulfilled_bucket = false;
        let mut deficit = HashRate::ZERO;
        let mut severe = false;

        for order in orders.iter().filter(|order| !order.is_sink()) {
            has_unfulfilled_bucket |= order.has_connected_upstream() && !order.is_fulfilled();

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
            severe |= order.is_severely_starving(now);
        }

        Self {
            has_unfulfilled_bucket,
            deficit,
            severe,
        }
    }

    fn exhausted(self) -> bool {
        self.deficit == HashRate::ZERO
    }

    fn consume(&mut self, trimmed: &Trim) {
        self.deficit -= trimmed.hashrate;
    }
}

fn log_rebalance(
    demand: Demand,
    session_budget: usize,
    overflow_trimmed: &Trim,
    sink_trimmed: &Trim,
    remaining: HashRate,
) {
    debug!(
        "Rebalance decision: deficit={} severe={} session_budget={} overflow_sessions={} overflow_hashrate={} sink_sessions={} sink_hashrate={} remaining_deficit={remaining}",
        demand.deficit,
        demand.severe,
        session_budget,
        overflow_trimmed.sessions.len(),
        overflow_trimmed.hashrate,
        sink_trimmed.sessions.len(),
        sink_trimmed.hashrate,
    );
}

pub(crate) struct Control {
    cooldowns: Mutex<HashMap<Extranonce, Instant>>,
    deficit_ticks: Mutex<HashMap<u32, usize>>,
    rng: Mutex<StdRng>,
}

impl Default for Control {
    fn default() -> Self {
        Self {
            cooldowns: Mutex::new(HashMap::new()),
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
            return Arc::clone(
                buckets
                    .iter()
                    .max_by_key(|order| residual(order))
                    .expect("buckets is non-empty"),
            );
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

    fn trim_budget(&self, severe: bool, boost: bool) -> usize {
        if severe || boost {
            usize::MAX
        } else {
            MAX_TRIMS_PER_TICK
        }
    }

    pub(crate) fn rebalance(&self, orders: &[Arc<Order>], boost: bool) {
        let now = Instant::now();

        self.cooldowns
            .lock()
            .retain(|_, since| now.duration_since(*since) < TRIM_COOLDOWN);

        let demand = Demand::snapshot(orders, now, &mut self.deficit_ticks.lock());

        if !demand.has_unfulfilled_bucket || demand.deficit == HashRate::ZERO {
            return;
        }

        let mut budget = demand;
        let mut session_budget = self.trim_budget(demand.severe, boost);
        let mut overflow_trimmed = Trim::default();
        let mut sink_trimmed = Trim::default();

        {
            let cooldowns = self.cooldowns.lock();

            for order in orders.iter().filter(|order| order.is_overflowing(now)) {
                if budget.exhausted() || session_budget == 0 {
                    break;
                }

                let trimmed = order.trim(Some(session_budget), now, &cooldowns);
                session_budget -= trimmed.sessions.len();
                budget.consume(&trimmed);
                overflow_trimmed += trimmed;
            }
        }

        let mut candidates = Vec::new();

        {
            let cooldowns = self.cooldowns.lock();

            for order in orders.iter().filter(|order| order.is_sink()) {
                for detail in order.session_details(now) {
                    if cooldowns
                        .get(&detail.enonce1)
                        .is_some_and(|since| now.duration_since(*since) < TRIM_COOLDOWN)
                    {
                        continue;
                    }

                    candidates.push((order, detail));
                }
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
                hashrate: detail.hashrate,
                sessions: vec![detail.clone()],
            };

            session_budget -= 1;
            budget.consume(&trimmed);
            sink_trimmed += trimmed;
        }

        {
            let mut cooldowns = self.cooldowns.lock();

            for detail in overflow_trimmed
                .sessions
                .iter()
                .chain(sink_trimmed.sessions.iter())
            {
                cooldowns.insert(detail.enonce1.clone(), now);
            }
        }

        log_rebalance(
            demand,
            session_budget,
            &overflow_trimmed,
            &sink_trimmed,
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
        order.add_session(session, cancel.clone(), addr(4444));
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
    fn demand_keeps_unfulfilled_bucket_without_deficit() {
        let control = test_control();
        let fed = test_order(
            0,
            Some(hash_days(1.0)),
            OrderStatus::Active,
            &control.metatron,
        );
        register_session(&control.metatron, &fed, "deadbeef", "foo", 1000.0);

        let disconnected = test_order(
            1,
            Some(hash_days(100.0)),
            OrderStatus::Active,
            &control.metatron,
        );
        disconnected.upstream().unwrap().set_connected(false);
        let orders = [fed, disconnected];

        let demand = Demand::snapshot(&orders, Instant::now(), &mut HashMap::new());
        assert!(demand.has_unfulfilled_bucket);
        assert_eq!(demand.deficit, HashRate::ZERO);
        assert!(!demand.severe);
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
                sink_a.add_session(session.clone(), CancellationToken::new(), addr(1));
                session.record_accepted(Difficulty::from(a_diff), Difficulty::from(a_diff));
            }

            if b_diff > 0.0 {
                let session = control.metatron.new_session(
                    test_authorization("cafebabe", "bar"),
                    1,
                    addr(4444),
                );
                sink_b.add_session(session.clone(), CancellationToken::new(), addr(2));
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
    fn rebalance_cooldown_skips_recently_trimmed() {
        let control = test_control();
        let bucket = test_order(
            0,
            Some(hash_days(100.0)),
            OrderStatus::Active,
            &control.metatron,
        );

        let sink = test_order(1, None, OrderStatus::Active, &control.metatron);
        let cancel_a = register_session(&control.metatron, &sink, "deadbeef", "foo", 200.0);
        let cancel_b = register_session(&control.metatron, &sink, "cafebabe", "bar", 100.0);

        control
            .cooldowns
            .lock()
            .insert("deadbeef".parse().unwrap(), Instant::now());

        let orders = [bucket, sink];

        for _ in 0..DEFICIT_PERSIST_TICKS {
            control.rebalance(&orders, false);
        }

        assert!(!cancel_a.is_cancelled());
        assert!(cancel_b.is_cancelled());
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
        sink_a.add_session(session_a.clone(), cancel_a.clone(), addr(1));
        sink_b.add_session(session_b.clone(), cancel_b.clone(), addr(2));
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
        let cooldowns = control.cooldowns.lock();
        assert!(!cooldowns.contains_key(&"deadbeef".parse().unwrap()));
        assert!(cooldowns.contains_key(&"cafebabe".parse().unwrap()));
        drop(cooldowns);
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
