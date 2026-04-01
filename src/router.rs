use {super::*, order::Order, orders::Orders};

pub(crate) mod order;
pub(crate) mod orders;

pub use order::OrderStatus;

pub(crate) struct Router {
    metatron: Arc<Metatron>,
    orders: RwLock<Orders>,
    round_robin: AtomicU64,
    next_id: AtomicU32,
    settings: Arc<Settings>,
    tasks: TaskTracker,
    cancel: CancellationToken,
}

impl Router {
    pub(crate) fn new(
        metatron: Arc<Metatron>,
        settings: Arc<Settings>,
        tasks: TaskTracker,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            metatron,
            orders: RwLock::new(Orders::new()),
            round_robin: AtomicU64::new(0),
            next_id: AtomicU32::new(0),
            settings,
            tasks,
            cancel,
        }
    }

    pub(crate) async fn add_order(self: &Arc<Self>, request: api::OrderRequest) -> Result<u32> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);

        let order = Order::connect(
            id,
            &request.target,
            request.target_work,
            self.settings.timeout(),
            self.settings.enonce1_extension_size(),
            self.cancel.child_token(),
            &self.tasks,
        )
        .await?;

        self.orders.write().add(order.clone());
        self.spawn_order_monitor(order);

        Ok(id)
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
        self.orders.read().match_round_robin(counter)
    }

    fn spawn_order_monitor(self: &Arc<Self>, order: Arc<Order>) {
        let router = self.clone();
        let check_interval = self.settings.tick_interval();
        self.tasks.spawn(async move {
            tokio::select! {
                biased;
                _ = order.cancel.cancelled() => {}
                _ = order.upstream.disconnected() => {
                    warn!(
                        "Upstream {} disconnected, order {} marked disconnected",
                        order.upstream.endpoint(),
                        order.id,
                    );
                    router.terminate_order(&order, OrderStatus::Disconnected);
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
                    router.terminate_order(&order, OrderStatus::Fulfilled);
                }
            }
        });
    }

    pub(crate) fn metatron(&self) -> Arc<Metatron> {
        self.metatron.clone()
    }

    pub(crate) fn upstream_sessions(&self, upstream_id: u32) -> Vec<Arc<Session>> {
        self.metatron
            .users()
            .iter()
            .flat_map(|user| user.sessions())
            .filter(|session| session.id().upstream_id() == upstream_id)
            .collect()
    }

    pub(crate) fn upstream_session_count(&self, upstream_id: u32) -> usize {
        self.metatron
            .users()
            .iter()
            .flat_map(|user| user.sessions())
            .filter(|session| session.id().upstream_id() == upstream_id)
            .count()
    }

    pub(crate) fn upstream_idle_count(&self, upstream_id: u32) -> usize {
        let now = Instant::now();
        self.metatron
            .users()
            .iter()
            .flat_map(|user| user.sessions())
            .filter(|session| session.id().upstream_id() == upstream_id && session.is_idle(now))
            .count()
    }

    pub(crate) fn upstream_snapshot(&self, upstream_id: u32) -> Stats {
        let now = Instant::now();
        self.metatron
            .users()
            .iter()
            .flat_map(|user| user.sessions())
            .filter(|session| session.id().upstream_id() == upstream_id)
            .fold(Stats::new(), |mut combined, session| {
                combined.absorb(session.snapshot(), now);
                combined
            })
    }

    pub(crate) fn upstream_user_count(&self, upstream_id: u32) -> usize {
        self.metatron
            .users()
            .iter()
            .filter(|user| {
                user.workers()
                    .any(|worker| worker.upstream_session_count(upstream_id) > 0)
            })
            .count()
    }

    pub(crate) fn upstream_worker_count(&self, upstream_id: u32) -> usize {
        self.metatron
            .users()
            .iter()
            .map(|user| {
                user.workers()
                    .filter(|worker| worker.upstream_session_count(upstream_id) > 0)
                    .count()
            })
            .sum()
    }

    pub(crate) fn upstream_disconnected_count(&self, upstream_id: u32) -> usize {
        self.metatron
            .disconnected()
            .iter()
            .filter(|entry| entry.value().0.id().upstream_id() == upstream_id)
            .count()
    }
}

impl StatusLine for Router {
    fn status_line(&self) -> String {
        let now = Instant::now();
        let all = self.orders();
        let stats = self.metatron.snapshot();
        let connected = all
            .iter()
            .filter(|o| o.is_active() && o.upstream.is_connected())
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
    use {super::*, crate::stratifier::state::Authorization};

    fn test_address() -> Address {
        "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc"
            .parse::<Address<bitcoin::address::NetworkUnchecked>>()
            .unwrap()
            .assume_checked()
    }

    fn test_auth(enonce1: &str, workername: &str) -> Arc<Authorization> {
        Arc::new(Authorization {
            enonce1: enonce1.parse().unwrap(),
            address: test_address(),
            workername: workername.into(),
            username: Username::new(format!(
                "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.{workername}"
            )),
            version_mask: None,
        })
    }

    fn test_router() -> Router {
        Router::new(
            Arc::new(Metatron::new()),
            Arc::new(Settings::default()),
            TaskTracker::new(),
            CancellationToken::new(),
        )
    }

    #[test]
    fn upstream_counts_are_isolated() {
        let router = test_router();
        let metatron = router.metatron();

        let s1 = metatron.new_session(test_auth("deadbeef", "foo"), 0);
        let s2 = metatron.new_session(test_auth("cafebabe", "bar"), 1);

        assert_eq!(router.upstream_session_count(0), 1);
        assert_eq!(router.upstream_session_count(1), 1);
        assert_eq!(router.upstream_sessions(0)[0].id(), s1.id());
        assert_eq!(router.upstream_sessions(1)[0].id(), s2.id());
    }

    #[test]
    fn upstream_user_and_worker_counts_are_filtered() {
        let router = test_router();
        let metatron = router.metatron();

        metatron.new_session(test_auth("deadbeef", "foo"), 0);
        metatron.new_session(test_auth("cafebabe", "bar"), 0);
        metatron.new_session(test_auth("facefeed", "foo"), 1);

        assert_eq!(router.upstream_user_count(0), 1);
        assert_eq!(router.upstream_worker_count(0), 2);
        assert_eq!(router.upstream_user_count(1), 1);
        assert_eq!(router.upstream_worker_count(1), 1);
    }

    #[test]
    fn upstream_snapshot_only_includes_requested_upstream() {
        let router = test_router();
        let metatron = router.metatron();

        let s1 = metatron.new_session(test_auth("deadbeef", "foo"), 0);
        let s2 = metatron.new_session(test_auth("cafebabe", "bar"), 1);

        s1.record_accepted(Difficulty::from(100.0), Difficulty::from(200.0));
        s2.record_rejected(Difficulty::from(300.0));

        let upstream_0 = router.upstream_snapshot(0);
        let upstream_1 = router.upstream_snapshot(1);

        assert_eq!(upstream_0.accepted_shares, 1);
        assert_eq!(upstream_0.rejected_shares, 0);
        assert_eq!(upstream_1.accepted_shares, 0);
        assert_eq!(upstream_1.rejected_shares, 1);
    }

    #[test]
    fn upstream_disconnected_count_is_filtered() {
        let router = test_router();
        let metatron = router.metatron();

        let s1 = metatron.new_session(test_auth("deadbeef", "foo"), 0);
        let s2 = metatron.new_session(test_auth("cafebabe", "bar"), 1);

        metatron.retire_session(s1);
        metatron.retire_session(s2);

        assert_eq!(router.upstream_disconnected_count(0), 1);
        assert_eq!(router.upstream_disconnected_count(1), 1);
    }

    #[test]
    fn retire_removes_session_from_upstream_queries() {
        let router = test_router();
        let metatron = router.metatron();

        let session = metatron.new_session(test_auth("deadbeef", "foo"), 0);
        assert_eq!(router.upstream_session_count(0), 1);

        metatron.retire_session(session);

        assert_eq!(router.upstream_session_count(0), 0);
        assert!(router.upstream_sessions(0).is_empty());
    }
}
