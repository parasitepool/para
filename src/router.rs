use {super::*, orders::Orders};

pub(crate) mod order;
pub(crate) mod orders;

pub use order::{Order, OrderStatus};

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
