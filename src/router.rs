use {
    super::*,
    crate::api::{DownstreamInfo, MiningStats, RouterStatus},
    orders::Orders,
};

pub(crate) mod error;
pub(crate) mod order;
pub(crate) mod orders;

pub(crate) use error::{RouterError, RouterResult};
pub use order::{Order, OrderKind, OrderStatus};

const RECLAIM_WINDOW: Duration = Duration::from_secs(24 * 3600);

pub(crate) struct Router {
    settings: Arc<Settings>,
    metatron: Arc<Metatron>,
    wallet: Arc<Wallet>,
    orders: RwLock<Orders>,
    next_id: AtomicU32,
    tasks: TaskTracker,
    cancel: CancellationToken,
}

impl Router {
    pub(crate) fn new(
        settings: Arc<Settings>,
        metatron: Arc<Metatron>,
        wallet: Arc<Wallet>,
        tasks: TaskTracker,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            settings,
            metatron,
            wallet,
            orders: RwLock::new(Orders::new()),
            next_id: AtomicU32::new(0),
            tasks,
            cancel,
        }
    }

    pub(crate) fn add_order(
        self: &Arc<Self>,
        upstream_target: UpstreamTarget,
        kind: OrderKind,
        price: HashPrice,
    ) -> RouterResult<Arc<Order>> {
        let payment_amount = match kind {
            OrderKind::Sink => Amount::ZERO,
            OrderKind::Bucket(hashdays) => {
                if hashdays.as_f64() <= 0.0 {
                    return Err(RouterError::InvalidHashdays);
                }

                let minimum = self.settings.hash_price();

                if price < minimum {
                    return Err(RouterError::HashPriceBelowMinimum {
                        bid: price,
                        minimum,
                    });
                }

                price
                    .total(hashdays)
                    .ok_or(RouterError::HashPriceOverflow)?
                    .max(self.wallet.dust_limit())
            }
        };

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);

        let address_info = self.wallet.reserve_address();

        let order = Order::new(
            id,
            upstream_target,
            kind,
            address_info.address,
            address_info.index,
            payment_amount,
            self.settings.invoice_timeout(),
            self.cancel.child_token(),
            self.metatron.clone(),
        );

        self.orders.write().add(order.clone());
        self.spawn_order_monitor(order.clone());

        Ok(order)
    }

    pub(crate) fn cancel_order(&self, id: u32) -> Option<Arc<Order>> {
        let order = self.orders.read().get(id)?;
        let previous = order.force_cancel();
        if previous != OrderStatus::Cancelled {
            info!(
                "Order {} at {} transitioned from {:?} to {:?}",
                order.id,
                order.upstream_target,
                previous,
                OrderStatus::Cancelled,
            );
        }
        Some(order)
    }

    pub(crate) fn get_order(&self, id: u32) -> Option<Arc<Order>> {
        self.orders.read().get(id)
    }

    pub(crate) fn orders(&self) -> Vec<Arc<Order>> {
        self.orders.read().all()
    }

    pub(crate) fn next_order(&self) -> Option<Arc<Order>> {
        let now = Instant::now();
        let routable = self.orders.read().routable(now);

        let mut best: Option<&Arc<Order>> = None;

        for order in &routable {
            let Some(current) = best else {
                best = Some(order);
                continue;
            };

            let prefer_order = if order.is_ramping_up() != current.is_ramping_up() {
                !order.is_ramping_up()
            } else if order.is_sink() && current.is_sink() {
                order.hashrate_1m(now) < current.hashrate_1m(now)
            } else {
                order.remaining_work() > current.remaining_work()
            };

            if prefer_order {
                best = Some(order);
            }
        }

        let order = best?;
        order.assign();
        Some(order.clone())
    }

    pub(crate) fn metatron(&self) -> Arc<Metatron> {
        self.metatron.clone()
    }

    pub(crate) fn status(&self) -> RouterStatus {
        let now = Instant::now();
        let metatron = &self.metatron;
        let orders = self.orders.read().all();
        let mut upstream = Stats::new();
        let mut bucket_order_count = 0;
        let mut sink_order_count = 0;
        let mut capacity_hashrate = HashRate::ZERO;
        let mut available_hashrate = HashRate::ZERO;

        for order in &orders {
            if order.status() != OrderStatus::Active {
                continue;
            }
            let stats = metatron.upstream_stats(order.id);
            let hashrate = stats.hashrate_1m(now);
            capacity_hashrate += hashrate;
            if order.is_sink() {
                sink_order_count += 1;
                available_hashrate += hashrate;
            } else {
                bucket_order_count += 1;
            }
            upstream.absorb(stats, now);
        }

        RouterStatus {
            uptime_secs: metatron.uptime().as_secs(),
            hash_price: self.settings.hash_price(),
            capacity_hashrate,
            available_hashrate,
            bucket_order_count,
            sink_order_count,
            upstream: MiningStats::from_snapshot(&upstream, now),
            downstream: DownstreamInfo::from_metatron(metatron, now),
        }
    }

    fn terminate_order(&self, order: &Order, status: OrderStatus) {
        if let Some(previous) = order.terminate(status) {
            info!(
                "Order {} at {} transitioned from {:?} to {:?}",
                order.id, order.upstream_target, previous, status,
            );
        }
    }

    fn spawn_order_monitor(self: &Arc<Self>, order: Arc<Order>) {
        let router = self.clone();
        self.tasks.spawn(async move {
            if !order.is_sink() {
                if !router.wait_for_payment(&order).await {
                    return;
                }

                if order.status() != OrderStatus::Pending {
                    return;
                }
            }

            if let Err(err) = order
                .activate(
                    router.settings.timeout(),
                    router.settings.enonce1_extension_size(),
                    &router.tasks,
                )
                .await
            {
                error!("Failed to connect upstream for order {}: {err}", order.id);
                if order.transition(OrderStatus::Pending, OrderStatus::Disconnected) {
                    order.cancel.cancel();
                }
                return;
            }

            if !order.transition(OrderStatus::Pending, OrderStatus::Active) {
                return;
            }

            info!("Order {} activated", order.id);

            router.run_active_order(&order).await;
        });
    }

    async fn wait_for_payment(self: &Arc<Self>, order: &Arc<Order>) -> bool {
        let mut ticker = tokio::time::interval(self.settings.tick_interval());

        loop {
            tokio::select! {
                biased;
                _ = order.cancel.cancelled() => {
                    if self.wallet.confirmed_received(order.payment_derivation_index) > Amount::ZERO {
                        order.set_status(OrderStatus::PaidLate);
                    } else {
                        self.wallet.release_address(order.payment_derivation_index);
                    }
                    return false;
                }
                _ = ticker.tick() => {
                    let elapsed = order.created_at.elapsed();
                    let received = self.wallet.confirmed_received(order.payment_derivation_index);

                    if order.ready_for_activation(received, elapsed) {
                        return true;
                    }

                    match order.status() {
                        OrderStatus::PaidLate => return false,
                        OrderStatus::Expired
                            if elapsed >= order.payment_timeout + RECLAIM_WINDOW =>
                        {
                            self.wallet.release_address(order.payment_derivation_index);
                            return false;
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    async fn run_active_order(self: &Arc<Self>, order: &Arc<Order>) {
        let check_interval = self.settings.tick_interval();
        let upstream = order.upstream().expect("just activated");

        tokio::select! {
            biased;
            _ = order.cancel.cancelled() => {
                self.terminate_order(order, OrderStatus::Cancelled);
            }
            _ = upstream.disconnected() => {
                warn!(
                    "Upstream {} disconnected, order {} marked disconnected",
                    upstream.endpoint(),
                    order.id,
                );

                self.terminate_order(order, OrderStatus::Disconnected);
            }
            _ = async {
                let mut ticker = tokio::time::interval(check_interval);
                loop {
                    ticker.tick().await;

                    if order.is_fulfilled() {
                        break;
                    }

                    order.trim();
                }
            } => {
                info!("Order {} fulfilled", order.id);
                self.terminate_order(order, OrderStatus::Fulfilled);
            }
        }
    }

    pub(crate) fn spawn_rebalance_loop(self: &Arc<Self>) {
        let router = self.clone();
        self.tasks.spawn(async move {
            let mut ticker = tokio::time::interval(router.settings.tick_interval());
            loop {
                tokio::select! {
                    biased;
                    _ = router.cancel.cancelled() => break,
                    _ = ticker.tick() => router.rebalance(),
                }
            }
        });
    }

    fn rebalance(&self) {
        let now = Instant::now();
        let orders = self.orders.read().active();

        if !orders.iter().any(|order| order.is_starving(now)) {
            return;
        }

        let mut best = None;

        for order in orders.iter().filter(|order| order.is_sink()) {
            let sessions = order.sessions.lock();
            for (session, _) in sessions.values() {
                let hashrate = session.hashrate_1m(now);
                let replace = match best {
                    None => true,
                    Some((_, _, best_hashrate)) => hashrate > best_hashrate,
                };
                if replace {
                    best = Some((order, session.id(), hashrate));
                }
            }
        }

        let Some((order, id, _)) = best else {
            warn!("Rebalance needed but no sink session available");
            return;
        };

        order.trim_session(id, now);
    }
}

impl StatusLine for Router {
    fn status_line(&self) -> String {
        let now = Instant::now();
        let all = self.orders();
        let stats = self.metatron.snapshot();
        let connected = all
            .iter()
            .filter(|order| {
                order.status() == OrderStatus::Active
                    && order.upstream().is_some_and(|u| u.is_connected())
            })
            .count();

        format!(
            "upstreams={}/{}  sessions={}  hashrate={:.2}",
            connected,
            all.iter()
                .filter(|order| order.status() == OrderStatus::Active)
                .count(),
            self.metatron.total_sessions(),
            stats.hashrate_1m(now),
        )
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

    fn test_wallet() -> Wallet {
        let mnemonic: bdk_wallet::keys::bip39::Mnemonic =
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about"
                .parse()
                .unwrap();

        let (_, descriptor, change_descriptor) =
            Wallet::generate_from_mnemonic(mnemonic, bitcoin::Network::Regtest).unwrap();

        Wallet::new(
            &descriptor,
            Some(&change_descriptor),
            bitcoin::Network::Regtest,
            "http://127.0.0.1:1",
            bdk_bitcoind_rpc::bitcoincore_rpc::Auth::None,
            0,
        )
        .unwrap()
    }

    fn test_router() -> Arc<Router> {
        Arc::new(Router::new(
            Arc::new(Settings::default()),
            Arc::new(Metatron::new()),
            Arc::new(test_wallet()),
            TaskTracker::new(),
            CancellationToken::new(),
        ))
    }

    fn test_order(
        id: u32,
        kind: OrderKind,
        status: OrderStatus,
        metatron: &Arc<Metatron>,
    ) -> Arc<Order> {
        let order = Order::new(
            id,
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
        );

        order.set_status(status);

        if status == OrderStatus::Active {
            let _ = order.upstream.set(Upstream::test(id, metatron.clone()));
            let _ = order.allocator.set(Arc::new(EnonceAllocator::new(
                Extranonces::Pool(PoolExtranonces::new(4, 4).unwrap()),
                id,
            )));
        }

        order
    }

    fn hashdays(value: f64) -> HashDays {
        HashDays::new(value).unwrap()
    }

    fn set_accepted_work(metatron: &Metatron, order: &Order, value: f64) {
        metatron.set_upstream_accepted_work(order.id, hashdays(value).to_total_work());
    }

    fn add_orders(router: &Router, orders: impl IntoIterator<Item = Arc<Order>>) {
        let mut stored = router.orders.write();

        for order in orders {
            stored.add(order);
        }
    }

    fn ids(orders: Vec<Arc<Order>>) -> Vec<u32> {
        orders.into_iter().map(|order| order.id).collect()
    }

    #[test]
    fn is_fulfilled() {
        #[track_caller]
        fn case(kind: OrderKind, accepted: Option<f64>, expected: bool) {
            let metatron = Arc::new(Metatron::new());
            let order = test_order(0, kind, OrderStatus::Active, &metatron);

            if let Some(accepted) = accepted {
                set_accepted_work(&metatron, order.as_ref(), accepted);
            }

            assert_eq!(order.is_fulfilled(), expected);
        }

        case(OrderKind::Sink, None, false);
        case(OrderKind::Bucket(hashdays(1e15)), None, false);
        case(OrderKind::Bucket(hashdays(1e12)), Some(1e12), true);
        case(OrderKind::Bucket(hashdays(1e12)), Some(2e12), true);
    }

    #[test]
    fn status_serde() {
        #[track_caller]
        fn case(status: OrderStatus, expected: &str) {
            assert_eq!(serde_json::to_string(&status).unwrap(), expected);
        }

        case(OrderStatus::Pending, "\"pending\"");
        case(OrderStatus::Active, "\"active\"");
        case(OrderStatus::Fulfilled, "\"fulfilled\"");
        case(OrderStatus::Cancelled, "\"cancelled\"");
        case(OrderStatus::Disconnected, "\"disconnected\"");
        case(OrderStatus::Expired, "\"expired\"");
        case(OrderStatus::PaidLate, "\"paid_late\"");
    }

    #[test]
    fn orders_active_returns_only_active_status() {
        let metatron = Arc::new(Metatron::new());
        let mut orders = Orders::new();
        orders.add(test_order(
            0,
            OrderKind::Sink,
            OrderStatus::Pending,
            &metatron,
        ));
        orders.add(test_order(
            1,
            OrderKind::Bucket(hashdays(100.0)),
            OrderStatus::Active,
            &metatron,
        ));
        orders.add(test_order(
            2,
            OrderKind::Sink,
            OrderStatus::Active,
            &metatron,
        ));

        assert_eq!(ids(orders.active()), vec![1, 2]);
    }

    #[test]
    fn orders_get() {
        let metatron = Arc::new(Metatron::new());
        let mut orders = Orders::new();
        orders.add(test_order(
            0,
            OrderKind::Sink,
            OrderStatus::Pending,
            &metatron,
        ));

        assert_eq!(orders.get(0).unwrap().id, 0);
        assert!(orders.get(1).is_none());
    }

    #[test]
    fn next_order_none_when_empty() {
        let router = test_router();
        assert!(router.next_order().is_none());
    }

    #[test]
    fn next_order_prefers_bucket_over_sink() {
        let router = test_router();
        let bucket = test_order(
            0,
            OrderKind::Bucket(hashdays(100.0)),
            OrderStatus::Active,
            &router.metatron,
        );
        let sink = test_order(1, OrderKind::Sink, OrderStatus::Active, &router.metatron);

        add_orders(router.as_ref(), [sink, bucket]);

        assert_eq!(router.next_order().unwrap().id, 0);
    }

    #[test]
    fn next_order_prefers_least_filled_bucket() {
        let router = test_router();
        let mostly_empty = test_order(
            0,
            OrderKind::Bucket(hashdays(1e20)),
            OrderStatus::Active,
            &router.metatron,
        );
        let partly_filled = test_order(
            1,
            OrderKind::Bucket(hashdays(1e20)),
            OrderStatus::Active,
            &router.metatron,
        );
        router
            .metatron
            .set_upstream_accepted_work(1, hashdays(1e10).to_total_work());

        add_orders(router.as_ref(), [partly_filled, mostly_empty]);

        assert_eq!(router.next_order().unwrap().id, 0);
    }

    #[test]
    fn next_order_second_call_sees_first_as_ramping_up() {
        let router = test_router();
        let active = test_order(
            0,
            OrderKind::Bucket(hashdays(100.0)),
            OrderStatus::Active,
            &router.metatron,
        );
        let sink = test_order(1, OrderKind::Sink, OrderStatus::Active, &router.metatron);

        add_orders(router.as_ref(), [active, sink]);

        let first = router.next_order().unwrap();
        assert_eq!(first.id, 0);

        let second = router.next_order().unwrap();
        assert_eq!(second.id, 1);
    }

    #[test]
    fn next_order_probe_disconnect_clears_ramping_up() {
        let router = test_router();
        let bucket = test_order(
            0,
            OrderKind::Bucket(hashdays(100.0)),
            OrderStatus::Active,
            &router.metatron,
        );
        let sink = test_order(1, OrderKind::Sink, OrderStatus::Active, &router.metatron);
        add_orders(router.as_ref(), [bucket, sink]);

        let first = router.next_order().unwrap();
        assert_eq!(first.id, 0);

        first.unassign();
        drop(first);

        let second = router.next_order().unwrap();
        assert_eq!(second.id, 0);
    }

    #[test]
    fn next_order_burst_spreads_across_bucket_orders() {
        let router = test_router();
        let order_0 = test_order(
            0,
            OrderKind::Bucket(hashdays(100.0)),
            OrderStatus::Active,
            &router.metatron,
        );
        let order_1 = test_order(
            1,
            OrderKind::Bucket(hashdays(100.0)),
            OrderStatus::Active,
            &router.metatron,
        );
        let order_2 = test_order(
            2,
            OrderKind::Bucket(hashdays(100.0)),
            OrderStatus::Active,
            &router.metatron,
        );
        add_orders(router.as_ref(), [order_0, order_1, order_2]);

        let first = router.next_order().unwrap();
        let second = router.next_order().unwrap();
        let third = router.next_order().unwrap();

        let mut picked = [first.id, second.id, third.id];
        picked.sort();
        assert_eq!(picked, [0, 1, 2]);
    }

    #[test]
    fn next_order_skips_full_bucket_order() {
        let router = test_router();
        let active = test_order(
            0,
            OrderKind::Bucket(hashdays(100.0)),
            OrderStatus::Active,
            &router.metatron,
        );

        let session = router
            .metatron
            .new_session(test_authorization("deadbeef", "foo"), 0);
        session.record_accepted(Difficulty::from(1000.0), Difficulty::from(1000.0));
        active.add_session(session, CancellationToken::new());

        add_orders(router.as_ref(), [active]);

        assert!(router.next_order().is_none());
    }

    #[test]
    fn next_order_prefers_sink_with_least_hashrate() {
        #[track_caller]
        fn case(a_diff: f64, b_diff: f64, expected: u32) {
            let router = test_router();
            let sink_a = test_order(0, OrderKind::Sink, OrderStatus::Active, &router.metatron);
            let sink_b = test_order(1, OrderKind::Sink, OrderStatus::Active, &router.metatron);

            if a_diff > 0.0 {
                let session = router
                    .metatron
                    .new_session(test_authorization("deadbeef", "foo"), 0);
                sink_a.add_session(session.clone(), CancellationToken::new());
                session.record_accepted(Difficulty::from(a_diff), Difficulty::from(a_diff));
            }

            if b_diff > 0.0 {
                let session = router
                    .metatron
                    .new_session(test_authorization("cafebabe", "bar"), 1);
                sink_b.add_session(session.clone(), CancellationToken::new());
                session.record_accepted(Difficulty::from(b_diff), Difficulty::from(b_diff));
            }

            add_orders(router.as_ref(), [sink_a, sink_b]);

            assert_eq!(router.next_order().unwrap().id, expected);
        }

        case(100.0, 0.0, 1);
        case(200.0, 100.0, 1);
        case(100.0, 200.0, 0);
    }

    #[test]
    fn rebalance_trims_fattest_sink_when_bucket_starving() {
        let router = test_router();
        let active = test_order(
            0,
            OrderKind::Bucket(hashdays(100.0)),
            OrderStatus::Active,
            &router.metatron,
        );
        let sink_a = test_order(1, OrderKind::Sink, OrderStatus::Active, &router.metatron);
        let sink_b = test_order(2, OrderKind::Sink, OrderStatus::Active, &router.metatron);

        let cancel_a = CancellationToken::new();
        let cancel_b = CancellationToken::new();
        let session_a = router
            .metatron
            .new_session(test_authorization("deadbeef", "foo"), 1);
        let session_b = router
            .metatron
            .new_session(test_authorization("cafebabe", "bar"), 2);
        sink_a.add_session(session_a.clone(), cancel_a.clone());
        sink_b.add_session(session_b.clone(), cancel_b.clone());
        session_a.record_accepted(Difficulty::from(200.0), Difficulty::from(200.0));
        session_b.record_accepted(Difficulty::from(100.0), Difficulty::from(100.0));

        add_orders(router.as_ref(), [active, sink_a, sink_b]);

        router.rebalance();

        assert!(cancel_a.is_cancelled());
        assert!(!cancel_b.is_cancelled());
    }

    #[test]
    fn rebalance_noop_when_no_starving_bucket() {
        let router = test_router();
        let active = test_order(
            0,
            OrderKind::Bucket(hashdays(100.0)),
            OrderStatus::Active,
            &router.metatron,
        );
        let active_session = router
            .metatron
            .new_session(test_authorization("feedface", "baz"), 0);
        active_session.record_accepted(Difficulty::from(1000.0), Difficulty::from(1000.0));
        active.add_session(active_session, CancellationToken::new());

        let sink = test_order(1, OrderKind::Sink, OrderStatus::Active, &router.metatron);

        let cancel = CancellationToken::new();
        let session = router
            .metatron
            .new_session(test_authorization("deadbeef", "foo"), 1);
        sink.add_session(session, cancel.clone());

        add_orders(router.as_ref(), [active, sink]);

        router.rebalance();

        assert!(!cancel.is_cancelled());
    }

    fn test_authorization(
        enonce1: &str,
        workername: &str,
    ) -> Arc<crate::stratifier::state::Authorization> {
        Arc::new(crate::stratifier::state::Authorization {
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
    fn trim_session_cancels_matching_token() {
        let metatron = Arc::new(Metatron::new());
        let order = test_order(0, OrderKind::Sink, OrderStatus::Pending, &metatron);

        let cancel_kept = CancellationToken::new();
        let cancel_trimmed = CancellationToken::new();
        let kept = metatron.new_session(test_authorization("deadbeef", "foo"), 0);
        let trimmed = metatron.new_session(test_authorization("cafebabe", "bar"), 0);
        order.add_session(kept.clone(), cancel_kept.clone());
        order.add_session(trimmed.clone(), cancel_trimmed.clone());

        order.trim_session(trimmed.id(), Instant::now());

        assert!(cancel_trimmed.is_cancelled());
        assert!(!cancel_kept.is_cancelled());
    }

    #[test]
    fn ready_for_activation() {
        #[track_caller]
        fn case<F>(received: Amount, elapsed: F, expected: bool, status: OrderStatus)
        where
            F: FnOnce(&Order) -> Duration,
        {
            let metatron = Arc::new(Metatron::new());
            let order = test_order(0, OrderKind::Sink, OrderStatus::Pending, &metatron);
            let elapsed = elapsed(order.as_ref());

            assert_eq!(order.ready_for_activation(received, elapsed), expected);
            assert_eq!(order.status(), status);
        }

        case(
            Amount::from_sat(1000),
            |_| Duration::from_secs(59),
            true,
            OrderStatus::Pending,
        );
        case(
            Amount::ZERO,
            |order| order.payment_timeout,
            false,
            OrderStatus::Expired,
        );
        case(
            Amount::from_sat(1000),
            |order| order.payment_timeout,
            false,
            OrderStatus::PaidLate,
        );
    }

    #[test]
    fn transitions_require_pending_status() {
        #[track_caller]
        fn case(to: OrderStatus) {
            let metatron = Arc::new(Metatron::new());
            let order = test_order(0, OrderKind::Sink, OrderStatus::Pending, &metatron);
            order.set_status(OrderStatus::Cancelled);

            assert!(!order.transition(OrderStatus::Pending, to));
            assert_eq!(order.status(), OrderStatus::Cancelled);
        }

        case(OrderStatus::Active);
        case(OrderStatus::Disconnected);
    }
}
