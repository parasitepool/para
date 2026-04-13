use {super::*, orders::Orders};

pub(crate) mod error;
pub(crate) mod order;
pub(crate) mod orders;

pub(crate) use error::{RouterError, RouterResult};
pub use order::{Order, OrderStatus};

const RECLAIM_WINDOW: Duration = Duration::from_secs(24 * 3600);

pub(crate) struct Router {
    metatron: Arc<Metatron>,
    orders: RwLock<Orders>,
    next_id: AtomicU32,
    settings: Arc<Settings>,
    tasks: TaskTracker,
    cancel: CancellationToken,
    wallet: Arc<Wallet>,
}

impl Router {
    pub(crate) fn new(
        metatron: Arc<Metatron>,
        settings: Arc<Settings>,
        tasks: TaskTracker,
        cancel: CancellationToken,
        wallet: Arc<Wallet>,
    ) -> Self {
        Self {
            metatron,
            orders: RwLock::new(Orders::new()),
            next_id: AtomicU32::new(0),
            settings,
            tasks,
            cancel,
            wallet,
        }
    }

    pub(crate) fn add_order(
        self: &Arc<Self>,
        upstream_target: UpstreamTarget,
        hashdays: Option<HashDays>,
        price: HashPrice,
    ) -> RouterResult<Arc<Order>> {
        let payment_amount = if let Some(hashdays) = hashdays {
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
        } else {
            Amount::ZERO
        };

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);

        let address_info = self.wallet.reserve_address();

        let order = Order::new(
            id,
            upstream_target,
            hashdays,
            address_info.address,
            address_info.index,
            payment_amount,
            self.settings.invoice_timeout(),
            self.cancel.child_token(),
        );

        self.orders.write().add(order.clone());
        self.spawn_order_monitor(order.clone());

        Ok(order)
    }

    pub(crate) fn cancel_order(&self, id: u32) -> Option<Arc<Order>> {
        let order = self.orders.read().get(id)?;
        self.terminate_order(&order, OrderStatus::Cancelled);
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
        let orders = self.orders.read();
        let paid = orders.active_paid();

        let best = paid
            .iter()
            .filter_map(|order| {
                let remaining = order.remaining_work(&self.metatron)?;
                let hashrate = order.hashrate_1m(&self.metatron, now);
                Some((order, hashrate.0 / remaining.as_f64()))
            })
            .min_by(|(_, a), (_, b)| a.total_cmp(b));

        match best {
            Some((order, _)) => Some(order.clone()),
            None => orders.active_default().into_iter().min_by(|a, b| {
                a.hashrate_1m(&self.metatron, now)
                    .0
                    .total_cmp(&b.hashrate_1m(&self.metatron, now).0)
            }),
        }
    }

    pub(crate) fn metatron(&self) -> Arc<Metatron> {
        self.metatron.clone()
    }

    fn terminate_order(&self, order: &Order, status: OrderStatus) {
        let previous = order.status();

        if previous != status {
            info!(
                "Order {} at {} transitioned from {:?} to {:?}",
                order.id, order.upstream_target, previous, status,
            );
        }

        order.set_status(status);
        order.cancel.cancel();
    }

    fn spawn_order_monitor(self: &Arc<Self>, order: Arc<Order>) {
        let router = self.clone();
        self.tasks.spawn(async move {
            if !order.is_default() {
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
                    router.metatron.clone(),
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
                    if order.is_fulfilled(&self.metatron) {
                        break;
                    }
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
        let orders = self.orders.read();
        let active_paid = orders.active_paid();
        let active_default = orders.active_default();

        let starving_paid = active_paid
            .iter()
            .filter(|order| order.hashrate_1m(&self.metatron, now) == HashRate::ZERO)
            .map(|order| order.id)
            .collect::<Vec<_>>();

        if starving_paid.is_empty() {
            return;
        }

        if let Some(order) = active_default.iter().max_by(|a, b| {
            a.hashrate_1m(&self.metatron, now)
                .0
                .total_cmp(&b.hashrate_1m(&self.metatron, now).0)
        }) {
            info!(
                "Rebalancing: starving_paid_orders={starving_paid:?} trimming 1 session from default order {} at {} (hashrate_1m={})",
                order.id,
                order.upstream_target,
                order.hashrate_1m(&self.metatron, now),
            );

            order.trim_sessions(1);
        } else {
            warn!(
                "Rebalance needed but no active default order available: starving_paid_orders={starving_paid:?}"
            );
        }
    }
}

impl StatusLine for Router {
    fn status_line(&self) -> String {
        let now = Instant::now();
        let all = self.orders();
        let stats = self.metatron.snapshot();
        let connected = all
            .iter()
            .filter(|o| {
                o.status() == OrderStatus::Active && o.upstream().is_some_and(|u| u.is_connected())
            })
            .count();

        format!(
            "upstreams={}/{}  sessions={}  hashrate={:.2}",
            connected,
            all.iter()
                .filter(|o| o.status() == OrderStatus::Active)
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
            Arc::new(Metatron::new()),
            Arc::new(Settings::default()),
            TaskTracker::new(),
            CancellationToken::new(),
            Arc::new(test_wallet()),
        ))
    }

    fn test_order(
        id: u32,
        hashdays: Option<HashDays>,
        status: OrderStatus,
        metatron: &Arc<Metatron>,
    ) -> Arc<Order> {
        let order = Order::new(
            id,
            "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.worker@bar:3333"
                .parse()
                .unwrap(),
            hashdays,
            test_address(),
            0,
            Amount::from_sat(1000),
            Duration::from_secs(3600),
            CancellationToken::new(),
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

    fn record_hashrate(
        router: &Router,
        order_id: u32,
        enonce1: &str,
        workername: &str,
        difficulty: f64,
    ) {
        let session = router
            .metatron
            .new_session(test_authorization(enonce1, workername), order_id);

        let difficulty = Difficulty::from(difficulty);
        session.record_accepted(difficulty, difficulty);
    }

    fn ids(orders: Vec<Arc<Order>>) -> Vec<u32> {
        orders.into_iter().map(|order| order.id).collect()
    }

    #[test]
    fn is_fulfilled() {
        #[track_caller]
        fn case(target: Option<f64>, accepted: Option<f64>, expected: bool) {
            let metatron = Arc::new(Metatron::new());
            let order = test_order(0, target.map(hashdays), OrderStatus::Active, &metatron);

            if let Some(accepted) = accepted {
                set_accepted_work(&metatron, order.as_ref(), accepted);
            }

            assert_eq!(order.is_fulfilled(&metatron), expected);
        }

        case(None, None, false);
        case(Some(1e15), None, false);
        case(Some(1e12), Some(1e12), true);
        case(Some(1e12), Some(2e12), true);
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
    fn orders_filter_active_orders() {
        let metatron = Arc::new(Metatron::new());
        let mut orders = Orders::new();
        orders.add(test_order(0, None, OrderStatus::Pending, &metatron));
        orders.add(test_order(
            1,
            Some(hashdays(100.0)),
            OrderStatus::Active,
            &metatron,
        ));
        orders.add(test_order(2, None, OrderStatus::Active, &metatron));

        assert_eq!(ids(orders.active_paid()), vec![1]);
        assert_eq!(ids(orders.active_default()), vec![2]);
    }

    #[test]
    fn orders_get() {
        let metatron = Arc::new(Metatron::new());
        let mut orders = Orders::new();
        orders.add(test_order(0, None, OrderStatus::Pending, &metatron));

        assert_eq!(orders.get(0).unwrap().id, 0);
        assert!(orders.get(1).is_none());
    }

    #[test]
    fn remaining_work() {
        #[track_caller]
        fn case(target: Option<f64>, accepted: Option<f64>, expected: Option<f64>) {
            let metatron = Arc::new(Metatron::new());
            let order = test_order(0, target.map(hashdays), OrderStatus::Active, &metatron);

            if let Some(accepted) = accepted {
                set_accepted_work(&metatron, order.as_ref(), accepted);
            }

            assert_eq!(
                order.remaining_work(&metatron).map(HashDays::as_f64),
                expected
            );
        }

        case(None, None, None);
        case(Some(100.0), None, Some(100.0));
        case(Some(100.0), Some(40.0), Some(60.0));
        case(Some(100.0), Some(100.0), None);
        case(Some(100.0), Some(120.0), None);
    }

    #[test]
    fn next_order_none_when_empty() {
        let router = test_router();
        assert!(router.next_order().is_none());
    }

    #[test]
    fn next_order_falls_back_to_default_when_paid_fulfilled() {
        let router = test_router();
        let target = hashdays(100.0);
        let paid = test_order(0, Some(target), OrderStatus::Active, &router.metatron);
        set_accepted_work(&router.metatron, paid.as_ref(), target.as_f64());
        let default = test_order(1, None, OrderStatus::Active, &router.metatron);

        add_orders(router.as_ref(), [paid, default]);

        assert_eq!(router.next_order().unwrap().id, 1);
    }

    #[test]
    fn next_order_prefers_paid_order_with_lowest_hashrate_per_remaining_work() {
        let router = test_router();
        let order_a = test_order(
            0,
            Some(hashdays(100.0)),
            OrderStatus::Active,
            &router.metatron,
        );
        let order_b = test_order(
            1,
            Some(hashdays(100.0)),
            OrderStatus::Active,
            &router.metatron,
        );

        set_accepted_work(&router.metatron, order_a.as_ref(), 20.0);
        set_accepted_work(&router.metatron, order_b.as_ref(), 60.0);

        record_hashrate(router.as_ref(), 0, "deadbeef", "foo", 100.0);
        record_hashrate(router.as_ref(), 1, "cafebabe", "bar", 100.0);

        add_orders(router.as_ref(), [order_a, order_b]);

        assert_eq!(router.next_order().unwrap().id, 0);
    }

    #[test]
    fn next_order_picks_lowest_hashrate_default() {
        let router = test_router();
        let order_a = test_order(0, None, OrderStatus::Active, &router.metatron);
        let order_b = test_order(1, None, OrderStatus::Active, &router.metatron);

        record_hashrate(router.as_ref(), 0, "deadbeef", "foo", 100.0);
        add_orders(router.as_ref(), [order_a, order_b]);

        assert_eq!(router.next_order().unwrap().id, 1);
    }

    #[test]
    fn rebalance_noop_without_starving_paid() {
        let router = test_router();
        let default = test_order(0, None, OrderStatus::Active, &router.metatron);
        let token = default.register_session();

        add_orders(router.as_ref(), [default]);
        router.rebalance();

        assert!(!token.is_cancelled());
    }

    #[test]
    fn rebalance_trims_fattest_default_when_paid_starving() {
        let router = test_router();
        let paid = test_order(
            0,
            Some(hashdays(100.0)),
            OrderStatus::Active,
            &router.metatron,
        );
        let default_a = test_order(1, None, OrderStatus::Active, &router.metatron);
        let default_b = test_order(2, None, OrderStatus::Active, &router.metatron);

        record_hashrate(router.as_ref(), 1, "deadbeef", "foo", 200.0);
        record_hashrate(router.as_ref(), 2, "cafebabe", "bar", 100.0);

        let token_a = default_a.register_session();
        let token_b = default_b.register_session();

        add_orders(router.as_ref(), [paid, default_a, default_b]);

        router.rebalance();

        assert!(token_a.is_cancelled());
        assert!(!token_b.is_cancelled());
    }

    #[test]
    fn trim_sessions_cancels_oldest_tokens() {
        let metatron = Arc::new(Metatron::new());
        let order = test_order(0, None, OrderStatus::Pending, &metatron);
        let token_a = order.register_session();
        let token_b = order.register_session();
        let token_c = order.register_session();

        order.trim_sessions(2);
        assert!(token_a.is_cancelled());
        assert!(token_b.is_cancelled());
        assert!(!token_c.is_cancelled());

        order.trim_sessions(usize::MAX);
        assert!(token_c.is_cancelled());
        assert!(!order.cancel.is_cancelled());

        let token = order.register_session();
        order.trim_sessions(100);
        assert!(token.is_cancelled());
    }

    #[test]
    fn register_session_prunes_cancelled_tokens() {
        let metatron = Arc::new(Metatron::new());
        let order = test_order(0, None, OrderStatus::Pending, &metatron);
        let token_a = order.register_session();
        let token_b = order.register_session();

        token_a.cancel();
        let token_c = order.register_session();

        order.trim_sessions(1);

        assert!(token_a.is_cancelled());
        assert!(token_b.is_cancelled());
        assert!(!token_c.is_cancelled());

        order.cancel.cancel();
        assert!(token_c.is_cancelled());
    }

    #[test]
    fn ready_for_activation() {
        #[track_caller]
        fn case<F>(received: Amount, elapsed: F, expected: bool, status: OrderStatus)
        where
            F: FnOnce(&Order) -> Duration,
        {
            let metatron = Arc::new(Metatron::new());
            let order = test_order(0, None, OrderStatus::Pending, &metatron);
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
            let order = test_order(0, None, OrderStatus::Pending, &metatron);
            order.set_status(OrderStatus::Cancelled);

            assert!(!order.transition(OrderStatus::Pending, to));
            assert_eq!(order.status(), OrderStatus::Cancelled);
        }

        case(OrderStatus::Active);
        case(OrderStatus::Disconnected);
    }
}
