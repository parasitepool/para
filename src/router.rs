use {super::*, orders::Orders};

pub(crate) mod order;
pub(crate) mod orders;

pub use order::{Order, OrderStatus};

const RECLAIM_WINDOW: Duration = Duration::from_secs(24 * 3600);

pub(crate) struct Router {
    metatron: Arc<Metatron>,
    orders: RwLock<Orders>,
    round_robin: AtomicU64,
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
            round_robin: AtomicU64::new(0),
            next_id: AtomicU32::new(0),
            settings,
            tasks,
            cancel,
            wallet,
        }
    }

    pub(crate) fn add_order(
        self: &Arc<Self>,
        request: api::OrderRequest,
        default: bool,
    ) -> Arc<Order> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);

        let address_info = self.wallet.reserve_address();

        let order = Order::new(
            id,
            request.target,
            request.target_work,
            self.cancel.child_token(),
            order::Payment {
                address: address_info.address,
                derivation_index: address_info.index,
                amount: request.amount,
                timeout: self.settings.invoice_timeout(),
            },
            default,
        );

        self.orders.write().add(order.clone());
        self.spawn_order_monitor(order.clone());

        order
    }

    fn terminate_order(&self, order: &Order, status: OrderStatus) {
        order.set_status(status);
        order.cancel.cancel();
        self.orders.write().deactivate(order.id);
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

    pub(crate) fn match_with_order(&self) -> Option<Arc<Order>> {
        let counter = self.round_robin.fetch_add(1, Ordering::Relaxed);
        let orders = self.orders.read();

        let all_paid_served = orders.active_paid().iter().all(|order| {
            order
                .upstream()
                .is_some_and(|upstream| self.metatron.upstream_session_count(upstream.id()) > 0)
        });

        if all_paid_served {
            orders.match_any(counter)
        } else {
            orders.match_paid(counter)
        }
    }

    fn spawn_order_monitor(self: &Arc<Self>, order: Arc<Order>) {
        let router = self.clone();
        self.tasks.spawn(async move {
            if !order.default {
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
                    router.orders.write().deactivate(order.id);
                }
                return;
            }

            if !order.mark_active() {
                return;
            }

            router.orders.write().activate(order.id);
            info!("Order {} activated", order.id);

            if !order.default {
                router.rebalance();
            }

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

    fn rebalance(&self) {
        let (defaults, paid) = {
            let orders = self.orders.read();
            (
                orders.active_default().to_vec(),
                orders.active_paid().to_vec(),
            )
        };

        for order in &defaults {
            order.trim_sessions(usize::MAX);
        }

        if paid.len() <= 1 {
            return;
        }

        let counts: Vec<usize> = paid
            .iter()
            .map(|order| {
                order
                    .upstream()
                    .map_or(0, |u| self.metatron.upstream_session_count(u.id()))
            })
            .collect();

        let total: usize = counts.iter().sum();
        let target = (total / paid.len()).max(1);

        for (order, &count) in paid.iter().zip(&counts) {
            if count > 1 && count > target {
                order.trim_sessions(count - target);
            }
        }
    }

    pub(crate) fn metatron(&self) -> Arc<Metatron> {
        self.metatron.clone()
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

    fn test_order_with(
        id: u32,
        target_work: Option<HashDays>,
        status: OrderStatus,
        default: bool,
    ) -> Arc<Order> {
        let order = Order::new(
            id,
            "foo@bar:3333".parse().unwrap(),
            target_work,
            CancellationToken::new(),
            order::Payment {
                address: test_address(),
                derivation_index: 0,
                amount: Amount::from_sat(1000),
                timeout: Duration::from_secs(3600),
            },
            default,
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

    fn test_order(id: u32) -> Arc<Order> {
        test_order_with(id, None, OrderStatus::Pending, false)
    }

    fn test_default_order(id: u32) -> Arc<Order> {
        test_order_with(id, None, OrderStatus::Pending, true)
    }

    #[tokio::test]
    async fn is_fulfilled() {
        let order = test_order_with(0, None, OrderStatus::Active, false);
        assert!(!order.is_fulfilled());

        let order = test_order_with(0, Some(HashDays(1e15)), OrderStatus::Active, false);
        assert!(!order.is_fulfilled());

        let target = HashDays(1e12);

        let order = test_order_with(0, Some(target), OrderStatus::Active, false);
        order
            .upstream()
            .unwrap()
            .set_accepted_work(target.to_total_work());
        assert!(order.is_fulfilled());

        let order = test_order_with(0, Some(target), OrderStatus::Active, false);
        order
            .upstream()
            .unwrap()
            .set_accepted_work(HashDays(2e12).to_total_work());
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
    fn add_does_not_activate() {
        let mut orders = Orders::new();
        let order = test_order(0);

        orders.add(order.clone());

        assert_eq!(orders.all_len(), 1);
        assert_eq!(orders.active_len(), 0);
        assert!(orders.contains(0));
    }

    #[test]
    fn activate_moves_to_active() {
        let mut orders = Orders::new();
        orders.add(test_order(0));

        orders.activate(0);

        assert_eq!(orders.all_len(), 1);
        assert_eq!(orders.active_len(), 1);
        assert_eq!(orders.active_id(0), 0);
    }

    #[test]
    fn deactivate_removes_from_active_but_not_all() {
        let mut orders = Orders::new();
        orders.add(test_order(0));
        orders.add(test_order(1));
        orders.activate(0);
        orders.activate(1);

        orders.deactivate(0);

        assert_eq!(orders.all_len(), 2);
        assert_eq!(orders.active_len(), 1);
        assert_eq!(orders.active_id(0), 1);
    }

    #[test]
    fn get() {
        let mut orders = Orders::new();
        orders.add(test_order(0));

        assert!(orders.get(0).is_some());
        assert!(orders.get(1).is_none());
    }

    #[test]
    fn match_paid() {
        #[track_caller]
        fn case(orders: &Orders, counter: u64, expected: Option<u32>) {
            assert_eq!(orders.match_paid(counter).map(|order| order.id), expected,);
        }

        let mut orders = Orders::new();
        case(&orders, 0, None);

        orders.add(test_default_order(0));
        orders.activate(0);
        case(&orders, 0, Some(0));

        orders.add(test_order(1));
        orders.activate(1);
        case(&orders, 0, Some(1));
        case(&orders, 1, Some(1));

        orders.add(test_order(2));
        orders.activate(2);
        case(&orders, 0, Some(1));
        case(&orders, 1, Some(2));
        case(&orders, 2, Some(1));
    }

    #[test]
    fn match_falls_back_to_default_when_no_paid() {
        #[track_caller]
        fn case(orders: &Orders, counter: u64, expected: Option<u32>) {
            assert_eq!(orders.match_paid(counter).map(|order| order.id), expected,);
        }

        let mut orders = Orders::new();

        orders.add(test_default_order(0));
        orders.add(test_default_order(1));
        orders.activate(0);
        orders.activate(1);

        case(&orders, 0, Some(0));
        case(&orders, 1, Some(1));

        orders.add(test_order(2));
        orders.activate(2);
        case(&orders, 0, Some(2));

        orders.deactivate(2);
        case(&orders, 0, Some(0));
        case(&orders, 1, Some(1));
    }

    #[test]
    fn match_any_includes_default() {
        #[track_caller]
        fn case(orders: &Orders, counter: u64, expected: Option<u32>) {
            assert_eq!(orders.match_any(counter).map(|order| order.id), expected,);
        }

        let mut orders = Orders::new();
        case(&orders, 0, None);

        orders.add(test_default_order(0));
        orders.activate(0);
        case(&orders, 0, Some(0));

        orders.add(test_order(1));
        orders.activate(1);
        case(&orders, 0, Some(1));
        case(&orders, 1, Some(0));

        orders.add(test_order(2));
        orders.activate(2);
        case(&orders, 0, Some(1));
        case(&orders, 1, Some(2));
        case(&orders, 2, Some(0));
        case(&orders, 3, Some(1));
    }

    #[test]
    fn trim_sessions() {
        let order = test_default_order(0);
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
        let order = test_default_order(0);
        let token_a = order.register_session();
        let _token_b = order.register_session();

        token_a.cancel();
        let _token_c = order.register_session();
        assert_eq!(order.session_token_count(), 2);

        let order = test_default_order(0);
        let token = order.register_session();
        order.cancel.cancel();
        assert!(token.is_cancelled());
    }

    #[test]
    fn paid_late_transition() {
        let order = test_order_with(0, None, OrderStatus::Expired, false);
        assert_eq!(order.status(), OrderStatus::Expired);
        order.set_status(OrderStatus::PaidLate);
        assert_eq!(order.status(), OrderStatus::PaidLate);
    }

    #[test]
    fn no_payment_after_timeout_expires() {
        let order = test_order(0);

        assert!(!order.ready_for_activation(Amount::ZERO, order.payment.timeout));
        assert_eq!(order.status(), OrderStatus::Expired);
    }

    #[test]
    fn cancel_pending_order() {
        let order = test_order(0);
        assert_eq!(order.status(), OrderStatus::Pending);
        order.cancel.cancel();
        assert!(order.cancel.is_cancelled());
    }
}
