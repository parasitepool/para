use {
    super::*,
    crate::{
        api::{
            DownstreamStats, PlacementCounts, RouterStatus, RoutingInfo, UpstreamStats,
            UpstreamTotals, WalletInfo,
        },
        event_sink::Event,
    },
    cashier::{Cashier, Refund},
    control::Control,
    dispatcher::Dispatcher,
    error::{RouterError, RouterResult},
    greeter::{Prelude, greet},
    order::{Bucket, Order, OrderStatus, Payment},
    order_book::OrderBook,
    price_feed::PriceFeed,
    runner::OrderRunner,
};

pub(crate) mod cashier;
pub(crate) mod control;
pub(crate) mod dispatcher;
pub(crate) mod error;
pub(crate) mod greeter;
mod intents;
pub mod order;
pub(crate) mod order_book;
pub(crate) mod price_feed;
pub(crate) mod runner;

#[cfg(test)]
pub(crate) mod testkit;

pub(crate) const PAYMENT_TIMEOUT: u32 = 6;
pub(crate) const EXTENDED_PAYMENT_TIMEOUT: u32 = 144;
const MAX_ORDER_CREATIONS_PER_MINUTE: usize = 100;
const SWEEP_INTERVAL: Duration = Duration::from_hours(1);

struct RateWindow {
    start: Instant,
    count: usize,
}

pub(crate) struct Router {
    settings: Arc<Settings>,
    metatron: Arc<Metatron>,
    wallet: Option<Arc<Wallet>>,
    book: Arc<OrderBook>,
    cashier: Option<Arc<Cashier>>,
    price_feed: PriceFeed,
    creation_window: Mutex<RateWindow>,
    halt: AtomicBool,
    boost: AtomicBool,
    capacity_work: AtomicU64,
    premium_percent: AtomicU64,
    control: Control,
    runner: Arc<OrderRunner>,
    tasks: TaskTracker,
    cancel: CancellationToken,
}

impl Router {
    pub(crate) fn new(
        settings: Arc<Settings>,
        metatron: Arc<Metatron>,
        wallet: Option<Arc<Wallet>>,
        tasks: TaskTracker,
        cancel: CancellationToken,
        initial_hash_value: HashValue,
    ) -> Self {
        let halt = settings.halt();
        let boost = settings.boost();
        let capacity_work = settings.capacity_work();
        let premium_percent = settings.premium_percent();

        let control = Control::new(settings.clone(), metatron.clone());

        let book = Arc::new(OrderBook::new(
            settings.clone(),
            metatron.clone(),
            cancel.child_token(),
        ));

        let cashier = wallet
            .clone()
            .map(|wallet| Arc::new(Cashier::new(wallet, settings.clone())));

        let runner = Arc::new(OrderRunner::new(
            settings.clone(),
            cashier.clone(),
            tasks.clone(),
            cancel.clone(),
        ));

        Self {
            settings,
            metatron,
            wallet,
            book,
            cashier,
            price_feed: PriceFeed::new(initial_hash_value),
            creation_window: Mutex::new(RateWindow {
                start: Instant::now(),
                count: 0,
            }),
            halt: AtomicBool::new(halt),
            boost: AtomicBool::new(boost),
            capacity_work: AtomicU64::new(capacity_work.as_f64().to_bits()),
            premium_percent: AtomicU64::new(premium_percent.to_bits()),
            control,
            runner,
            tasks,
            cancel,
        }
    }

    pub(crate) fn hash_value(&self) -> HashValue {
        self.price_feed.hash_value()
    }

    #[cfg(test)]
    pub(crate) fn set_hash_value(&self, hash_value: HashValue) {
        self.price_feed.set_hash_value(hash_value);
    }

    pub(crate) fn hash_price(&self) -> HashPrice {
        self.price_feed.hash_price(self.premium_percent())
    }

    pub(crate) fn difficulty_multiplier(&self) -> f64 {
        self.price_feed.difficulty_multiplier()
    }

    #[cfg(test)]
    pub(crate) fn set_difficulty_multiplier(&self, multiplier: f64) {
        self.price_feed.set_difficulty_multiplier(multiplier);
    }

    pub(crate) fn wallet(&self) -> Option<&Wallet> {
        self.wallet.as_deref()
    }

    pub(crate) fn order_snapshots(
        &self,
        cold_filter: impl Fn(u32, &entry::OrderEntry) -> bool,
    ) -> (Vec<Arc<Order>>, Vec<(u32, entry::OrderEntry)>) {
        self.book.order_snapshots(cold_filter)
    }

    #[cfg(test)]
    pub(crate) fn live_orders(&self) -> Vec<Arc<Order>> {
        self.book.live_orders()
    }

    pub(crate) fn persist(&self) -> Result {
        self.book.persist(self.wallet.as_deref())
    }

    pub(crate) fn retire_orders(&self) {
        self.book.retire_orders();
    }

    pub(crate) fn flag_orders(&self) {
        if let Some(wallet) = &self.wallet {
            self.book.flag_orders(wallet);
        }
    }

    #[cfg(test)]
    pub(crate) fn audit_receipts(&self) -> Vec<(u32, Amount)> {
        match &self.wallet {
            Some(wallet) => self.book.audit_receipts(wallet),
            None => Vec::new(),
        }
    }

    pub(crate) fn sweep_orphan_receipts(&self) {
        if let Some(wallet) = &self.wallet {
            self.book.sweep_orphan_receipts(wallet);
        }
    }

    fn refund_target(&self, id: u32) -> RouterResult<(u32, Address<NetworkUnchecked>)> {
        if let Some(order) = self.book.get_order(id) {
            let bucket = order
                .bucket
                .as_ref()
                .ok_or(RouterError::NotABucketOrder { id })?;

            return Ok((
                bucket.payment.derivation_index,
                order.upstream_target.username().address().clone(),
            ));
        }

        let entry = self
            .book
            .cold_order(id)
            .ok_or(RouterError::OrderNotFound { id })?;

        let bucket = entry.bucket.ok_or(RouterError::NotABucketOrder { id })?;

        Ok((
            bucket.derivation_index,
            entry.upstream_target.username().address().clone(),
        ))
    }

    pub(crate) fn build_refund(
        &self,
        id: u32,
        fee_rate: Option<FeeRate>,
        destination: Option<Address>,
    ) -> RouterResult<Refund> {
        let (derivation_index, default_destination) = self.refund_target(id)?;

        self.cashier
            .as_ref()
            .ok_or(RouterError::WalletRequired)?
            .build_refund(
                id,
                derivation_index,
                &default_destination,
                fee_rate,
                destination,
            )
    }

    #[cfg(test)]
    async fn wait_for_payment(&self, order: &Arc<Order>, payment: &Payment) -> RouterResult<bool> {
        self.cashier
            .as_ref()
            .ok_or(RouterError::WalletRequired)?
            .wait_for_payment(order, payment)
            .await
    }

    fn rebalance(&self) {
        self.control.rebalance(&self.book.active(), self.boost());
    }

    #[cfg(test)]
    pub(crate) fn allocate_id(&self) -> RouterResult<u32> {
        self.book.allocate_id()
    }

    #[cfg(test)]
    pub(crate) async fn execute_order(self: &Arc<Self>, order: &Arc<Order>) -> RouterResult<()> {
        self.runner.execute(order).await
    }

    pub(crate) fn halt(&self) -> bool {
        self.halt.load(Ordering::Relaxed)
    }

    pub(crate) fn set_halt(&self, enabled: bool) {
        self.halt.store(enabled, Ordering::Relaxed);
    }

    pub(crate) fn boost(&self) -> bool {
        self.boost.load(Ordering::Relaxed)
    }

    pub(crate) fn set_boost(&self, enabled: bool) {
        self.boost.store(enabled, Ordering::Relaxed);
    }

    pub(crate) fn capacity_work(&self) -> HashDays {
        HashDays::from_raw(f64::from_bits(self.capacity_work.load(Ordering::Relaxed)))
    }

    pub(crate) fn set_capacity_work(&self, capacity: HashDays) {
        self.capacity_work
            .store(capacity.as_f64().to_bits(), Ordering::Relaxed);
    }

    pub(crate) fn premium_percent(&self) -> f64 {
        f64::from_bits(self.premium_percent.load(Ordering::Relaxed))
    }

    pub(crate) fn set_premium_percent(&self, premium_percent: f64) {
        self.premium_percent
            .store(premium_percent.to_bits(), Ordering::Relaxed);
    }

    pub(crate) fn cancel_order(&self, id: u32) -> Option<Arc<Order>> {
        let order = self.book.get_order(id)?;
        order.terminate(OrderStatus::Cancelled);
        Some(order)
    }

    pub(crate) fn clear_order(&self, id: u32) -> Option<Arc<Order>> {
        let order = self.book.get_order(id)?;

        if !order.set_cleared() {
            return None;
        }

        Some(order)
    }

    pub(crate) fn get_order(&self, id: u32) -> Option<Arc<Order>> {
        self.book.get_order(id)
    }

    pub(crate) fn cold_order(&self, id: u32) -> Option<entry::OrderEntry> {
        self.book.cold_order(id)
    }

    pub(crate) fn metatron(&self) -> Arc<Metatron> {
        self.metatron.clone()
    }

    pub(crate) fn next_order(&self, addr: SocketAddr, prelude: &Prelude) -> Option<Arc<Order>> {
        self.control
            .next_order(&self.book.routable(), addr, prelude)
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

    pub(crate) fn add_bucket_order(
        self: &Arc<Self>,
        upstream_target: UpstreamTarget,
        target: HashDays,
        price: HashPrice,
    ) -> RouterResult<Arc<Order>> {
        {
            let mut window = self.creation_window.lock();

            if window.start.elapsed() >= Duration::from_secs(60) {
                *window = RateWindow {
                    start: Instant::now(),
                    count: 0,
                };
            }

            if window.count >= MAX_ORDER_CREATIONS_PER_MINUTE {
                return Err(RouterError::OrderRateLimited {
                    limit: MAX_ORDER_CREATIONS_PER_MINUTE,
                });
            }

            window.count += 1;
        }

        if self.halt() {
            return Err(RouterError::Halted);
        }

        let wallet = self.wallet.as_ref().ok_or(RouterError::WalletRequired)?;

        if !wallet.is_synced() {
            return Err(RouterError::WalletSyncing);
        }

        if target.as_f64() <= 0.0 {
            return Err(RouterError::InvalidHashdays);
        }

        let minimum = self.hash_price().tolerance();

        if price < minimum {
            return Err(RouterError::HashPriceBelowMinimum {
                bid: price,
                minimum,
            });
        }

        let amount = price.total(target).ok_or(RouterError::HashPriceOverflow)?;

        if amount < wallet.dust_limit() {
            return Err(RouterError::BelowDustLimit {
                amount,
                dust_limit: wallet.dust_limit(),
            });
        }

        let cancel = self.cancel.child_token();
        let metatron = self.metatron.clone();
        let capacity = self.capacity_work();

        let order = self.book.add_bucket_order(capacity, target, |id| {
            wallet
                .reveal_address_with(|address_info, created_at_height, wallet_delta| {
                    let bucket = Bucket {
                        target,
                        payment: Payment::new(
                            address_info.address,
                            address_info.index,
                            amount,
                            created_at_height,
                        ),
                    };
                    let order =
                        Order::new(id, upstream_target, Some(bucket), cancel, metatron.clone());

                    metatron.persist_order(id, &order.to_entry(), wallet_delta)?;

                    Ok(order)
                })
                .map_err(|error| RouterError::WalletPersistence { error })
        })?;

        self.runner.spawn(order.clone());

        Ok(order)
    }

    pub(crate) fn restore(self: &Arc<Self>, sink_orders: &[UpstreamTarget]) -> Result {
        self.book.restore(sink_orders, &|order| {
            self.book.add(order.clone());
            self.runner.spawn(order);
        })?;

        for upstream_target in sink_orders {
            self.ensure_sink_order(upstream_target.clone());
        }

        Ok(())
    }

    pub(crate) fn status(&self) -> RouterStatus {
        let now = Instant::now();
        let metatron = &self.metatron;
        let snapshot = self.book.status_snapshot();
        let used = snapshot.used;
        let orders = snapshot.live;

        let mut bucket_order_count = 0;
        let mut sink_order_count = 0;
        let mut starving_order_count = 0;
        let mut deficit_hashrate = HashRate::ZERO;
        let mut pending = 0;
        let mut disconnected = 0;

        let mut active_addresses: HashSet<&Address<NetworkUnchecked>> = HashSet::new();
        let mut active_workers: HashSet<&str> = HashSet::new();

        let mut active = Stats::new();
        let mut live = Stats::new();

        for order in &orders {
            let username = order.upstream_target.username();
            let stats = order.stats();

            match order.status() {
                OrderStatus::Active => {
                    if order.is_sink() {
                        sink_order_count += 1;
                    } else {
                        bucket_order_count += 1;
                        let measured = order.hashrate_1m(now);

                        if !order.is_fulfilled() {
                            if order.is_starving(measured) {
                                starving_order_count += 1;
                            }
                            deficit_hashrate += order.hashrate_shortfall(measured);
                        }

                        active_addresses.insert(username.address());
                        active_workers.insert(username.as_str());
                        active.absorb(stats.clone(), now);
                    }
                }
                OrderStatus::Pending | OrderStatus::InMempool => pending += 1,
                OrderStatus::Disconnected => disconnected += 1,
                _ => {}
            }

            live.absorb(stats, now);
        }

        let cold = snapshot.cold;

        let total_users = snapshot.total_users;
        let total_orders = snapshot.cold_count + orders.len();

        let total_best_share = if cold
            .best_share
            .is_some_and(|best| live.best_share.is_none_or(|current| best > current))
        {
            cold.best_share
        } else {
            live.best_share
        };

        let live_last_share_secs = live
            .last_share
            .map(|time| epoch::instant_to_epoch_secs(time, now));

        let total_last_share_secs = if cold
            .last_share_secs
            .is_some_and(|last| live_last_share_secs.is_none_or(|current| last > current))
        {
            cold.last_share_secs
        } else {
            live_last_share_secs
        };

        let accepted_work = cold.accepted_work + live.accepted_work;
        let rejected_work = cold.rejected_work + live.rejected_work;
        let delivered_work = accepted_work + rejected_work;

        let totals = UpstreamTotals {
            users: total_users,
            orders: total_orders,
            accepted_shares: cold.accepted_shares + live.accepted_shares,
            rejected_shares: cold.rejected_shares + live.rejected_shares,
            accepted_work,
            rejected_work,
            delivered_hash_days: delivered_work.to_hash_days(),
            best_share: total_best_share,
            last_share: total_last_share_secs.map(|secs| secs as u64),
        };

        let total_capacity_hash_days = self.capacity_work();

        let used_capacity_hash_days =
            HashDays::from_raw(used.as_f64().min(total_capacity_hash_days.as_f64()));

        let control_metrics = self.control.metrics(now);

        RouterStatus {
            uptime_secs: metatron.uptime().as_secs(),
            block_count: metatron.block_count() as u64,
            recent_blocks: metatron.recent_blocks(10),
            hash_price: self.hash_price(),
            hash_value: self.hash_value(),
            difficulty_multiplier: self.difficulty_multiplier(),
            total_capacity_hash_days,
            used_capacity_hash_days,
            halt: self.halt(),
            boost: self.boost(),
            wallet: WalletInfo {
                synced: self
                    .wallet
                    .as_ref()
                    .is_some_and(|wallet| wallet.is_synced()),
            },
            routing: RoutingInfo {
                sessions_trimmed_1h: control_metrics.sessions_trimmed_1h,
                intents_created_1h: control_metrics.intents_created_1h,
                intents_expired_1h: control_metrics.intents_expired_1h,
                intent_claimed_1h: control_metrics.intent_claimed_1h,
                placements_1h: control_metrics.placements_1h,
                deficit_hashrate,
                bucket_order_count,
                sink_order_count,
                starving_order_count,
            },
            upstream: UpstreamStats {
                users: active_addresses.len(),
                workers: active_workers.len(),
                orders: bucket_order_count,
                pending,
                disconnected,
                hashrate_1m: active.hashrate_1m(now),
                hashrate_5m: active.hashrate_5m(now),
                hashrate_15m: active.hashrate_15m(now),
                hashrate_1hr: active.hashrate_1hr(now),
                hashrate_6hr: active.hashrate_6hr(now),
                hashrate_1d: active.hashrate_1d(now),
                hashrate_7d: active.hashrate_7d(now),
                sps_1m: active.sps_1m(now),
                sps_5m: active.sps_5m(now),
                sps_15m: active.sps_15m(now),
                sps_1hr: active.sps_1hr(now),
                accepted_shares: active.accepted_shares,
                rejected_shares: active.rejected_shares,
                accepted_work: active.accepted_work,
                rejected_work: active.rejected_work,
                best_share: active.best_share,
                last_share: active.last_share_epoch_secs(now),
                totals,
            },
            downstream: DownstreamStats::from_metatron(metatron, now),
            git_commit: env!("GIT_COMMIT").into(),
        }
    }

    pub(crate) async fn serve(
        self: &Arc<Self>,
        listener: TcpListener,
        event_tx: Option<mpsc::Sender<Event>>,
        bitcoin_client: Option<Arc<BitcoindClient>>,
        cancel_token: CancellationToken,
    ) -> Result {
        let router = self.clone();

        self.tasks.spawn(async move {
            let mut ticker = ticker(router.settings.tick_interval());
            loop {
                tokio::select! {
                    biased;
                    _ = router.cancel.cancelled() => break,
                    _ = ticker.tick() => {
                        router.rebalance();
                        router.flag_orders();

                        if let Some(bitcoin_client) = &bitcoin_client {
                            router.price_feed.update(bitcoin_client, &router.settings).await;
                        }

                        if let Err(err) = router.persist() {
                            warn!("Router persistence error: {err}");
                        }

                        router.retire_orders();
                    }
                }
            }
        });

        let router = self.clone();

        self.tasks.spawn(async move {
            let mut ticker = ticker(SWEEP_INTERVAL);
            loop {
                tokio::select! {
                    biased;
                    _ = router.cancel.cancelled() => break,
                    _ = ticker.tick() => {
                        router.sweep_orphan_receipts();
                    }
                }
            }
        });

        let selector = {
            let router = self.clone();
            move |addr: SocketAddr, prelude: &Prelude| router.next_order(addr, prelude)
        };

        let on_shutdown = {
            let router = self.clone();
            move || router.persist()
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

impl StatusLine for Router {
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

#[cfg(test)]
mod tests {
    use {super::*, crate::router::testkit::*};

    #[test]
    fn status_serde() {
        #[track_caller]
        fn case(status: OrderStatus, expected: &str) {
            assert_eq!(serde_json::to_string(&status).unwrap(), expected);
            assert_eq!(
                serde_json::from_str::<OrderStatus>(expected).unwrap(),
                status,
            );
        }

        case(OrderStatus::Pending, "\"pending\"");
        case(OrderStatus::InMempool, "\"in_mempool\"");
        case(OrderStatus::Active, "\"active\"");
        case(OrderStatus::Fulfilled, "\"fulfilled\"");
        case(OrderStatus::Cancelled, "\"cancelled\"");
        case(OrderStatus::Disconnected, "\"disconnected\"");
        case(OrderStatus::Expired, "\"expired\"");
    }

    #[test]
    fn status_separates_now_and_totals() {
        let router = test_router();
        let order = test_order(
            0,
            Some(hash_days(100.0)),
            OrderStatus::Active,
            &router.metatron,
        );
        add_orders(router.as_ref(), [order.clone()]);

        router.metatron.record_order_accepted(
            order.id,
            Difficulty::from(1.0),
            Difficulty::from(1.0),
        );

        let session = router
            .metatron
            .new_session(test_authorization("deadbeef", "foo"), order.id);
        session.record_accepted(Difficulty::from(1.0), Difficulty::from(1.0));

        let status = router.status();
        assert_eq!(status.upstream.accepted_shares, 1);
        assert_eq!(status.upstream.totals.accepted_shares, 1);
        assert_eq!(status.upstream.totals.users, 1);
        assert_eq!(status.upstream.totals.orders, 1);
        assert_eq!(status.downstream.accepted_shares, 1);
        assert_eq!(status.downstream.totals.accepted_shares, 1);

        order.terminate(OrderStatus::Fulfilled);
        order.clear_dirty();

        let status = router.status();
        assert_eq!(status.upstream.accepted_shares, 0);
        assert_eq!(status.upstream.users, 0);
        assert_eq!(status.upstream.totals.accepted_shares, 1);
        assert_eq!(status.upstream.totals.users, 1);

        router.retire_orders();

        let status = router.status();
        assert_eq!(status.upstream.accepted_shares, 0);
        assert_eq!(status.upstream.totals.accepted_shares, 1);
        assert_eq!(status.upstream.totals.users, 1);
        assert_eq!(status.upstream.totals.orders, 1);
        assert_eq!(status.downstream.accepted_shares, 1);
        assert_eq!(status.downstream.totals.accepted_shares, 1);

        let json = serde_json::to_value(router.status()).unwrap();
        assert!(json["upstream"]["hashrate_1m"].is_number());
        assert!(json["upstream"]["totals"]["orders"].is_number());
        assert!(json["downstream"]["sessions"].is_number());
        assert!(json["downstream"]["totals"]["users"].is_number());
        assert!(json.get("traffic").is_none());
        assert!(json.get("history").is_none());
    }

    #[test]
    fn status_excludes_sinks_from_traffic_plane() {
        let router = test_router();
        let sink = test_order(0, None, OrderStatus::Active, &router.metatron);
        add_orders(router.as_ref(), [sink.clone()]);

        router.metatron.record_order_accepted(
            sink.id,
            Difficulty::from(1.0),
            Difficulty::from(1.0),
        );

        let status = router.status();
        assert_eq!(status.routing.sink_order_count, 1);
        assert_eq!(status.upstream.orders, 0);
        assert_eq!(status.upstream.users, 0);
        assert_eq!(status.upstream.workers, 0);
        assert_eq!(status.upstream.accepted_shares, 0);
        assert_eq!(status.upstream.hashrate_1m, HashRate::ZERO);
        assert_eq!(status.upstream.totals.accepted_shares, 1);
        assert_eq!(status.upstream.totals.orders, 1);
        assert_eq!(status.upstream.totals.users, 1);
    }

    #[test]
    fn status_traffic_best_share_excludes_inactive_orders() {
        let router = test_router();
        let inactive = test_order(
            0,
            Some(hash_days(100.0)),
            OrderStatus::Fulfilled,
            &router.metatron,
        );
        let active = test_order(
            1,
            Some(hash_days(100.0)),
            OrderStatus::Active,
            &router.metatron,
        );
        add_orders(router.as_ref(), [inactive.clone(), active.clone()]);

        router.metatron.record_order_accepted(
            inactive.id,
            Difficulty::from(1.0),
            Difficulty::from(400.97e12),
        );
        router.metatron.record_order_accepted(
            active.id,
            Difficulty::from(1.0),
            Difficulty::from(100.0e12),
        );

        let status = router.status();

        assert_eq!(status.upstream.best_share, Some(Difficulty::from(100.0e12)));
        assert_eq!(
            status.upstream.totals.best_share,
            Some(Difficulty::from(400.97e12)),
        );
    }

    #[test]
    fn finish_order_changes_non_terminal_status_and_cancels() {
        let router = test_router();
        let order = test_order(0, None, OrderStatus::Active, &router.metatron);

        order.terminate(OrderStatus::Fulfilled);

        assert_eq!(order.status(), OrderStatus::Fulfilled);
        assert!(order.cancel.is_cancelled());
    }

    #[test]
    fn finish_order_is_noop_for_terminal_status() {
        let router = test_router();
        let order = test_order(0, None, OrderStatus::Fulfilled, &router.metatron);

        order.terminate(OrderStatus::Cancelled);

        assert_eq!(order.status(), OrderStatus::Fulfilled);
        assert!(!order.cancel.is_cancelled());
    }

    #[test]
    fn finish_order_does_not_change_terminal_statuses() {
        let router = test_router();
        let terminal = [
            OrderStatus::Fulfilled,
            OrderStatus::Cancelled,
            OrderStatus::Disconnected,
            OrderStatus::Expired,
        ];

        for from in terminal {
            for to in terminal {
                let order = test_order(0, None, from, &router.metatron);

                order.terminate(to);

                assert_eq!(order.status(), from, "{from:?} should ignore {to:?}");
                assert!(
                    !order.cancel.is_cancelled(),
                    "{from:?} should not cancel again for {to:?}",
                );
            }
        }
    }

    #[test]
    fn finish_order_ignores_non_terminal_status() {
        let router = test_router();
        let order = test_order(0, None, OrderStatus::Active, &router.metatron);

        order.terminate(OrderStatus::Pending);

        assert_eq!(order.status(), OrderStatus::Active);
        assert!(!order.cancel.is_cancelled());
    }

    #[test]
    fn expired_order_is_terminal() {
        let router = test_router();
        let order = test_order(0, None, OrderStatus::Pending, &router.metatron);

        order.terminate(OrderStatus::Expired);
        order.terminate(OrderStatus::Cancelled);

        assert_eq!(order.status(), OrderStatus::Expired);
        assert!(order.cancel.is_cancelled());
    }

    #[test]
    fn set_flagged_sets_and_logs_once() {
        let router = test_router();
        let order = test_order(0, None, OrderStatus::Expired, &router.metatron);

        assert!(!order.is_flagged());

        assert!(order.set_flagged());
        assert!(order.is_flagged());

        assert!(!order.set_flagged());
        assert!(order.is_flagged());
    }

    #[test]
    fn set_flagged_is_noop_once_cleared() {
        let router = test_router();
        let order = test_order(0, None, OrderStatus::Expired, &router.metatron);

        order.set_flagged();
        assert!(order.set_cleared());

        assert!(!order.set_flagged());
        assert!(!order.is_flagged());
        assert!(order.is_cleared());
    }

    #[test]
    fn terminate_marks_dirty_in_same_critical_section() {
        let router = test_router();
        let order = test_order(0, None, OrderStatus::Active, &router.metatron);

        order.clear_dirty();
        order.terminate(OrderStatus::Cancelled);

        let lifecycle = order.lifecycle();
        assert_eq!(lifecycle.status, OrderStatus::Cancelled);
        assert!(lifecycle.dirty);
    }

    #[test]
    fn set_cleared() {
        let router = test_router();
        let order = test_order(0, None, OrderStatus::Expired, &router.metatron);

        order.set_flagged();

        assert!(order.set_cleared());
        assert!(order.is_cleared());
        assert!(!order.is_flagged());
    }

    #[test]
    fn set_cleared_noop_when_not_flagged() {
        let router = test_router();
        let order = test_order(0, None, OrderStatus::Expired, &router.metatron);

        assert!(!order.set_cleared());
        assert!(!order.is_cleared());
    }

    #[test]
    fn clear_order_requires_flagged() {
        let test = test_router();
        let router = test.router.clone();
        let order = test_order(0, None, OrderStatus::Expired, &router.metatron);

        add_orders(router.as_ref(), [order.clone()]);

        assert!(router.clear_order(0).is_none());

        order.set_flagged();

        assert!(router.clear_order(0).is_some());
        assert!(order.is_cleared());
        assert!(router.clear_order(0).is_none());
    }

    #[test]
    fn order_creation_rate_window_resets_after_one_minute() {
        let router = test_router();

        let attempt = || {
            router.add_bucket_order(
                test_upstream_target(),
                hash_days(1.0),
                HashPrice::from_sats(1),
            )
        };

        for _ in 0..MAX_ORDER_CREATIONS_PER_MINUTE {
            assert!(matches!(attempt(), Err(RouterError::WalletSyncing)));
        }

        assert!(matches!(
            attempt(),
            Err(RouterError::OrderRateLimited { .. })
        ));

        router.creation_window.lock().start = Instant::now() - Duration::from_secs(61);

        assert!(matches!(attempt(), Err(RouterError::WalletSyncing)));
    }

    #[tokio::test(start_paused = true)]
    async fn execute_order_disconnects_on_retry_exhaustion() {
        let router = test_router();
        let order = test_order(0, None, OrderStatus::Pending, &router.metatron);
        add_orders(router.as_ref(), [order.clone()]);

        router.execute_order(&order).await.unwrap();

        assert_eq!(order.status(), OrderStatus::Disconnected);
    }

    #[tokio::test(start_paused = true)]
    async fn execute_order_cancels_on_cancellation_during_retry() {
        let router = test_router();
        let order = test_order(0, None, OrderStatus::Pending, &router.metatron);
        add_orders(router.as_ref(), [order.clone()]);

        let canceller = order.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1_500)).await;
            canceller.cancel.cancel();
        });

        router.execute_order(&order).await.unwrap();

        assert_eq!(order.status(), OrderStatus::Cancelled);
    }

    #[tokio::test(start_paused = true)]
    async fn execute_order_does_not_cancel_on_router_shutdown() {
        let (router, _directory) = router_with_wallet(None);
        let order = Order::new(
            0,
            test_upstream_target(),
            None,
            router.cancel.child_token(),
            router.metatron.clone(),
        );
        add_orders(router.as_ref(), [order.clone()]);

        let shutdown = router.cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1_500)).await;
            shutdown.cancel();
        });

        router.execute_order(&order).await.unwrap();

        assert_eq!(order.status(), OrderStatus::Pending);
    }

    #[tokio::test(start_paused = true)]
    async fn execute_order_requires_wallet_for_bucket_order() {
        let router = test_router_with_wallet(None);
        let order = Order::new(
            0,
            test_upstream_target(),
            Some(Bucket {
                target: hash_days(100.0),
                payment: Payment::new(test_address(), 0, Amount::from_sat(1000), 0),
            }),
            router.cancel.child_token(),
            router.metatron.clone(),
        );

        assert!(matches!(
            router.execute_order(&order).await,
            Err(RouterError::WalletRequired),
        ));
        assert_eq!(order.status(), OrderStatus::Pending);
    }

    #[tokio::test(start_paused = true)]
    async fn execute_order_waits_for_wallet_sync() {
        let router = test_router();
        let order = Order::new(
            0,
            test_upstream_target(),
            Some(Bucket {
                target: hash_days(100.0),
                payment: Payment::new(test_address(), 0, Amount::from_sat(1000), 0),
            }),
            router.cancel.child_token(),
            router.metatron.clone(),
        );
        order.force_status(OrderStatus::InMempool);
        add_orders(router.as_ref(), [order.clone()]);

        let order_clone = order.clone();
        let router_clone = router.router.clone();
        let handle = tokio::spawn(async move {
            router_clone.execute_order(&order_clone).await.unwrap();
        });

        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(order.status(), OrderStatus::InMempool);

        router.wallet.as_ref().unwrap().mark_synced();
        tokio::time::sleep(Duration::from_millis(1)).await;

        router.cancel.cancel();
        handle.await.unwrap();

        assert_eq!(order.status(), OrderStatus::Pending);
    }

    #[test]
    fn next_order_none_when_only_fulfilled_bucket() {
        let router = test_router();
        let bucket = test_order(
            0,
            Some(hash_days(1.0)),
            OrderStatus::Active,
            &router.metatron,
        );
        set_delivered_work(&router.metatron, &bucket, 1.0);

        add_orders(router.as_ref(), [bucket]);

        assert!(router.next_order(addr(1), &blank()).is_none());
    }

    #[tokio::test]
    async fn restore_gives_each_order_its_own_cancel_token() {
        let directory = tempfile::tempdir().unwrap();
        let store =
            Arc::new(Store::open(&directory.path().join("test.redb"), Chain::Regtest).unwrap());
        let metatron = Arc::new(Metatron::test_with_store(store.clone()));

        // id 4: sink at "worker@bar" (not configured -> orphan -> Cancelled on restore)
        let orphan_sink = test_order(4, None, OrderStatus::Pending, &metatron);
        // id 5: sink at the configured target -> must survive restore untouched
        let kept_sink = Order::new(
            5,
            test_upstream_target(),
            None,
            CancellationToken::new(),
            metatron.clone(),
        );

        let txn = store.begin().unwrap();
        txn.insert_order(orphan_sink.id, &orphan_sink.to_entry())
            .unwrap();
        txn.insert_order(kept_sink.id, &kept_sink.to_entry())
            .unwrap();
        txn.commit().unwrap();

        let router = Arc::new(Router::new(
            Arc::new(Settings::default()),
            metatron,
            None,
            TaskTracker::new(),
            CancellationToken::new(),
            HashValue::from_sats(1),
        ));

        router.restore(&[test_upstream_target()]).unwrap();

        assert_eq!(
            router.get_order(4).unwrap().status(),
            OrderStatus::Cancelled
        );

        let kept = router.get_order(5).unwrap();
        assert!(
            !kept.cancel.is_cancelled(),
            "cancelling orphan sink 4 must not cancel unrelated restored order 5"
        );
        assert!(!kept.status().is_terminal());
    }

    #[test]
    fn restore_terminates_orphan_sink_orders() {
        let directory = tempfile::tempdir().unwrap();
        let store =
            Arc::new(Store::open(&directory.path().join("test.redb"), Chain::Regtest).unwrap());
        let metatron = Arc::new(Metatron::test_with_store(store.clone()));
        let order = test_order(4, None, OrderStatus::Pending, &metatron);

        let txn = store.begin().unwrap();
        txn.insert_order(order.id, &order.to_entry()).unwrap();
        txn.commit().unwrap();

        let router = Arc::new(Router::new(
            Arc::new(Settings::default()),
            metatron,
            None,
            TaskTracker::new(),
            CancellationToken::new(),
            HashValue::from_sats(1),
        ));

        router.restore(&[]).unwrap();

        assert_eq!(
            router.get_order(4).unwrap().status(),
            OrderStatus::Cancelled
        );
        assert!(router.tasks.is_empty());
        assert_eq!(router.book.next_id(), 5);
    }

    #[tokio::test]
    async fn restore_keeps_configured_sink_orders() {
        let directory = tempfile::tempdir().unwrap();
        let store =
            Arc::new(Store::open(&directory.path().join("test.redb"), Chain::Regtest).unwrap());
        let metatron = Arc::new(Metatron::test_with_store(store.clone()));
        let order = test_order(4, None, OrderStatus::Pending, &metatron);
        let target = order.upstream_target.clone();

        let txn = store.begin().unwrap();
        txn.insert_order(order.id, &order.to_entry()).unwrap();
        txn.commit().unwrap();

        let router = Arc::new(Router::new(
            Arc::new(Settings::default()),
            metatron,
            None,
            TaskTracker::new(),
            CancellationToken::new(),
            HashValue::from_sats(1),
        ));

        router.restore(std::slice::from_ref(&target)).unwrap();

        let sinks = router
            .live_orders()
            .into_iter()
            .filter(|order| order.is_sink() && order.upstream_target == target)
            .collect::<Vec<_>>();
        assert_eq!(sinks.len(), 1);
        assert_eq!(sinks[0].id, 4);
        assert!(!sinks[0].status().is_terminal());
        assert_eq!(router.book.next_id(), 5);

        router.cancel.cancel();
        router.tasks.close();
        timeout(Duration::from_secs(1), router.tasks.wait())
            .await
            .expect("configured sink execution should stop after cancellation");
    }

    #[test]
    fn ensure_sink_order_does_not_duplicate_live_sink_target() {
        let router = test_router();
        let restored = test_order(3, None, OrderStatus::Pending, &router.metatron);
        let target = restored.upstream_target.clone();
        add_orders(router.as_ref(), [restored]);

        router.ensure_sink_order(target);

        assert_eq!(router.live_orders().len(), 1);
    }

    #[test]
    fn order_detail_uses_restored_order_stats_without_live_sessions() {
        let router = test_router();
        let order = test_order(3, None, OrderStatus::Fulfilled, &router.metatron);
        let mut stats = Stats::new();
        stats.accepted_shares = 1;
        stats.accepted_work = HashWork::from_difficulty(Difficulty::from(100.0));
        stats.best_share = Some(Difficulty::from(200.0));
        stats.dsps_1m = DecayingAverage::restore(10.0, Duration::from_secs(60), Instant::now());
        router.metatron.restore_order_stats(order.id, stats);

        let detail = crate::api::OrderDetail::from_order(
            &order,
            &router.metatron,
            Instant::now(),
            Vec::new(),
        );

        assert!(detail.sessions.is_empty());
        assert_eq!(detail.upstream.accepted_shares, 1);
        assert!(detail.upstream.accepted_work > HashWork::ZERO);
        assert!(detail.upstream.hashrate_1m > HashRate::ZERO);
        assert_eq!(detail.downstream.accepted_work, HashWork::ZERO);
    }

    #[test]
    fn trim_session_cancels_matching_token() {
        let (metatron, _dir) = Metatron::test();
        let metatron = Arc::new(metatron);
        let order = test_order(0, None, OrderStatus::Pending, &metatron);

        let cancel_kept = CancellationToken::new();
        let cancel_trimmed = CancellationToken::new();
        let kept = metatron.new_session(test_authorization("deadbeef", "foo"), 0);
        let trimmed = metatron.new_session(test_authorization("cafebabe", "bar"), 0);
        order.add_session(kept.clone(), cancel_kept.clone(), addr(1));
        order.add_session(trimmed.clone(), cancel_trimmed.clone(), addr(2));

        assert!(order.trim_session(trimmed.id(), Instant::now()));
        assert!(!order.trim_session(trimmed.id(), Instant::now()));

        assert!(cancel_trimmed.is_cancelled());
        assert!(!cancel_kept.is_cancelled());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn add_order_rejects_bucket_before_sync() {
        let router = test_router();
        let target: UpstreamTarget = "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.foo@bar:3333"
            .parse()
            .unwrap();
        let price = router.hash_price();

        let wallet = router.wallet.as_ref().unwrap();
        assert!(!wallet.is_synced());

        assert!(matches!(
            router.add_bucket_order(target.clone(), hash_days(1e18), price),
            Err(RouterError::WalletSyncing),
        ));

        wallet.mark_synced();

        router
            .add_bucket_order(target, hash_days(1e18), price)
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn add_order_rejects_price_below_minimum() {
        let router = test_router();
        router.set_hash_value(HashValue::from_sats(100));
        router.wallet.as_ref().unwrap().mark_synced();

        let target: UpstreamTarget = "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.foo@bar:3333"
            .parse()
            .unwrap();

        assert!(matches!(
            router.add_bucket_order(target.clone(), hash_days(1e18), HashPrice::from_sats(102)),
            Err(RouterError::HashPriceBelowMinimum { .. }),
        ));

        let order = router
            .add_bucket_order(target.clone(), hash_days(1e18), HashPrice::from_sats(103))
            .unwrap();

        assert_eq!(
            order.bucket.as_ref().unwrap().payment.amount,
            HashPrice::from_sats(103).total(hash_days(1e18)).unwrap(),
        );

        router
            .add_bucket_order(target, hash_days(1e18), HashPrice::from_sats(105))
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn add_bucket_order_charges_bid_price() {
        let router = test_router();
        router.set_hash_value(HashValue::from_sats(100));
        router.set_difficulty_multiplier(2.0);
        router.wallet.as_ref().unwrap().mark_synced();

        assert!(matches!(
            router.add_bucket_order(
                test_upstream_target(),
                hash_days(1e18),
                HashPrice::from_sats(206),
            ),
            Err(RouterError::HashPriceBelowMinimum { .. }),
        ));

        let order = router
            .add_bucket_order(
                test_upstream_target(),
                hash_days(1e18),
                HashPrice::from_sats(250),
            )
            .unwrap();

        assert_eq!(
            order.bucket.as_ref().unwrap().payment.amount,
            HashPrice::from_sats(250).total(hash_days(1e18)).unwrap(),
        );
    }

    #[test]
    fn status_hash_price_adds_five_percent_to_hash_value() {
        let router = test_router();
        router.set_hash_value(HashValue::from_sats(100));

        assert_eq!(router.status().hash_price, HashPrice::from_sats(105));
        assert_eq!(router.status().hash_value, HashValue::from_sats(100));
        assert_eq!(router.status().difficulty_multiplier, 1.0);
    }

    #[test]
    fn set_premium_percent_updates_hash_price() {
        let router = test_router();
        router.set_hash_value(HashValue::from_sats(100));

        router.set_premium_percent(10.0);
        assert_eq!(router.premium_percent(), 10.0);
        assert_eq!(router.hash_price(), HashPrice::from_sats(110));

        router.set_premium_percent(1.0);
        assert_eq!(router.hash_price(), HashPrice::from_sats(101));

        router.set_premium_percent(0.0);
        assert_eq!(router.hash_price(), HashPrice::from_sats(100));
    }

    #[test]
    fn difficulty_multiplier_raises_hash_price() {
        let router = test_router();
        router.set_hash_value(HashValue::from_sats(100));
        router.set_premium_percent(0.0);

        router.set_difficulty_multiplier(2.0);
        assert_eq!(router.hash_price(), HashPrice::from_sats(200));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn bucket_order_creation_persists_revealed_address_before_returning() {
        let directory = tempfile::tempdir().unwrap();
        let store =
            Arc::new(Store::open(&directory.path().join("test.redb"), Chain::Regtest).unwrap());
        let wallet = Arc::new(
            Wallet::open(
                wallet_settings_with_descriptors(directory.path()),
                store.clone(),
            )
            .unwrap(),
        );
        let metatron = Arc::new(Metatron::test_with_store(store.clone()));
        let router = Arc::new(Router::new(
            test_settings(directory.path()),
            metatron,
            Some(wallet),
            TaskTracker::new(),
            CancellationToken::new(),
            HashValue::from_sats(1),
        ));

        let order = add_test_bucket_order(&router);
        let payment = &order.bucket.as_ref().unwrap().payment;

        assert_eq!(
            persisted_next_external_index(&store),
            payment.derivation_index + 1,
        );
        assert_eq!(store.read_orders().unwrap().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn expired_unpaid_bucket_order_does_not_reuse_address() {
        let router = test_router();

        let first = add_test_bucket_order(&router);
        let first_payment = &first.bucket.as_ref().unwrap().payment;
        let first_index = first_payment.derivation_index;
        let first_address = first_payment.address.clone();

        first.force_status(OrderStatus::Expired);
        assert_eq!(first.status(), OrderStatus::Expired);

        let second = add_test_bucket_order(&router);
        let second_payment = &second.bucket.as_ref().unwrap().payment;

        assert_eq!(second_payment.derivation_index, first_index + 1);
        assert_ne!(second_payment.address, first_address);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn restart_after_unpaid_bucket_order_continues_at_next_address() {
        let directory = tempfile::tempdir().unwrap();
        let store_path = directory.path().join("test.redb");
        let store = Arc::new(Store::open(&store_path, Chain::Regtest).unwrap());
        let wallet = Arc::new(
            Wallet::open(
                wallet_settings_with_descriptors(directory.path()),
                store.clone(),
            )
            .unwrap(),
        );
        let metatron = Arc::new(Metatron::test_with_store(store.clone()));
        let router = Arc::new(Router::new(
            test_settings(directory.path()),
            metatron,
            Some(wallet),
            TaskTracker::new(),
            CancellationToken::new(),
            HashValue::from_sats(1),
        ));

        let first = add_test_bucket_order(&router);
        let first_index = first.bucket.as_ref().unwrap().payment.derivation_index;
        router.persist().unwrap();

        router.cancel_order(first.id).unwrap();
        drop(first);
        router.persist().unwrap();
        router.tasks.close();
        timeout(Duration::from_secs(1), router.tasks.wait())
            .await
            .expect("order lifecycle should stop after cancellation");
        drop(router);
        drop(store);

        let store = Arc::new(Store::open(&store_path, Chain::Regtest).unwrap());
        let wallet =
            Arc::new(Wallet::open(wallet_settings_without_descriptors(), store.clone()).unwrap());
        let metatron = Arc::new(Metatron::test_with_store(store.clone()));
        let restarted = TestRouter {
            router: Arc::new(Router::new(
                test_router_settings(),
                metatron,
                Some(wallet),
                TaskTracker::new(),
                CancellationToken::new(),
                HashValue::from_sats(1),
            )),
            _wallet: None,
            _directory: None,
        };

        let second = add_test_bucket_order(&restarted);
        let second_index = second.bucket.as_ref().unwrap().payment.derivation_index;

        assert_eq!(second_index, first_index + 1);
    }

    #[tokio::test]
    async fn add_order_requires_wallet_for_bucket() {
        let router = test_router_with_wallet(None);
        let target: UpstreamTarget = "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.foo@bar:3333"
            .parse()
            .unwrap();

        assert!(matches!(
            router.add_bucket_order(target, hash_days(1.0), router.hash_price(),),
            Err(RouterError::WalletRequired),
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn halt() {
        let router = test_router();
        router.wallet.as_ref().unwrap().mark_synced();

        assert!(!router.halt());
        assert!(!router.status().halt);

        router.set_halt(true);
        assert!(router.status().halt);

        let target = test_upstream_target();
        let price = router.hash_price();

        assert!(matches!(
            router.add_bucket_order(target.clone(), hash_days(1e18), price),
            Err(RouterError::Halted),
        ));

        router.set_halt(false);
        router
            .add_bucket_order(target, hash_days(1e18), price)
            .unwrap();
    }

    #[test]
    fn boost() {
        let router = test_router();

        assert!(!router.boost());
        assert!(!router.status().boost);

        router.set_boost(true);
        assert!(router.status().boost);

        router.set_boost(false);
        assert!(!router.status().boost);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn capacity_enforcement() {
        let router = test_router();
        let wallet = router.wallet.as_ref().unwrap();
        wallet.mark_synced();
        let price = router.hash_price();
        router.set_capacity_work(hash_days(1e18));

        let order = router
            .add_bucket_order(test_upstream_target(), hash_days(6e17), price)
            .unwrap();
        order.force_status(OrderStatus::Active);

        assert!(matches!(
            router.add_bucket_order(test_upstream_target(), hash_days(6e17), price),
            Err(RouterError::InsufficientCapacity { .. }),
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn capacity_enforcement_exact_fit() {
        let router = test_router();
        let wallet = router.wallet.as_ref().unwrap();
        wallet.mark_synced();
        let price = router.hash_price();
        router.set_capacity_work(hash_days(1e18));

        let order = router
            .add_bucket_order(test_upstream_target(), hash_days(5e17), price)
            .unwrap();
        order.force_status(OrderStatus::Active);

        router
            .add_bucket_order(test_upstream_target(), hash_days(5e17), price)
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn capacity_ignores_pending_orders() {
        let router = test_router();
        let wallet = router.wallet.as_ref().unwrap();
        wallet.mark_synced();
        let price = router.hash_price();
        router.set_capacity_work(hash_days(1e18));

        router
            .add_bucket_order(test_upstream_target(), hash_days(6e17), price)
            .unwrap();

        router
            .add_bucket_order(test_upstream_target(), hash_days(6e17), price)
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn rejected_order_does_not_burn_order_id() {
        let router = test_router();
        let wallet = router.wallet.as_ref().unwrap();
        wallet.mark_synced();
        let price = router.hash_price();
        router.set_capacity_work(hash_days(1e18));

        let order = router
            .add_bucket_order(test_upstream_target(), hash_days(6e17), price)
            .unwrap();
        order.force_status(OrderStatus::Active);

        assert!(matches!(
            router.add_bucket_order(test_upstream_target(), hash_days(6e17), price),
            Err(RouterError::InsufficientCapacity { .. }),
        ));

        let next = router
            .add_bucket_order(test_upstream_target(), hash_days(4e17), price)
            .unwrap();
        assert_eq!(next.id, order.id + 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn add_order_rate_limited() {
        let router = test_router();
        router.set_halt(true);

        for _ in 0..MAX_ORDER_CREATIONS_PER_MINUTE {
            assert!(matches!(
                router.add_bucket_order(
                    test_upstream_target(),
                    hash_days(1.0),
                    HashPrice::from_sats(1),
                ),
                Err(RouterError::Halted),
            ));
        }

        assert!(matches!(
            router.add_bucket_order(
                test_upstream_target(),
                hash_days(1.0),
                HashPrice::from_sats(1),
            ),
            Err(RouterError::OrderRateLimited { .. }),
        ));
    }

    #[test]
    fn set_capacity_work_updates_capacity() {
        let router = test_router();
        let wallet = router.wallet.as_ref().unwrap();
        wallet.mark_synced();

        router.set_capacity_work(hash_days(100.0));
        assert_eq!(router.capacity_work().as_f64(), 100.0);

        router.set_capacity_work(hash_days(200.0));
        assert_eq!(router.capacity_work().as_f64(), 200.0);
    }

    #[test]
    fn status_reports_used_capacity_hash_days() {
        let router = test_router();
        router.set_capacity_work(hash_days(500.0));

        let metatron = &router.metatron;
        let order = test_order(0, Some(hash_days(300.0)), OrderStatus::Active, metatron);
        add_orders(router.as_ref(), [order]);

        let status = router.status();
        assert_eq!(status.total_capacity_hash_days.as_f64(), 500.0);
        assert_eq!(status.used_capacity_hash_days.as_f64(), 300.0);
        assert_eq!(status.routing.bucket_order_count, 1);
    }

    #[test]
    fn status_deficit_and_starving_include_only_active_unfulfilled_buckets() {
        let router = test_router();
        let metatron = &router.metatron;

        let active = test_order(0, Some(hash_days(100.0)), OrderStatus::Active, metatron);
        let pending = test_order(1, Some(hash_days(200.0)), OrderStatus::Pending, metatron);
        let fulfilled = test_order(2, Some(hash_days(300.0)), OrderStatus::Active, metatron);
        set_delivered_work(metatron, &fulfilled, 300.0);
        let cancelled = test_order(3, Some(hash_days(400.0)), OrderStatus::Cancelled, metatron);
        let sink = test_order(4, None, OrderStatus::Active, metatron);

        add_orders(
            router.as_ref(),
            [active, pending, fulfilled, cancelled, sink],
        );

        let status = router.status();
        assert_eq!(status.routing.deficit_hashrate, HashRate::from_hps(100.0));
        assert_eq!(status.routing.starving_order_count, 1);
    }

    #[test]
    fn status_reports_placements() {
        let router = test_router();
        let sink = test_order(0, None, OrderStatus::Active, &router.metatron);
        add_orders(router.as_ref(), [sink]);

        router.next_order(addr(1), &blank()).unwrap();

        assert_eq!(
            router.status().routing.placements_1h,
            PlacementCounts {
                blind: 1,
                ..PlacementCounts::default()
            }
        );
    }
}
