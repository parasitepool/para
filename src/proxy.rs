use {
    super::*,
    crate::{
        event_sink::Event,
        router::{
            control::Control, dispatcher::Dispatcher, error::RouterResult, greeter::Prelude,
            order::Order, order_book::OrderBook, runner::OrderRunner,
        },
    },
};

pub(crate) struct Proxy {
    settings: Arc<Settings>,
    metatron: Arc<Metatron>,
    book: Arc<OrderBook>,
    control: Control,
    runner: Arc<OrderRunner>,
    tasks: TaskTracker,
    cancel: CancellationToken,
}

impl Proxy {
    pub(crate) fn new(
        settings: Arc<Settings>,
        metatron: Arc<Metatron>,
        tasks: TaskTracker,
        cancel: CancellationToken,
    ) -> Self {
        let book = Arc::new(OrderBook::new(
            settings.clone(),
            metatron.clone(),
            cancel.child_token(),
        ));

        let control = Control::new(settings.clone(), metatron.clone());

        let runner = Arc::new(OrderRunner::new(
            settings.clone(),
            None,
            tasks.clone(),
            cancel.clone(),
        ));

        Self {
            settings,
            metatron,
            book,
            control,
            runner,
            tasks,
            cancel,
        }
    }

    pub(crate) fn metatron(&self) -> Arc<Metatron> {
        self.metatron.clone()
    }

    pub(crate) fn live_orders(&self) -> Vec<Arc<Order>> {
        self.book.live_orders()
    }

    pub(crate) fn persist(&self) -> Result {
        self.book.persist(None)
    }

    pub(crate) fn add_sink_order(
        self: &Arc<Self>,
        upstream_target: UpstreamTarget,
    ) -> RouterResult<Arc<Order>> {
        let id = self.book.allocate_id()?;

        let order = Order::new(
            id,
            upstream_target,
            None,
            self.cancel.child_token(),
            self.metatron.clone(),
        );

        self.book.add(order.clone());
        self.runner.spawn(order.clone());

        Ok(order)
    }

    pub(crate) fn ensure_sink_order(self: &Arc<Self>, upstream_target: UpstreamTarget) {
        if self.book.live_orders().into_iter().any(|order| {
            order.is_sink()
                && !order.status().is_terminal()
                && order.upstream_target == upstream_target
        }) {
            return;
        }

        if let Err(err) = self.add_sink_order(upstream_target) {
            warn!("Failed to add sink order: {err}");
        }
    }

    pub(crate) fn restore(self: &Arc<Self>, sink_orders: &[UpstreamTarget]) -> Result {
        self.book.restore(sink_orders, &|order| {
            self.book.add(order.clone());
            self.runner.spawn(order);
        })?;

        for upstream_target in sink_orders {
            self.ensure_sink_order(upstream_target.clone());
        }

        self.persist()?;
        self.book.retire_orders();

        Ok(())
    }

    pub(crate) async fn serve(
        self: &Arc<Self>,
        listener: TcpListener,
        event_tx: Option<mpsc::Sender<Event>>,
        cancel_token: CancellationToken,
    ) -> Result {
        let proxy = self.clone();
        self.tasks.spawn(async move {
            let mut ticker = ticker(proxy.settings.tick_interval());
            loop {
                tokio::select! {
                    biased;
                    _ = proxy.cancel.cancelled() => break,
                    _ = ticker.tick() => {
                        if let Err(err) = proxy.persist() {
                            warn!("Proxy persistence error: {err}");
                        }

                        proxy.book.retire_orders();
                    }
                }
            }
        });

        let selector = {
            let proxy = self.clone();
            move |addr: SocketAddr, prelude: &Prelude| {
                proxy
                    .control
                    .next_order(&proxy.book.routable(), addr, prelude)
            }
        };

        let on_shutdown = {
            let proxy = self.clone();
            move || proxy.persist()
        };

        Arc::new(Dispatcher::new(
            self.settings.clone(),
            self.metatron.clone(),
            self.tasks.clone(),
        ))
        .serve(listener, event_tx, selector, on_shutdown, cancel_token)
        .await
    }
}

impl StatusLine for Proxy {
    fn status_line(&self) -> String {
        let now = Instant::now();
        let stats = self.metatron.snapshot();

        format!(
            "orders={}  sessions={}  hashrate={:.2}  blocks={}",
            self.book.active().len(),
            self.metatron.total_sessions(),
            stats.hashrate_1m(now),
            self.metatron.block_count(),
        )
    }
}
