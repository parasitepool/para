use {super::*, orders::Orders};

pub(crate) mod order;
pub(crate) mod orders;

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
        target: UpstreamTarget,
        target_work: Option<HashDays>,
    ) -> Option<Arc<Order>> {
        let amount = match target_work {
            Some(target_work) => {
                if target_work.as_f64() <= 0.0 {
                    return None;
                }
                self.settings
                    .price(target_work)
                    .map(|amount| amount.max(self.wallet.dust_limit()))?
            }
            None => Amount::ZERO,
        };

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);

        let address_info = self.wallet.reserve_address();

        let order = Order::new(
            id,
            target,
            target_work,
            self.cancel.child_token(),
            order::Payment {
                address: address_info.address,
                derivation_index: address_info.index,
                amount,
                timeout: self.settings.invoice_timeout(),
            },
        );

        self.orders.write().add(order.clone());
        self.spawn_order_monitor(order.clone());

        Some(order)
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
                let remaining = order.remaining_work()?;
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
                order.id, order.target, previous, status,
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
                )
                .await
            {
                error!("Failed to connect upstream for order {}: {err}", order.id);
                if order.mark_disconnected_while_pending() {
                    order.cancel.cancel();
                }
                return;
            }

            if !order.mark_active() {
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
                    if self.wallet.confirmed_received(order.payment.derivation_index) > Amount::ZERO {
                        order.set_status(OrderStatus::PaidLate);
                    } else {
                        self.wallet.release_address(order.payment.derivation_index);
                    }
                    return false;
                }
                _ = ticker.tick() => {
                    let elapsed = order.created_at.elapsed();
                    let received = self.wallet.confirmed_received(order.payment.derivation_index);

                    if order.ready_for_activation(received, elapsed) {
                        return true;
                    }

                    match order.status() {
                        OrderStatus::PaidLate => return false,
                        OrderStatus::Expired
                            if elapsed >= order.payment.timeout + RECLAIM_WINDOW =>
                        {
                            self.wallet.release_address(order.payment.derivation_index);
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
                order.target,
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
            .filter(|o| o.is_active() && o.upstream().is_some_and(|u| u.is_connected()))
            .count();

        format!(
            "upstreams={}/{}  sessions={}  hashrate={:.2}",
            connected,
            all.iter().filter(|o| o.is_active()).count(),
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

    fn test_order(id: u32, target_work: Option<HashDays>, status: OrderStatus) -> Arc<Order> {
        let order = Order::new(
            id,
            "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.worker@bar:3333"
                .parse()
                .unwrap(),
            target_work,
            CancellationToken::new(),
            order::Payment {
                address: test_address(),
                derivation_index: 0,
                amount: Amount::from_sat(1000),
                timeout: Duration::from_secs(3600),
            },
        );

        order.set_status(status);

        if status == OrderStatus::Active {
            let _ = order.upstream.set(Upstream::test(id));
            let _ = order.allocator.set(Arc::new(EnonceAllocator::new(
                Extranonces::Pool(PoolExtranonces::new(4, 4).unwrap()),
                id,
            )));
        }

        order
    }

    #[tokio::test]
    async fn is_fulfilled() {
        let order = test_order(0, None, OrderStatus::Active);
        assert!(!order.is_fulfilled());

        let order = test_order(0, Some(HashDays::new(1e15).unwrap()), OrderStatus::Active);
        assert!(!order.is_fulfilled());

        let target = HashDays::new(1e12).unwrap();

        let order = test_order(0, Some(target), OrderStatus::Active);
        order
            .upstream()
            .unwrap()
            .set_accepted_work(target.to_total_work());
        assert!(order.is_fulfilled());

        let order = test_order(0, Some(target), OrderStatus::Active);
        order
            .upstream()
            .unwrap()
            .set_accepted_work(HashDays::new(2e12).unwrap().to_total_work());
        assert!(order.is_fulfilled());
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
    fn active_paid_filters_by_status() {
        let mut orders = Orders::new();
        orders.add(test_order(0, None, OrderStatus::Pending));

        assert_eq!(orders.all_len(), 1);
        assert!(orders.contains(0));
        assert_eq!(orders.active_len(), 0);
        assert!(orders.active_paid().is_empty());

        orders.add(test_order(
            1,
            Some(HashDays::new(100.0).unwrap()),
            OrderStatus::Active,
        ));
        assert_eq!(orders.active_len(), 1);
        assert_eq!(orders.active_id(0), 1);
        assert_eq!(orders.active_paid().len(), 1);
        assert_eq!(orders.active_paid()[0].id, 1);
    }

    #[test]
    fn get() {
        let mut orders = Orders::new();
        orders.add(test_order(0, None, OrderStatus::Pending));

        assert!(orders.get(0).is_some());
        assert!(orders.get(1).is_none());
    }

    #[test]
    fn active_default_excludes_paid() {
        let mut orders = Orders::new();
        orders.add(test_order(0, None, OrderStatus::Active));
        orders.add(test_order(
            1,
            Some(HashDays::new(100.0).unwrap()),
            OrderStatus::Active,
        ));

        assert_eq!(orders.active_default().len(), 1);
        assert_eq!(orders.active_default()[0].id, 0);
    }

    #[test]
    fn remaining_work() {
        #[track_caller]
        fn case(order: &Order, expected: Option<f64>) {
            assert_eq!(order.remaining_work().map(HashDays::as_f64), expected,);
        }

        case(&test_order(0, None, OrderStatus::Active), None);

        let order = test_order(0, Some(HashDays::new(100.0).unwrap()), OrderStatus::Active);
        case(&order, Some(100.0));

        let order = test_order(0, Some(HashDays::new(100.0).unwrap()), OrderStatus::Active);
        order
            .upstream()
            .unwrap()
            .set_accepted_work(HashDays::new(40.0).unwrap().to_total_work());
        case(&order, Some(60.0));

        let order = test_order(0, Some(HashDays::new(100.0).unwrap()), OrderStatus::Active);
        order
            .upstream()
            .unwrap()
            .set_accepted_work(HashDays::new(100.0).unwrap().to_total_work());
        case(&order, None);
    }

    #[test]
    fn next_order_none_when_empty() {
        let router = test_router();
        assert!(router.next_order().is_none());
    }

    #[test]
    fn next_order_falls_back_to_default_when_paid_fulfilled() {
        let router = test_router();
        let target = HashDays::new(100.0).unwrap();
        let paid = test_order(0, Some(target), OrderStatus::Active);
        paid.upstream()
            .unwrap()
            .set_accepted_work(target.to_total_work());
        let default = test_order(1, None, OrderStatus::Active);

        let mut orders = router.orders.write();
        orders.add(paid);
        orders.add(default);
        drop(orders);

        assert_eq!(router.next_order().unwrap().id, 1);
    }

    #[test]
    fn match_with_order_water_fills_paid_orders() {
        let router = test_router();
        let order_a = test_order(0, Some(HashDays::new(100.0).unwrap()), OrderStatus::Active);
        let order_b = test_order(1, Some(HashDays::new(100.0).unwrap()), OrderStatus::Active);

        order_a
            .upstream()
            .unwrap()
            .set_accepted_work(HashDays::new(20.0).unwrap().to_total_work());
        order_b
            .upstream()
            .unwrap()
            .set_accepted_work(HashDays::new(60.0).unwrap().to_total_work());

        let session_a = router
            .metatron
            .new_session(test_authorization("deadbeef", "foo"), 0);
        let session_b = router
            .metatron
            .new_session(test_authorization("cafebabe", "bar"), 1);

        session_a.record_accepted(Difficulty::from(100.0), Difficulty::from(100.0));
        session_b.record_accepted(Difficulty::from(100.0), Difficulty::from(100.0));

        let mut orders = router.orders.write();
        orders.add(order_a);
        orders.add(order_b);
        drop(orders);

        assert_eq!(router.next_order().unwrap().id, 0);
    }

    #[test]
    fn match_with_order_picks_lowest_hashrate_default() {
        let router = test_router();
        let order_a = test_order(0, None, OrderStatus::Active);
        let order_b = test_order(1, None, OrderStatus::Active);

        let session_a = router
            .metatron
            .new_session(test_authorization("deadbeef", "foo"), 0);

        session_a.record_accepted(Difficulty::from(100.0), Difficulty::from(100.0));

        let mut orders = router.orders.write();
        orders.add(order_a);
        orders.add(order_b);
        drop(orders);

        assert_eq!(router.next_order().unwrap().id, 1);
    }

    #[test]
    fn rebalance_noop_without_starving_paid() {
        let router = test_router();
        let default = test_order(0, None, OrderStatus::Active);
        let token = default.register_session();

        router.orders.write().add(default);
        router.rebalance();

        assert!(!token.is_cancelled());
    }

    #[test]
    fn rebalance_trims_fattest_default_when_paid_starving() {
        let router = test_router();
        let paid = test_order(0, Some(HashDays::new(100.0).unwrap()), OrderStatus::Active);
        let default_a = test_order(1, None, OrderStatus::Active);
        let default_b = test_order(2, None, OrderStatus::Active);

        let session_a = router
            .metatron
            .new_session(test_authorization("deadbeef", "foo"), 1);
        session_a.record_accepted(Difficulty::from(200.0), Difficulty::from(200.0));

        let session_b = router
            .metatron
            .new_session(test_authorization("cafebabe", "bar"), 2);
        session_b.record_accepted(Difficulty::from(100.0), Difficulty::from(100.0));

        let token_a = default_a.register_session();
        let token_b = default_b.register_session();

        let mut orders = router.orders.write();
        orders.add(paid);
        orders.add(default_a);
        orders.add(default_b);
        drop(orders);

        router.rebalance();

        assert!(token_a.is_cancelled());
        assert!(!token_b.is_cancelled());
    }

    #[test]
    fn trim_sessions() {
        let order = test_order(0, None, OrderStatus::Pending);
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
    fn session_token_lifecycle() {
        let order = test_order(0, None, OrderStatus::Pending);
        let token_a = order.register_session();
        let _token_b = order.register_session();

        token_a.cancel();
        let _token_c = order.register_session();
        assert_eq!(order.session_token_count(), 2);

        let order = test_order(0, None, OrderStatus::Pending);
        let token = order.register_session();
        order.cancel.cancel();
        assert!(token.is_cancelled());
    }

    #[test]
    fn paid_late_transition() {
        let order = test_order(0, None, OrderStatus::Expired);
        assert_eq!(order.status(), OrderStatus::Expired);
        order.set_status(OrderStatus::PaidLate);
        assert_eq!(order.status(), OrderStatus::PaidLate);
    }

    #[test]
    fn no_payment_after_timeout_expires() {
        let order = test_order(0, None, OrderStatus::Pending);

        assert!(!order.ready_for_activation(Amount::ZERO, order.payment.timeout));
        assert_eq!(order.status(), OrderStatus::Expired);
    }

    #[test]
    fn cancel_pending_order() {
        let order = test_order(0, None, OrderStatus::Pending);
        assert_eq!(order.status(), OrderStatus::Pending);
        order.cancel.cancel();
        assert!(order.cancel.is_cancelled());
    }
}
