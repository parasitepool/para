use {
    super::*,
    crate::{
        api::{
            DownstreamStats, OrphanReceipt, PlacementCounts, RouterStatus, RoutingInfo,
            UpstreamStats, UpstreamTotals, WalletInfo,
        },
        event_sink::Event,
        generator::get_block_template,
        hash::price::difficulty_multiplier,
    },
    bdk_wallet::ChangeSet,
    control::Control,
    error::{RouterError, RouterResult},
    greeter::{Prelude, greet},
    order::{Bucket, Order, OrderStatus, Payment},
    orders::Orders,
};

pub mod control;
pub mod error;
pub mod greeter;
mod intents;
pub mod order;
mod orders;

const PAYMENT_TIMEOUT: u32 = 6;
const EXTENDED_PAYMENT_TIMEOUT: u32 = 144;
const MAX_ORDER_CREATIONS_PER_MINUTE: usize = 100;
const SWEEP_INTERVAL: Duration = Duration::from_hours(1);
const DEFAULT_REFUND_FEE_RATE: FeeRate = FeeRate::from_sat_per_kwu(250);

struct RateWindow {
    start: Instant,
    count: usize,
}

pub(crate) struct Refund {
    pub(crate) psbt: Psbt,
    pub(crate) destination: Address,
    pub(crate) amount: Amount,
    pub(crate) outpoints: Vec<OutPoint>,
    pub(crate) fee_rate: FeeRate,
}

pub(crate) struct Router {
    settings: Arc<Settings>,
    metatron: Arc<Metatron>,
    wallet: Option<Arc<Wallet>>,
    orders: RwLock<Orders>,
    next_id: AtomicU32,
    creation_window: Mutex<RateWindow>,
    orphan_receipts: Mutex<BTreeMap<u32, OrphanReceipt>>,
    hash_value: AtomicU64,
    halt: AtomicBool,
    boost: AtomicBool,
    capacity_work: AtomicU64,
    premium_percent: AtomicU64,
    difficulty_multiplier: AtomicU64,
    control: Control,
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

        Self {
            settings,
            metatron,
            wallet,
            orders: RwLock::new(Orders::new()),
            next_id: AtomicU32::new(0),
            creation_window: Mutex::new(RateWindow {
                start: Instant::now(),
                count: 0,
            }),
            orphan_receipts: Mutex::new(BTreeMap::new()),
            hash_value: AtomicU64::new(initial_hash_value.to_sats()),
            halt: AtomicBool::new(halt),
            boost: AtomicBool::new(boost),
            capacity_work: AtomicU64::new(capacity_work.as_f64().to_bits()),
            premium_percent: AtomicU64::new(premium_percent.to_bits()),
            difficulty_multiplier: AtomicU64::new(1.0f64.to_bits()),
            control,
            tasks,
            cancel,
        }
    }

    pub(crate) fn hash_value(&self) -> HashValue {
        HashValue::from_sats(self.hash_value.load(Ordering::Relaxed))
    }

    pub(crate) fn hash_price(&self) -> HashPrice {
        HashPrice::from_hash_value(
            self.hash_value(),
            self.premium_percent(),
            f64::from_bits(self.difficulty_multiplier.load(Ordering::Relaxed)),
        )
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

    pub(crate) fn set_difficulty_multiplier(&self, multiplier: f64) {
        self.difficulty_multiplier
            .store(multiplier.to_bits(), Ordering::Relaxed);
    }

    pub(crate) fn difficulty_multiplier(&self) -> f64 {
        f64::from_bits(self.difficulty_multiplier.load(Ordering::Relaxed))
    }

    pub(crate) fn set_hash_value(&self, hash_value: HashValue) {
        self.hash_value
            .store(hash_value.to_sats(), Ordering::Relaxed);
    }

    pub(crate) fn cancel_order(&self, id: u32) -> Option<Arc<Order>> {
        let order = self.orders.read().get(id)?;
        order.terminate(OrderStatus::Cancelled);
        Some(order)
    }

    pub(crate) fn clear_order(&self, id: u32) -> Option<Arc<Order>> {
        let order = self.orders.read().get(id)?;

        if !order.set_cleared() {
            return None;
        }

        Some(order)
    }

    pub(crate) fn get_order(&self, id: u32) -> Option<Arc<Order>> {
        self.orders.read().get(id)
    }

    pub(crate) fn cold_order(&self, id: u32) -> Option<entry::OrderEntry> {
        self.orders.read().cold_entry(id).cloned()
    }

    fn refund_target(&self, id: u32) -> RouterResult<(u32, Address<NetworkUnchecked>)> {
        if let Some(order) = self.get_order(id) {
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

        let wallet = self.wallet.as_ref().ok_or(RouterError::WalletRequired)?;

        if !wallet.is_synced() {
            return Err(RouterError::WalletSyncing);
        }

        let outpoints = wallet.unspent_by_derivation_index(derivation_index);

        if outpoints.is_empty() {
            return Err(RouterError::NoUnspentFunds { id });
        }

        let destination = match destination {
            Some(destination) => destination,
            None => default_destination
                .require_network(self.settings.chain().network())
                .map_err(|_| RouterError::InvalidRefundDestination { id })?,
        };

        let fee_rate = fee_rate.unwrap_or(DEFAULT_REFUND_FEE_RATE);

        let psbt = wallet
            .build_refund_psbt(&outpoints, destination.clone(), fee_rate)
            .map_err(|error| RouterError::RefundConstruction { error })?;

        let amount = psbt.unsigned_tx.output[0].value;

        Ok(Refund {
            psbt,
            destination,
            amount,
            outpoints,
            fee_rate,
        })
    }

    pub(crate) fn order_snapshots(
        &self,
        cold_filter: impl Fn(u32, &entry::OrderEntry) -> bool,
    ) -> (Vec<Arc<Order>>, Vec<(u32, entry::OrderEntry)>) {
        let orders = self.orders.read();

        let live = orders.live();

        let cold = orders
            .cold_entries()
            .filter(|(id, entry)| cold_filter(*id, entry))
            .map(|(id, entry)| (id, entry.clone()))
            .collect();

        (live, cold)
    }

    pub(crate) fn live_orders(&self) -> Vec<Arc<Order>> {
        self.orders.read().live()
    }

    pub(crate) fn wallet(&self) -> Option<&Wallet> {
        self.wallet.as_deref()
    }

    pub(crate) fn metatron(&self) -> Arc<Metatron> {
        self.metatron.clone()
    }

    pub(crate) fn next_order(&self, addr: SocketAddr, prelude: &Prelude) -> Option<Arc<Order>> {
        self.control
            .next_order(&self.orders.read().routable(), addr, prelude)
    }

    fn allocate_id(&self) -> RouterResult<u32> {
        self.next_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| RouterError::OrderIdExhausted)
    }

    pub(crate) fn add_sink_order(
        self: &Arc<Self>,
        upstream_target: UpstreamTarget,
    ) -> RouterResult<Arc<Order>> {
        let id = self.allocate_id()?;

        let order = Order::new(
            id,
            upstream_target,
            None,
            self.cancel.child_token(),
            self.metatron.clone(),
        );

        self.register_and_execute(order.clone());

        Ok(order)
    }

    pub(crate) fn ensure_sink_order(self: &Arc<Self>, upstream_target: UpstreamTarget) {
        if self.orders.read().live().into_iter().any(|order| {
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

        let order = {
            let mut orders = self.orders.write();

            let used = orders.used_work();
            let capacity = self.capacity_work();
            if used.as_f64() + target.as_f64() > capacity.as_f64() {
                let available = HashDays::from_raw((capacity.as_f64() - used.as_f64()).max(0.0));
                return Err(RouterError::InsufficientCapacity {
                    requested: target,
                    available,
                });
            }

            let id = self.allocate_id()?;

            let order = wallet
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
                .map_err(|error| RouterError::WalletPersistence { error })?;

            orders.add(order.clone());
            order
        };

        self.spawn_order_execution(order.clone());

        Ok(order)
    }

    fn register_and_execute(self: &Arc<Self>, order: Arc<Order>) {
        self.orders.write().add(order.clone());
        self.spawn_order_execution(order);
    }

    fn spawn_order_execution(self: &Arc<Self>, order: Arc<Order>) {
        if order.status().is_terminal() {
            return;
        }
        let router = self.clone();
        self.tasks.spawn(async move {
            if let Err(err) = router.execute_order(&order).await {
                error!("Order {} execution error: {err}", order.id);
            }
        });
    }

    async fn execute_order(self: &Arc<Self>, order: &Arc<Order>) -> RouterResult<()> {
        if order.bucket.is_some() {
            let wallet = self.wallet.as_ref().ok_or(RouterError::WalletRequired)?;

            tokio::select! {
                biased;
                _ = order.cancel.cancelled() => {
                    if !self.cancel.is_cancelled() {
                        order.terminate(OrderStatus::Cancelled);
                    }
                    return Ok(());
                }
                synced = wallet.synced() => {
                    if !synced {
                        return Ok(());
                    }
                }
            }
        }

        if let Some(bucket) = &order.bucket
            && order.status().awaiting_payment()
            && !self.wait_for_payment(order, &bucket.payment).await?
        {
            return Ok(());
        }

        let check_interval = self.settings.tick_interval();

        loop {
            match retry_with_backoff(&order.cancel, &format!("Order {}", order.id), || {
                order.connect(
                    self.settings.timeout(),
                    self.settings.enonce1_extension_size(),
                    &self.tasks,
                )
            })
            .await
            {
                Ok(()) => {}
                Err(BackoffEnd::Cancelled) => {
                    if !self.cancel.is_cancelled() {
                        order.terminate(OrderStatus::Cancelled);
                    }

                    return Ok(());
                }
                Err(BackoffEnd::Exhausted) => {
                    order.terminate(OrderStatus::Disconnected);

                    return Ok(());
                }
            }

            let upstream = order
                .upstream()
                .ok_or(RouterError::MissingActiveUpstream { id: order.id })?;

            tokio::select! {
                biased;
                _ = order.cancel.cancelled() => {
                    if !self.cancel.is_cancelled() {
                        order.terminate(OrderStatus::Cancelled);
                    }
                    return Ok(());
                }
                _ = upstream.disconnected() => {
                    warn!(
                        "Upstream {} disconnected, attempting reconnect for order {}",
                        upstream.endpoint(),
                        order.id,
                    );

                    order.cancel_all_sessions();

                    if order.is_fulfilled() {
                        info!("Order {} fulfilled", order.id);
                        order.terminate(OrderStatus::Fulfilled);

                        return Ok(());
                    }

                    continue;
                }
                _ = async {
                    let mut ticker = ticker(check_interval);
                    while !order.is_fulfilled() {
                        ticker.tick().await;
                    }
                } => {
                    info!("Order {} fulfilled", order.id);
                    order.terminate(OrderStatus::Fulfilled);

                    return Ok(());
                }
            }
        }
    }

    async fn wait_for_payment(
        self: &Arc<Self>,
        order: &Arc<Order>,
        payment: &Payment,
    ) -> RouterResult<bool> {
        let wallet = self.wallet.as_ref().ok_or(RouterError::WalletRequired)?;
        let mut sync_rx = wallet.subscribe_sync();

        loop {
            let total = wallet.received(payment.derivation_index);

            let timeout = if total >= payment.amount {
                EXTENDED_PAYMENT_TIMEOUT
            } else {
                PAYMENT_TIMEOUT
            };

            let deadline_height = payment.created_at_height.saturating_add(timeout);

            let confirmed_by_deadline =
                wallet.received_by_deadline(payment.derivation_index, deadline_height);

            let timed_out = wallet.tip() >= deadline_height;

            {
                let lifecycle = order.lifecycle.lock();

                if confirmed_by_deadline >= payment.amount && lifecycle.status.awaiting_payment() {
                    return Ok(true);
                }

                if timed_out {
                    drop(lifecycle);
                    order.terminate(OrderStatus::Expired);
                    return Ok(false);
                }
            }

            order.note_payment_seen(total >= payment.amount);

            tokio::select! {
                biased;
                _ = order.cancel.cancelled() => return Ok(false),
                _ = sync_rx.changed() => {}
            }
        }
    }

    fn active_route(order: &Order) -> RouterResult<(Arc<Upstream>, Arc<EnonceAllocator>)> {
        let upstream = order
            .upstream()
            .ok_or(RouterError::MissingActiveUpstream { id: order.id })?;

        let allocator = order
            .allocator()
            .cloned()
            .ok_or(RouterError::MissingActiveAllocator { id: order.id })?;

        Ok((upstream, allocator))
    }

    pub(crate) fn restore(self: &Arc<Self>, sink_orders: &[UpstreamTarget]) -> Result {
        let entries = self.metatron.store().read_orders()?;
        let mut next_id = 0u32;

        for (id, entry) in entries {
            let candidate = id
                .checked_add(1)
                .with_context(|| format!("persisted order id {id} exhausts u32 order ids"))?;

            next_id = next_id.max(candidate);

            if entry.status.is_terminal() && entry.review != Review::Flagged {
                debug!("Not restoring terminal order {id}");
                self.orders.write().add_cold(id, entry);
                continue;
            }

            let order = Order::restore(
                id,
                entry,
                self.settings.chain().network(),
                self.cancel.child_token(),
                self.metatron.clone(),
            )?;

            if order.is_sink()
                && !sink_orders
                    .iter()
                    .any(|target| target == &order.upstream_target)
            {
                info!(
                    "Marking orphan sink order {} for {} as cancelled; not in configured sinks",
                    order.id, order.upstream_target,
                );
                order.terminate(OrderStatus::Cancelled);
            }

            self.register_and_execute(order);
        }

        self.next_id.store(next_id, Ordering::Relaxed);

        for upstream_target in sink_orders {
            self.ensure_sink_order(upstream_target.clone());
        }

        Ok(())
    }

    pub(crate) fn flag_orders(&self) {
        let Some(wallet) = &self.wallet else {
            return;
        };

        let confirmed = wallet.confirmed_by_index();

        for order in self.orders.read().live() {
            let Some(bucket) = &order.bucket else {
                continue;
            };

            let received = confirmed
                .get(&bucket.payment.derivation_index)
                .copied()
                .unwrap_or(Amount::ZERO);

            if order.status().is_terminal() && !order.is_fulfilled() && received > Amount::ZERO {
                order.set_flagged();
            }

            if wallet.is_synced()
                && order.status() == OrderStatus::Active
                && received < bucket.payment.amount
                && order.set_flagged()
            {
                warn!(
                    "Active order {} payment vanished (received {received} of {})",
                    order.id, bucket.payment.amount,
                );
            }
        }

        let rehydratable = {
            let orders = self.orders.read();

            confirmed
                .iter()
                .filter(|(_, amount)| **amount > Amount::ZERO)
                .filter_map(|(index, _)| orders.cold_id(*index))
                .collect::<Vec<_>>()
        };

        for id in rehydratable {
            let entry = self.orders.read().cold_entry(id).cloned();

            let Some(entry) = entry else {
                continue;
            };

            if entry.status == OrderStatus::Fulfilled || entry.review != Review::Clean {
                continue;
            }

            match Order::restore(
                id,
                entry,
                self.settings.chain().network(),
                self.cancel.child_token(),
                self.metatron.clone(),
            ) {
                Ok(order) => {
                    info!("Rehydrating funded terminal order {id} for review");
                    order.set_flagged();

                    let mut orders = self.orders.write();
                    orders.add(order);
                    orders.remove_cold(id);
                }
                Err(err) => warn!("Failed to rehydrate order {id}: {err:#}"),
            }
        }
    }

    pub(crate) fn audit_receipts(&self) -> Vec<(u32, Amount)> {
        let Some(wallet) = &self.wallet else {
            return Vec::new();
        };

        if !wallet.is_synced() {
            return Vec::new();
        }

        let orders = self.orders.read();

        let known = orders
            .live()
            .iter()
            .filter_map(|order| {
                order
                    .bucket
                    .as_ref()
                    .map(|bucket| bucket.payment.derivation_index)
            })
            .collect::<HashSet<_>>();

        let mut orphans = Vec::new();

        for (index, amount) in wallet.confirmed_by_index() {
            if amount == Amount::ZERO {
                continue;
            }

            if !known.contains(&index) && orders.cold_id(index).is_none() {
                orphans.push((index, amount));
            }
        }

        orphans
    }

    pub(crate) fn sweep_orphan_receipts(&self) {
        let Some(wallet) = &self.wallet else {
            return;
        };

        if !wallet.is_synced() {
            return;
        }

        let findings = self.audit_receipts();

        let height = wallet.tip();

        let mut stored = self.orphan_receipts.lock();

        stored.retain(|index, _| findings.iter().any(|(orphan, _)| orphan == index));

        for (index, amount) in findings {
            if let Some(receipt) = stored.get_mut(&index) {
                receipt.amount = amount;
            } else {
                warn!("Received {amount} at derivation index {index} with no matching order");

                stored.insert(
                    index,
                    OrphanReceipt {
                        derivation_index: index,
                        address: wallet.peek_address(index).as_unchecked().clone(),
                        amount,
                        first_seen_height: height,
                    },
                );
            }
        }
    }

    pub(crate) fn persist(&self) -> Result {
        let mut entries = Vec::new();
        let mut candidates = Vec::new();

        for order in self.orders.read().live() {
            let lifecycle = order.lifecycle();

            if lifecycle.status.is_terminal() && !lifecycle.dirty {
                continue;
            }

            order.clear_dirty();
            entries.push((order.id, order.to_entry()));
            candidates.push(order);
        }

        let result = if let Some(wallet) = &self.wallet {
            wallet.persist_staged_with(|wallet_delta| self.metatron.persist(&entries, wallet_delta))
        } else {
            self.metatron.persist(&entries, &ChangeSet::default())
        };

        if result.is_err() {
            for order in &candidates {
                order.mark_dirty();
            }
        }

        result
    }

    fn retirable(order: &Order) -> bool {
        let lifecycle = order.lifecycle();

        lifecycle.status.is_terminal() && lifecycle.review != Review::Flagged && !lifecycle.dirty
    }

    pub(crate) fn retire_orders(&self) {
        let mut orders = self.orders.write();

        let retirable = orders
            .live()
            .into_iter()
            .filter(|order| Self::retirable(order))
            .collect::<Vec<_>>();

        for order in retirable {
            if !Self::retirable(&order) {
                continue;
            }

            debug!(
                "Retiring terminal order {} at {} to cold storage",
                order.id, order.upstream_target,
            );

            orders.retire(&order);
            self.metatron.remove_order(order.id);
        }
    }

    fn rebalance(&self) {
        self.control
            .rebalance(&self.orders.read().active(), self.boost());
    }

    pub(crate) fn status(&self) -> RouterStatus {
        let now = Instant::now();
        let metatron = &self.metatron;
        let guard = self.orders.read();
        let used = guard.used_work();
        let orders = guard.live();

        drop(guard);

        let mut bucket_order_count = 0;
        let mut sink_order_count = 0;
        let mut starving_order_count = 0;
        let mut deficit_hashrate = HashRate::ZERO;
        let mut pending = 0;
        let mut disconnected = 0;

        let mut active_addresses: HashSet<&Address<NetworkUnchecked>> = HashSet::new();
        let mut active_workers: HashSet<&str> = HashSet::new();
        let mut live_addresses: HashSet<&Address<NetworkUnchecked>> = HashSet::new();

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

            live_addresses.insert(username.address());
            live.absorb(stats, now);
        }

        let guard = self.orders.read();
        let cold = guard.cold_totals();

        live_addresses.extend(cold.addresses.iter());
        let total_users = live_addresses.len();
        let total_orders = guard.cold_count() + orders.len();

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

        drop(guard);

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
                orphan_receipts: self.orphan_receipts.lock().values().cloned().collect(),
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
                            match get_block_template(bitcoin_client, &router.settings).await {
                                Ok(template) => {
                                    router.set_hash_value(HashValue::compute(
                                        template.coinbase_value,
                                        template.bits,
                                    ));

                                    let multiplier =
                                        difficulty_multiplier(bitcoin_client, template.height)
                                            .await;
                                    router.set_difficulty_multiplier(multiplier);
                                }
                                Err(err) => warn!("Failed to update hash value: {err}"),
                            }
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

        loop {
            let (stream, addr) = tokio::select! {
                accept = listener.accept() => {
                    match accept {
                        Ok((stream, addr)) => (stream, addr),
                        Err(err) => {
                            error!("Accept error: {err}");
                            continue;
                        }
                    }
                }
                _ = cancel_token.cancelled() => {
                    info!("Shutting down router");

                    self.tasks.close();
                    let _ = timeout(Duration::from_secs(2), self.tasks.wait()).await;

                    if let Err(err) = self.persist() {
                        warn!("Final router persistence error: {err}");
                    }

                    info!("All router tasks stopped");

                    return Ok(());
                }
            };

            let _ = stream.set_nodelay(true);

            let router = self.clone();
            let event_tx = event_tx.clone();

            self.tasks.spawn(async move {
                let (read_half, write_half) = stream.into_split();

                let reader =
                    FramedRead::new(read_half, LinesCodec::new_with_max_length(MAX_MESSAGE_SIZE));

                let writer = FramedWrite::new(write_half, LinesCodec::new());

                let Some((reader, prelude)) = greet(reader, addr).await else {
                    return;
                };

                let Some(order) = router.next_order(addr, &prelude) else {
                    warn!("No order to match with available, dropping connection from {addr}");
                    return;
                };

                let order_type = if order.is_sink() { "sink" } else { "bucket" };

                info!(
                    "Routing {addr} to {order_type} order {} at {}",
                    order.id, order.upstream_target,
                );

                let settings = router.settings.clone();
                let metatron = router.metatron.clone();
                let start_diff = settings.start_diff();
                let cancel = order.cancel.child_token();

                let (upstream, allocator) = match Router::active_route(&order) {
                    Ok(route) => route,
                    Err(err) => {
                        error!("Dropping {addr} for order {}: {err}", order.id);
                        order.release_placement(&addr);
                        return;
                    }
                };

                let mut stratifier: Stratifier<Notify> = Stratifier::new(
                    addr,
                    settings,
                    allocator,
                    metatron,
                    Some(upstream.clone()),
                    reader,
                    writer,
                    prelude.inbox,
                    upstream.workbase_rx(),
                    cancel,
                    event_tx,
                    start_diff,
                    Some(order.clone()),
                );

                if let Err(err) = stratifier.serve().await {
                    error!("Stratifier error for {addr} on order {}: {err}", order.id);
                }
            });
        }
    }
}

impl StatusLine for Router {
    fn status_line(&self) -> String {
        let now = Instant::now();
        let stats = self.metatron.snapshot();

        format!(
            "orders={}  sessions={}  hashrate={:.2}  blocks={}",
            self.orders.read().active().len(),
            self.metatron.total_sessions(),
            stats.hashrate_1m(now),
            self.metatron.block_count(),
        )
    }
}

#[cfg(test)]
mod tests {
    use {super::*, crate::settings::CommonOptions, bdk_wallet::KeychainKind};

    struct TestWallet {
        wallet: Arc<Wallet>,
        store: Arc<Store>,
        _directory: tempfile::TempDir,
    }

    struct TestRouter {
        router: Arc<Router>,
        _wallet: Option<TestWallet>,
        _directory: Option<tempfile::TempDir>,
    }

    impl std::ops::Deref for TestRouter {
        type Target = Arc<Router>;

        fn deref(&self) -> &Self::Target {
            &self.router
        }
    }

    impl AsRef<Router> for TestRouter {
        fn as_ref(&self) -> &Router {
            self.router.as_ref()
        }
    }

    fn test_address() -> Address {
        "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc"
            .parse::<Address<NetworkUnchecked>>()
            .unwrap()
            .assume_checked()
    }

    fn test_wallet() -> TestWallet {
        let (descriptor, change_descriptor) = test_wallet_descriptors();
        let directory = tempfile::tempdir().unwrap();
        let settings = wallet_settings_with_descriptors_and_data_dir(
            descriptor,
            change_descriptor,
            directory.path(),
        );
        let store =
            Arc::new(Store::open(&directory.path().join("test.redb"), Chain::Regtest).unwrap());
        let wallet = Arc::new(Wallet::open(settings, store.clone()).unwrap());

        TestWallet {
            wallet,
            store,
            _directory: directory,
        }
    }

    fn test_wallet_descriptors() -> (String, String) {
        let mnemonic: bdk_wallet::keys::bip39::Mnemonic =
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about"
                .parse()
                .unwrap();

        let (_, descriptor, change_descriptor) =
            Wallet::generate_from_mnemonic(mnemonic, bitcoin::Network::Regtest).unwrap();

        (descriptor, change_descriptor)
    }

    fn test_router() -> TestRouter {
        let wallet = test_wallet();
        let metatron = Arc::new(Metatron::test_with_store(wallet.store.clone()));
        let router = Arc::new(Router::new(
            Arc::new(Settings::default()),
            metatron,
            Some(wallet.wallet.clone()),
            TaskTracker::new(),
            CancellationToken::new(),
            HashValue::from_sats(1),
        ));

        TestRouter {
            router,
            _wallet: Some(wallet),
            _directory: None,
        }
    }

    fn test_router_with_wallet(wallet: Option<Arc<Wallet>>) -> TestRouter {
        let (router, directory) = router_with_wallet(wallet);
        TestRouter {
            router,
            _wallet: None,
            _directory: Some(directory),
        }
    }

    fn test_settings(data_dir: &Path) -> Arc<Settings> {
        Arc::new(
            Settings::from_proxy_options(ProxyOptions {
                common: CommonOptions {
                    address: "127.0.0.1".into(),
                    port: 0,
                    http_port: None,
                    bitcoin: BitcoinOptions {
                        chain: Some(Chain::Regtest),
                        bitcoin_data_dir: None,
                        bitcoin_rpc_port: Some(1),
                        bitcoin_rpc_cookie_file: None,
                        bitcoin_rpc_username: Some("user".into()),
                        bitcoin_rpc_password: Some("pass".into()),
                    },
                    start_diff: Difficulty::default(),
                    min_diff: None,
                    max_diff: None,
                    vardiff_period: 3.33,
                    vardiff_window: 300.0,
                    acme_domain: Vec::new(),
                    acme_contact: Vec::new(),
                    acme_cache: PathBuf::from("acme-cache"),
                    data_dir: Some(data_dir.to_path_buf()),
                    store_path: None,
                    http_api_token: None,
                    http_admin_token: None,
                },
                upstream: "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.worker@127.0.0.1:1"
                    .parse()
                    .unwrap(),
                timeout: 30,
                enonce1_extension_size: ENONCE1_EXTENSION_SIZE,
            })
            .unwrap(),
        )
    }

    fn test_router_settings() -> Arc<Settings> {
        let (descriptor, change_descriptor) = test_wallet_descriptors();

        Arc::new(
            Settings::from_router_options(RouterOptions {
                common: CommonOptions {
                    address: "127.0.0.1".into(),
                    port: 0,
                    http_port: None,
                    bitcoin: BitcoinOptions {
                        chain: Some(Chain::Regtest),
                        bitcoin_data_dir: None,
                        bitcoin_rpc_port: Some(1),
                        bitcoin_rpc_cookie_file: None,
                        bitcoin_rpc_username: Some("user".into()),
                        bitcoin_rpc_password: Some("pass".into()),
                    },
                    start_diff: Difficulty::default(),
                    min_diff: None,
                    max_diff: None,
                    vardiff_period: 3.33,
                    vardiff_window: 300.0,
                    acme_domain: Vec::new(),
                    acme_contact: Vec::new(),
                    acme_cache: PathBuf::from("acme-cache"),
                    data_dir: None,
                    store_path: None,
                    http_api_token: None,
                    http_admin_token: None,
                },
                descriptor,
                change_descriptor: Some(change_descriptor),
                wallet_birthday: 0,
                timeout: 30,
                enonce1_extension_size: ENONCE1_EXTENSION_SIZE,
                tick_interval: 60,
                sink_order: Vec::new(),
                halt: false,
                boost: false,
                capacity_work: 1e18,
                premium_percent: 5.0,
            })
            .unwrap(),
        )
    }

    fn router_with_wallet(wallet: Option<Arc<Wallet>>) -> (Arc<Router>, tempfile::TempDir) {
        let directory = tempfile::tempdir().unwrap();
        let store =
            Arc::new(Store::open(&directory.path().join("test.redb"), Chain::Regtest).unwrap());
        let router = Arc::new(Router::new(
            Arc::new(Settings::default()),
            Arc::new(Metatron::test_with_store(store)),
            wallet,
            TaskTracker::new(),
            CancellationToken::new(),
            HashValue::from_sats(1),
        ));
        (router, directory)
    }

    fn wallet_settings_without_descriptors() -> Arc<Settings> {
        Arc::new(
            Settings::from_bitcoin_options(BitcoinOptions {
                chain: Some(Chain::Regtest),
                bitcoin_data_dir: None,
                bitcoin_rpc_port: Some(1),
                bitcoin_rpc_cookie_file: None,
                bitcoin_rpc_username: Some("user".into()),
                bitcoin_rpc_password: Some("pass".into()),
            })
            .unwrap(),
        )
    }

    fn wallet_settings_with_descriptors(data_dir: &Path) -> Arc<Settings> {
        let (descriptor, change_descriptor) = test_wallet_descriptors();

        wallet_settings_with_descriptors_and_data_dir(descriptor, change_descriptor, data_dir)
    }

    fn wallet_settings_with_descriptors_and_data_dir(
        descriptor: String,
        change_descriptor: String,
        data_dir: &Path,
    ) -> Arc<Settings> {
        Arc::new(
            Settings::from_wallet_options(
                BitcoinOptions {
                    chain: Some(Chain::Regtest),
                    bitcoin_data_dir: None,
                    bitcoin_rpc_port: Some(1),
                    bitcoin_rpc_cookie_file: None,
                    bitcoin_rpc_username: Some("user".into()),
                    bitcoin_rpc_password: Some("pass".into()),
                },
                Some(data_dir.to_path_buf()),
                None,
                Some(descriptor),
                Some(change_descriptor),
                0,
            )
            .unwrap(),
        )
    }

    fn persisted_next_external_index(store: &Store) -> u32 {
        let changeset = store.read_wallet_changeset().unwrap();
        let mut wallet = bdk_wallet::Wallet::load()
            .check_network(Network::Regtest)
            .load_wallet_no_persist(changeset)
            .unwrap()
            .expect("wallet state persisted");

        wallet.reveal_next_address(KeychainKind::External).index
    }

    fn test_upstream_target() -> UpstreamTarget {
        "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.foo@bar:3333"
            .parse()
            .unwrap()
    }

    fn add_test_bucket_order(router: &Arc<Router>) -> Arc<Order> {
        let wallet = router.wallet.as_ref().unwrap();
        wallet.mark_synced();

        router
            .add_bucket_order(test_upstream_target(), hash_days(1e18), router.hash_price())
            .unwrap()
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

    fn test_order_with_payment(
        id: u32,
        payment: Payment,
        status: OrderStatus,
        metatron: &Arc<Metatron>,
    ) -> Arc<Order> {
        let order = Order::new(
            id,
            test_upstream_target(),
            Some(Bucket {
                target: hash_days(100.0),
                payment,
            }),
            CancellationToken::new(),
            metatron.clone(),
        );

        order.force_status(status);
        order
    }

    fn payment(order: &Order) -> &Payment {
        &order.bucket.as_ref().unwrap().payment
    }

    fn hash_days(value: f64) -> HashDays {
        HashDays::new(value).unwrap()
    }

    fn set_delivered_work(metatron: &Metatron, order: &Order, value: f64) {
        metatron.set_order_delivered_work(order.id, hash_days(value).to_hash_work());
    }

    fn add_orders(router: &Router, orders: impl IntoIterator<Item = Arc<Order>>) {
        let mut stored = router.orders.write();

        for order in orders {
            stored.add(order);
        }
    }

    fn addr(port: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], port))
    }

    fn blank() -> Prelude {
        Prelude::default()
    }

    fn ids(orders: Vec<Arc<Order>>) -> Vec<u32> {
        orders.into_iter().map(|order| order.id).collect()
    }

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
    fn cold_totals_track_cold_entries() {
        let router = test_router();

        router
            .metatron
            .record_order_accepted(0, Difficulty::from(1.0), Difficulty::from(1.0));
        router
            .metatron
            .record_order_accepted(1, Difficulty::from(1.0), Difficulty::from(1.0));
        router
            .metatron
            .record_order_accepted(1, Difficulty::from(1.0), Difficulty::from(1.0));

        let first = test_order(0, None, OrderStatus::Expired, &router.metatron).to_entry();
        let second = test_order(1, None, OrderStatus::Expired, &router.metatron).to_entry();

        let mut orders = router.orders.write();

        orders.add_cold(0, first);
        orders.add_cold(1, second);

        let totals = orders.cold_totals();
        assert_eq!(totals.accepted_shares, 3);
        assert!(totals.best_share.is_some());
        assert!(totals.last_share_secs.is_some());
        assert_eq!(totals.addresses.len(), 1);
        assert_eq!(orders.cold_count(), 2);

        orders.remove_cold(0);

        let totals = orders.cold_totals();
        assert_eq!(totals.accepted_shares, 2);
        assert_eq!(totals.addresses.len(), 1);
        assert_eq!(orders.cold_count(), 1);

        orders.remove_cold(0);

        assert_eq!(orders.cold_totals().accepted_shares, 2);
    }

    #[test]
    fn flag_orders_resurrection_conserves_upstream_totals() {
        let directory = tempfile::tempdir().unwrap();
        let (router, wallet) = regtest_wallet_router(&directory);
        wallet.mark_synced();

        let cold = wallet.test_reveal_address();

        router
            .metatron
            .record_order_accepted(1, Difficulty::from(1.0), Difficulty::from(1.0));

        let cold_entry = test_order_with_payment(
            1,
            Payment::new(cold.address.clone(), cold.index, Amount::from_sat(1000), 0),
            OrderStatus::Expired,
            &router.metatron,
        )
        .to_entry();

        router.orders.write().add_cold(1, cold_entry);

        let tx = wallet.test_receive_unconfirmed(&cold.address, Amount::from_sat(1000));
        wallet.test_confirm_tx(tx);

        let before = router.status();
        assert_eq!(before.upstream.accepted_shares, 0);
        assert_eq!(before.upstream.totals.accepted_shares, 1);
        assert_eq!(before.upstream.totals.orders, 1);
        assert_eq!(before.upstream.totals.users, 1);

        router.flag_orders();

        let after = router.status();
        assert_eq!(after.upstream.accepted_shares, 0);
        assert_eq!(after.upstream.totals.accepted_shares, 1);
        assert_eq!(after.upstream.totals.orders, 1);
        assert_eq!(after.upstream.totals.users, 1);
        assert_eq!(router.orders.read().cold_count(), 0);
        assert!(router.orders.read().get(1).is_some());
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
    fn flag_orders_flags_terminal_funded_unfulfilled_orders() {
        let test = test_router();
        let router = test.router.clone();
        let wallet = router.wallet.as_ref().unwrap().clone();

        #[track_caller]
        fn funded_order(
            router: &Router,
            wallet: &Wallet,
            id: u32,
            status: OrderStatus,
        ) -> Arc<Order> {
            let address = wallet.test_reveal_address();
            let order = test_order_with_payment(
                id,
                Payment::new(
                    address.address.clone(),
                    address.index,
                    Amount::from_sat(1000),
                    0,
                ),
                status,
                &router.metatron,
            );
            let tx = wallet.test_receive_unconfirmed(&address.address, Amount::from_sat(1000));
            wallet.test_confirm_tx(tx);
            order
        }

        let expired = funded_order(&router, &wallet, 0, OrderStatus::Expired);
        let cancelled = funded_order(&router, &wallet, 1, OrderStatus::Cancelled);
        let disconnected = funded_order(&router, &wallet, 2, OrderStatus::Disconnected);
        let active = funded_order(&router, &wallet, 3, OrderStatus::Active);

        let unfunded = test_order_with_payment(
            4,
            Payment::new(
                wallet.test_reveal_address().address,
                99,
                Amount::from_sat(1000),
                0,
            ),
            OrderStatus::Expired,
            &router.metatron,
        );

        let fulfilled = funded_order(&router, &wallet, 5, OrderStatus::Disconnected);
        set_delivered_work(&router.metatron, fulfilled.as_ref(), 100.0);

        add_orders(
            &router,
            [
                expired.clone(),
                cancelled.clone(),
                disconnected.clone(),
                active.clone(),
                unfunded.clone(),
                fulfilled.clone(),
            ],
        );

        router.flag_orders();

        assert!(expired.is_flagged());
        assert!(cancelled.is_flagged());
        assert!(disconnected.is_flagged());
        assert!(!active.is_flagged());
        assert!(!unfunded.is_flagged());
        assert!(!fulfilled.is_flagged());
    }

    #[test]
    fn flag_orders_keeps_flagged_order_flagged_after_condition_clears() {
        let test = test_router();
        let router = test.router.clone();
        let wallet = router.wallet.as_ref().unwrap().clone();
        let address = wallet.test_reveal_address();
        let order = test_order_with_payment(
            0,
            Payment::new(
                address.address.clone(),
                address.index,
                Amount::from_sat(1000),
                0,
            ),
            OrderStatus::Expired,
            &router.metatron,
        );
        let tx = wallet.test_receive_unconfirmed(&address.address, Amount::from_sat(1000));
        wallet.test_confirm_tx(tx);

        add_orders(&router, [order.clone()]);

        router.flag_orders();
        assert!(order.is_flagged());

        set_delivered_work(&router.metatron, order.as_ref(), 100.0);
        router.flag_orders();

        assert!(order.is_flagged());
    }

    #[test]
    fn flag_orders_does_not_reflag_cleared_order() {
        let test = test_router();
        let router = test.router.clone();
        let wallet = router.wallet.as_ref().unwrap().clone();
        let address = wallet.test_reveal_address();
        let order = test_order_with_payment(
            0,
            Payment::new(
                address.address.clone(),
                address.index,
                Amount::from_sat(1000),
                0,
            ),
            OrderStatus::Expired,
            &router.metatron,
        );
        let tx = wallet.test_receive_unconfirmed(&address.address, Amount::from_sat(1000));
        wallet.test_confirm_tx(tx);

        add_orders(&router, [order.clone()]);

        router.flag_orders();
        assert!(order.set_cleared());

        router.flag_orders();
        assert!(!order.is_flagged());
        assert!(order.is_cleared());
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
    fn allocate_id_errors_on_exhaustion() {
        let router = test_router();
        router.next_id.store(u32::MAX, Ordering::Relaxed);

        assert!(matches!(
            router.allocate_id(),
            Err(RouterError::OrderIdExhausted),
        ));
        assert_eq!(router.next_id.load(Ordering::Relaxed), u32::MAX);
    }

    #[test]
    fn retire_orders_retires_persisted_terminal_orders() {
        let router = test_router();
        let order = test_order(0, None, OrderStatus::Active, &router.metatron);
        add_orders(router.as_ref(), [order.clone()]);

        order.terminate(OrderStatus::Fulfilled);
        router.retire_orders();
        assert!(router.get_order(0).is_some(), "dirty order was retired");

        router.persist().unwrap();
        router.retire_orders();

        assert!(router.get_order(0).is_none());
        assert_eq!(router.cold_order(0).unwrap().status, OrderStatus::Fulfilled,);
        assert_eq!(
            router
                .metatron
                .store()
                .read_order(0)
                .unwrap()
                .unwrap()
                .status,
            OrderStatus::Fulfilled,
        );
    }

    #[test]
    fn retire_orders_removes_metatron_slot() {
        let router = test_router();
        let order = test_order(0, None, OrderStatus::Active, &router.metatron);
        add_orders(router.as_ref(), [order.clone()]);
        set_delivered_work(&router.metatron, &order, 100.0);

        order.terminate(OrderStatus::Fulfilled);
        router.persist().unwrap();
        router.retire_orders();

        assert_eq!(
            router.metatron.order_delivered_work(0),
            HashWork::ZERO,
            "metatron slot was not removed",
        );
        let stats = &router.cold_order(0).unwrap().stats;
        assert!(
            stats.accepted_work + stats.rejected_work > HashWork::ZERO,
            "cold entry lost the stats snapshot",
        );
    }

    #[test]
    fn retire_orders_keeps_flagged_orders_until_cleared() {
        let router = test_router();
        let order = test_order(0, None, OrderStatus::Expired, &router.metatron);
        add_orders(router.as_ref(), [order.clone()]);

        order.set_flagged();
        router.persist().unwrap();
        router.retire_orders();
        assert!(router.get_order(0).is_some());

        order.set_cleared();
        router.persist().unwrap();
        router.retire_orders();
        assert!(router.get_order(0).is_none());
        assert_eq!(router.cold_order(0).unwrap().review, Review::Cleared);
    }

    #[test]
    fn retire_orders_skips_orders_dirtied_after_persist() {
        let router = test_router();
        let order = test_order(0, None, OrderStatus::Expired, &router.metatron);
        add_orders(router.as_ref(), [order.clone()]);

        order.set_flagged();
        router.persist().unwrap();

        order.set_cleared();

        router.retire_orders();
        assert!(router.get_order(0).is_some(), "dirtied order was retired");

        router.persist().unwrap();
        router.retire_orders();

        assert!(router.get_order(0).is_none());
        assert_eq!(router.cold_order(0).unwrap().review, Review::Cleared);
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

    #[test]
    fn audit_receipts_reports_unclaimed_confirmed_funds() {
        let router = test_router();
        let wallet = router.wallet.clone().unwrap();

        assert!(router.audit_receipts().is_empty(), "wallet not synced");

        wallet.mark_synced();

        let live = wallet.test_reveal_address();
        let cold = wallet.test_reveal_address();
        let orphan = wallet.test_reveal_address();

        add_orders(
            router.as_ref(),
            [test_order_with_payment(
                0,
                Payment::new(live.address.clone(), live.index, Amount::from_sat(1000), 0),
                OrderStatus::Active,
                &router.metatron,
            )],
        );

        let cold_entry = test_order_with_payment(
            1,
            Payment::new(cold.address.clone(), cold.index, Amount::from_sat(1000), 0),
            OrderStatus::Expired,
            &router.metatron,
        )
        .to_entry();

        router.orders.write().add_cold(1, cold_entry);

        for address in [&live.address, &cold.address, &orphan.address] {
            let tx = wallet.test_receive_unconfirmed(address, Amount::from_sat(1000));
            wallet.test_confirm_tx(tx);
        }

        assert_eq!(
            router.audit_receipts(),
            vec![(orphan.index, Amount::from_sat(1000))],
        );
    }

    #[test]
    fn sweep_orphan_receipts_stores_first_seen_and_resolves() {
        let router = test_router();
        let wallet = router.wallet.clone().unwrap();

        router.sweep_orphan_receipts();
        assert!(
            router.orphan_receipts.lock().is_empty(),
            "wallet not synced"
        );

        wallet.mark_synced();

        let orphan = wallet.test_reveal_address();
        let tx = wallet.test_receive_unconfirmed(&orphan.address, Amount::from_sat(1000));
        wallet.test_confirm_tx(tx);

        router.sweep_orphan_receipts();

        let expected = OrphanReceipt {
            derivation_index: orphan.index,
            address: orphan.address.as_unchecked().clone(),
            amount: Amount::from_sat(1000),
            first_seen_height: wallet.tip(),
        };

        assert_eq!(
            router
                .orphan_receipts
                .lock()
                .values()
                .cloned()
                .collect::<Vec<_>>(),
            vec![expected.clone()],
        );

        router.sweep_orphan_receipts();

        assert_eq!(
            router.status().wallet.orphan_receipts,
            vec![expected],
            "first-seen height is stable across sweeps",
        );

        add_orders(
            router.as_ref(),
            [test_order_with_payment(
                0,
                Payment::new(
                    orphan.address.clone(),
                    orphan.index,
                    Amount::from_sat(1000),
                    0,
                ),
                OrderStatus::Active,
                &router.metatron,
            )],
        );

        router.sweep_orphan_receipts();

        assert!(router.orphan_receipts.lock().is_empty());
    }

    fn funded_bucket_order(
        id: u32,
        funded: &bdk_wallet::AddressInfo,
        amount: u64,
        metatron: &Arc<Metatron>,
    ) -> Arc<Order> {
        let order = Order::new(
            id,
            "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4.foo@bar:3333"
                .parse()
                .unwrap(),
            Some(Bucket {
                target: hash_days(100.0),
                payment: Payment::new(
                    funded.address.clone(),
                    funded.index,
                    Amount::from_sat(amount),
                    0,
                ),
            }),
            CancellationToken::new(),
            metatron.clone(),
        );

        order.force_status(OrderStatus::Expired);
        order
    }

    #[test]
    fn build_refund_constructs_unsigned_psbt() {
        let router = test_router();
        let wallet = router.wallet.clone().unwrap();
        wallet.mark_synced();

        let funded = wallet.test_reveal_address();
        let tx = wallet.test_receive_unconfirmed(&funded.address, Amount::from_sat(10_000));
        wallet.test_confirm_tx(tx);

        add_orders(
            router.as_ref(),
            [funded_bucket_order(0, &funded, 10_000, &router.metatron)],
        );

        let refund = router
            .build_refund(0, Some(FeeRate::from_sat_per_vb(1).unwrap()), None)
            .unwrap();

        assert_eq!(refund.amount, refund.psbt.unsigned_tx.output[0].value);
        assert!(refund.amount < Amount::from_sat(10_000));
        assert_eq!(refund.outpoints.len(), 1);
        assert_eq!(
            refund.destination.to_string(),
            "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4",
        );
        assert_eq!(refund.psbt.unsigned_tx.input.len(), 1);
        assert_eq!(
            refund.psbt.unsigned_tx.input[0].previous_output,
            refund.outpoints[0],
        );
        assert!(refund.psbt.inputs[0].final_script_witness.is_none());

        let cold = wallet.test_reveal_address();
        let tx = wallet.test_receive_unconfirmed(&cold.address, Amount::from_sat(2000));
        wallet.test_confirm_tx(tx);

        let order = funded_bucket_order(1, &cold, 2000, &router.metatron);
        router.orders.write().add_cold(1, order.to_entry());

        let override_destination = "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4"
            .parse::<Address<NetworkUnchecked>>()
            .unwrap()
            .require_network(Network::Bitcoin)
            .unwrap();

        let refund = router
            .build_refund(
                1,
                Some(FeeRate::from_sat_per_vb(1).unwrap()),
                Some(override_destination.clone()),
            )
            .unwrap();

        assert_eq!(refund.amount, refund.psbt.unsigned_tx.output[0].value);
        assert!(refund.amount < Amount::from_sat(2000));
        assert_eq!(refund.destination, override_destination);
    }

    #[test]
    fn build_refund_errors() {
        let fee_rate = FeeRate::from_sat_per_vb(1).unwrap();

        let router = test_router();
        let wallet = router.wallet.clone().unwrap();

        assert!(matches!(
            router.build_refund(99, Some(fee_rate), None),
            Err(RouterError::OrderNotFound { id: 99 })
        ));

        add_orders(
            router.as_ref(),
            [test_order(
                0,
                None,
                OrderStatus::Cancelled,
                &router.metatron,
            )],
        );

        assert!(matches!(
            router.build_refund(0, Some(fee_rate), None),
            Err(RouterError::NotABucketOrder { id: 0 })
        ));

        let unfunded = wallet.test_reveal_address();

        add_orders(
            router.as_ref(),
            [test_order_with_payment(
                1,
                Payment::new(
                    unfunded.address.clone(),
                    unfunded.index,
                    Amount::from_sat(1000),
                    0,
                ),
                OrderStatus::Expired,
                &router.metatron,
            )],
        );

        assert!(matches!(
            router.build_refund(1, Some(fee_rate), None),
            Err(RouterError::WalletSyncing)
        ));

        wallet.mark_synced();

        assert!(matches!(
            router.build_refund(1, Some(fee_rate), None),
            Err(RouterError::NoUnspentFunds { id: 1 })
        ));

        let funded = wallet.test_reveal_address();
        let tx = wallet.test_receive_unconfirmed(&funded.address, Amount::from_sat(1000));
        wallet.test_confirm_tx(tx);

        add_orders(
            router.as_ref(),
            [test_order_with_payment(
                2,
                Payment::new(
                    funded.address.clone(),
                    funded.index,
                    Amount::from_sat(1000),
                    0,
                ),
                OrderStatus::Expired,
                &router.metatron,
            )],
        );

        assert!(matches!(
            router.build_refund(2, Some(fee_rate), None),
            Err(RouterError::InvalidRefundDestination { id: 2 })
        ));

        let router = test_router_with_wallet(None);

        add_orders(
            router.as_ref(),
            [test_order_with_payment(
                0,
                Payment::new(test_address(), 0, Amount::from_sat(1000), 0),
                OrderStatus::Expired,
                &router.metatron,
            )],
        );

        assert!(matches!(
            router.build_refund(0, Some(fee_rate), None),
            Err(RouterError::WalletRequired)
        ));
    }

    fn regtest_wallet_router(directory: &tempfile::TempDir) -> (Arc<Router>, Arc<Wallet>) {
        let store =
            Arc::new(Store::open(&directory.path().join("test.redb"), Chain::Regtest).unwrap());
        let wallet = Arc::new(
            Wallet::open(
                wallet_settings_with_descriptors(directory.path()),
                store.clone(),
            )
            .unwrap(),
        );
        let router = Arc::new(Router::new(
            test_settings(directory.path()),
            Arc::new(Metatron::test_with_store(store)),
            Some(wallet.clone()),
            TaskTracker::new(),
            CancellationToken::new(),
            HashValue::from_sats(1),
        ));
        (router, wallet)
    }

    #[test]
    fn flag_orders_rehydrates_and_flags_cold_funded_expired_order() {
        let directory = tempfile::tempdir().unwrap();
        let (router, wallet) = regtest_wallet_router(&directory);

        let address = wallet.test_reveal_address();
        let order = test_order_with_payment(
            0,
            Payment::new(
                address.address.clone(),
                address.index,
                Amount::from_sat(1000),
                0,
            ),
            OrderStatus::Pending,
            &router.metatron,
        );
        let tx = wallet.test_receive_unconfirmed(&address.address, Amount::from_sat(1000));
        wallet.test_confirm_tx(tx);

        add_orders(router.as_ref(), [order.clone()]);
        order.terminate(OrderStatus::Expired);
        router.persist().unwrap();
        router.retire_orders();
        assert!(router.get_order(0).is_none());
        assert!(router.cold_order(0).is_some());

        router.flag_orders();

        let rehydrated = router.get_order(0).unwrap();
        assert_eq!(rehydrated.status(), OrderStatus::Expired);
        assert!(rehydrated.is_flagged());
        assert!(router.cold_order(0).is_none());
    }

    #[test]
    fn flag_orders_does_not_rehydrate_accounted_orders() {
        let test = test_router();
        let router = test.router.clone();
        let wallet = router.wallet.as_ref().unwrap().clone();

        let address = wallet.test_reveal_address();
        let order = test_order_with_payment(
            0,
            Payment::new(
                address.address.clone(),
                address.index,
                Amount::from_sat(1000),
                0,
            ),
            OrderStatus::Pending,
            &router.metatron,
        );
        let tx = wallet.test_receive_unconfirmed(&address.address, Amount::from_sat(1000));
        wallet.test_confirm_tx(tx);

        add_orders(router.as_ref(), [order.clone()]);
        order.terminate(OrderStatus::Fulfilled);
        router.persist().unwrap();
        router.retire_orders();
        assert!(router.get_order(0).is_none());

        router.flag_orders();

        assert!(router.get_order(0).is_none());
        assert!(router.cold_order(0).is_some());
    }

    #[test]
    fn flag_orders_marks_active_order_whose_payment_vanished() {
        let router = test_router();
        let order = test_order_with_payment(
            0,
            Payment::new(test_address(), 0, Amount::from_sat(1000), 0),
            OrderStatus::Active,
            &router.metatron,
        );
        add_orders(router.as_ref(), [order.clone()]);

        router.flag_orders();
        assert!(!order.is_flagged(), "flag check ran while wallet unsynced");

        router.wallet.as_ref().unwrap().mark_synced();
        router.flag_orders();

        assert!(order.is_flagged());
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
    fn routable_filters_disconnected_upstreams() {
        let (metatron, _dir) = Metatron::test();
        let metatron = Arc::new(metatron);
        let mut orders = Orders::new();
        let connected = test_order(0, None, OrderStatus::Active, &metatron);
        let disconnected = test_order(1, None, OrderStatus::Active, &metatron);
        disconnected.upstream().unwrap().set_connected(false);
        let pending = test_order(2, None, OrderStatus::Pending, &metatron);
        let in_mempool = test_order(3, Some(hash_days(100.0)), OrderStatus::InMempool, &metatron);

        orders.add(connected);
        orders.add(disconnected);
        orders.add(pending);
        orders.add(in_mempool);

        assert_eq!(ids(orders.routable()), vec![0]);
    }

    #[test]
    fn active_route_reports_missing_upstream_after_selection() {
        let router = test_router();
        let order = test_order(0, None, OrderStatus::Active, &router.metatron);
        add_orders(router.as_ref(), [order]);

        let selected = router.next_order(addr(1), &blank()).unwrap();
        *selected.upstream.lock() = None;

        assert!(matches!(
            Router::active_route(&selected),
            Err(RouterError::MissingActiveUpstream { id: 0 }),
        ));

        selected.release_placement(&addr(1));
    }

    #[test]
    fn active_route_reports_missing_allocator() {
        let router = test_router();
        let order = Order::new(
            0,
            test_upstream_target(),
            None,
            CancellationToken::new(),
            router.metatron.clone(),
        );
        order.force_status(OrderStatus::Active);
        *order.upstream.lock() = Some(Upstream::test(0, router.metatron.clone()));

        assert!(matches!(
            Router::active_route(&order),
            Err(RouterError::MissingActiveAllocator { id: 0 }),
        ));
    }

    #[test]
    fn orders_active_returns_only_active_status() {
        let (metatron, _dir) = Metatron::test();
        let metatron = Arc::new(metatron);
        let mut orders = Orders::new();
        orders.add(test_order(0, None, OrderStatus::Pending, &metatron));
        orders.add(test_order(
            3,
            Some(hash_days(100.0)),
            OrderStatus::InMempool,
            &metatron,
        ));
        orders.add(test_order(
            1,
            Some(hash_days(100.0)),
            OrderStatus::Active,
            &metatron,
        ));
        orders.add(test_order(2, None, OrderStatus::Active, &metatron));

        assert_eq!(ids(orders.active()), vec![1, 2]);
    }

    #[test]
    fn orders_get() {
        let (metatron, _dir) = Metatron::test();
        let metatron = Arc::new(metatron);
        let mut orders = Orders::new();
        orders.add(test_order(0, None, OrderStatus::Pending, &metatron));

        assert_eq!(orders.get(0).unwrap().id, 0);
        assert!(orders.get(1).is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn wait_for_payment_expires_instead_of_activating_after_extended_timeout() {
        let router = test_router();
        let router = router.router.clone();
        let wallet = router.wallet.as_ref().unwrap();
        let address = wallet.test_reveal_address();
        let amount = Amount::from_sat(1000);
        let order = test_order_with_payment(
            0,
            Payment::new(address.address.clone(), address.index, amount, 0),
            OrderStatus::Pending,
            &router.metatron,
        );

        wallet.test_receive_unconfirmed(&address.address, amount);
        wallet.test_advance_tip_to(EXTENDED_PAYMENT_TIMEOUT);

        assert!(
            !router
                .wait_for_payment(&order, payment(&order))
                .await
                .unwrap()
        );
        assert_eq!(order.status(), OrderStatus::Expired);
        assert!(order.cancel.is_cancelled());
    }

    #[tokio::test(start_paused = true)]
    async fn wait_for_payment_partial_payment_does_not_extend_deadline() {
        let router = test_router();
        let router = router.router.clone();
        let wallet = router.wallet.as_ref().unwrap();
        let address = wallet.test_reveal_address();
        let amount = Amount::from_sat(1000);
        let order = test_order_with_payment(
            0,
            Payment::new(address.address.clone(), address.index, amount, 0),
            OrderStatus::Pending,
            &router.metatron,
        );

        wallet.test_receive_unconfirmed(&address.address, Amount::from_sat(500));
        wallet.test_advance_tip_to(PAYMENT_TIMEOUT);

        assert!(
            !router
                .wait_for_payment(&order, payment(&order))
                .await
                .unwrap()
        );
        assert_eq!(order.status(), OrderStatus::Expired);
        assert!(order.cancel.is_cancelled());
    }

    #[tokio::test(start_paused = true)]
    async fn wait_for_payment_extends_deadline_while_payment_in_mempool() {
        let router = test_router();
        let router = router.router.clone();
        let wallet = router.wallet.as_ref().unwrap();
        let address = wallet.test_reveal_address();
        let amount = Amount::from_sat(1000);
        let order = test_order_with_payment(
            0,
            Payment::new(address.address.clone(), address.index, amount, 0),
            OrderStatus::Pending,
            &router.metatron,
        );

        wallet.test_receive_unconfirmed(&address.address, amount);
        wallet.test_advance_tip_to(PAYMENT_TIMEOUT);

        let monitored = order.clone();
        let waiter_router = router.clone();
        let waiter = tokio::spawn(async move {
            waiter_router
                .wait_for_payment(&monitored, payment(&monitored))
                .await
                .unwrap()
        });

        tokio::task::yield_now().await;
        assert_eq!(order.status(), OrderStatus::InMempool);
        assert!(!waiter.is_finished());

        order.cancel.cancel();
        assert!(!waiter.await.unwrap());
    }

    #[tokio::test(start_paused = true)]
    async fn wait_for_payment_expires_promptly_after_payment_disappears() {
        let router = test_router();
        let router = router.router.clone();
        let wallet = router.wallet.as_ref().unwrap().clone();
        let address = wallet.test_reveal_address();
        let amount = Amount::from_sat(1000);
        let order = test_order_with_payment(
            0,
            Payment::new(address.address.clone(), address.index, amount, 0),
            OrderStatus::Pending,
            &router.metatron,
        );

        let tx = wallet.test_receive_unconfirmed(&address.address, amount);

        let monitored = order.clone();
        let waiter_router = router.clone();
        let waiter = tokio::spawn(async move {
            waiter_router
                .wait_for_payment(&monitored, payment(&monitored))
                .await
                .unwrap()
        });

        tokio::task::yield_now().await;
        assert_eq!(order.status(), OrderStatus::InMempool);

        wallet.test_evict_tx(&tx);
        wallet.test_advance_tip_to(PAYMENT_TIMEOUT);
        wallet.mark_synced();

        assert!(!waiter.await.unwrap());
        assert_eq!(order.status(), OrderStatus::Expired);
    }

    #[tokio::test(start_paused = true)]
    async fn wait_for_payment_requires_wallet_for_bucket_order() {
        let router = test_router_with_wallet(None);
        let order = test_order_with_payment(
            0,
            Payment::new(test_address(), 0, Amount::from_sat(1000), 0),
            OrderStatus::Pending,
            &router.metatron,
        );

        assert!(matches!(
            router.wait_for_payment(&order, payment(&order)).await,
            Err(RouterError::WalletRequired),
        ));
        assert_eq!(order.status(), OrderStatus::Pending);
    }

    #[tokio::test(start_paused = true)]
    async fn wait_for_payment_marks_pending_order_in_mempool_before_confirmation() {
        let router = test_router();
        let router = router.router.clone();
        let wallet = router.wallet.as_ref().unwrap().clone();
        let address = wallet.test_reveal_address();
        let amount = Amount::from_sat(1000);
        let order = test_order_with_payment(
            0,
            Payment::new(address.address.clone(), address.index, amount, wallet.tip()),
            OrderStatus::Pending,
            &router.metatron,
        );

        wallet.test_receive_unconfirmed(&address.address, amount);

        let monitored = order.clone();
        let waiter_router = router.clone();
        let waiter = tokio::spawn(async move {
            waiter_router
                .wait_for_payment(&monitored, payment(&monitored))
                .await
                .unwrap()
        });

        tokio::task::yield_now().await;
        assert_eq!(order.status(), OrderStatus::InMempool);

        order.cancel.cancel();
        assert!(!waiter.await.unwrap());
    }

    #[tokio::test(start_paused = true)]
    async fn wait_for_payment_returns_in_mempool_to_pending_when_total_is_below_amount() {
        let router = test_router();
        let router = router.router.clone();
        let wallet = router.wallet.as_ref().unwrap().clone();
        let address = wallet.test_reveal_address();
        let amount = Amount::from_sat(1000);
        let order = test_order_with_payment(
            0,
            Payment::new(address.address.clone(), address.index, amount, wallet.tip()),
            OrderStatus::InMempool,
            &router.metatron,
        );
        let monitored = order.clone();
        let waiter_router = router.clone();
        let waiter = tokio::spawn(async move {
            waiter_router
                .wait_for_payment(&monitored, payment(&monitored))
                .await
                .unwrap()
        });

        tokio::task::yield_now().await;
        assert_eq!(order.status(), OrderStatus::Pending);

        order.cancel.cancel();
        assert!(!waiter.await.unwrap());
    }

    #[tokio::test(start_paused = true)]
    async fn wait_for_payment_activates_when_confirmed_at_timeout() {
        let router = test_router();
        let router = router.router.clone();
        let wallet = router.wallet.as_ref().unwrap().clone();
        let address = wallet.test_reveal_address();
        let amount = Amount::from_sat(1000);
        let order = test_order_with_payment(
            0,
            Payment::new(address.address.clone(), address.index, amount, wallet.tip()),
            OrderStatus::Pending,
            &router.metatron,
        );

        let tx = wallet.test_receive_unconfirmed(&address.address, amount);

        let monitored = order.clone();
        let waiter_router = router.clone();
        let waiter = tokio::spawn(async move {
            waiter_router
                .wait_for_payment(&monitored, payment(&monitored))
                .await
                .unwrap()
        });

        tokio::task::yield_now().await;
        assert_eq!(order.status(), OrderStatus::InMempool);

        wallet.test_confirm_tx(tx);
        wallet.test_advance_tip_to(PAYMENT_TIMEOUT);
        wallet.mark_synced();

        assert!(waiter.await.unwrap());
        assert_eq!(order.status(), OrderStatus::InMempool);
        assert!(!order.cancel.is_cancelled());
    }

    #[tokio::test(start_paused = true)]
    async fn wait_for_payment_expires_when_payment_confirms_after_extended_timeout() {
        let router = test_router();
        let router = router.router.clone();
        let wallet = router.wallet.as_ref().unwrap().clone();
        let address = wallet.test_reveal_address();
        let amount = Amount::from_sat(1000);
        let order = test_order_with_payment(
            0,
            Payment::new(address.address.clone(), address.index, amount, wallet.tip()),
            OrderStatus::Pending,
            &router.metatron,
        );

        let tx = wallet.test_receive_unconfirmed(&address.address, amount);
        wallet.test_advance_tip_to(EXTENDED_PAYMENT_TIMEOUT);
        wallet.test_confirm_tx(tx);

        assert!(
            !router
                .wait_for_payment(&order, payment(&order))
                .await
                .unwrap()
        );
        assert_eq!(order.status(), OrderStatus::Expired);
        assert!(order.cancel.is_cancelled());
    }

    #[tokio::test(start_paused = true)]
    async fn wait_for_payment_activates_when_confirmed_within_extended_deadline() {
        let router = test_router();
        let router = router.router.clone();
        let wallet = router.wallet.as_ref().unwrap().clone();
        let address = wallet.test_reveal_address();
        let amount = Amount::from_sat(1000);
        let order = test_order_with_payment(
            0,
            Payment::new(address.address.clone(), address.index, amount, wallet.tip()),
            OrderStatus::Pending,
            &router.metatron,
        );

        let tx = wallet.test_receive_unconfirmed(&address.address, amount);

        let monitored = order.clone();
        let waiter_router = router.clone();
        let waiter = tokio::spawn(async move {
            waiter_router
                .wait_for_payment(&monitored, payment(&monitored))
                .await
                .unwrap()
        });

        tokio::task::yield_now().await;
        assert_eq!(order.status(), OrderStatus::InMempool);

        wallet.test_advance_tip_to(PAYMENT_TIMEOUT + 10);
        wallet.test_confirm_tx(tx);
        wallet.mark_synced();

        assert!(waiter.await.unwrap());
        assert!(!order.cancel.is_cancelled());
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

    #[test]
    fn restore_skips_terminal_orders_but_derives_next_id() {
        let directory = tempfile::tempdir().unwrap();
        let store =
            Arc::new(Store::open(&directory.path().join("test.redb"), Chain::Regtest).unwrap());
        let metatron = Arc::new(Metatron::test_with_store(store.clone()));
        let fulfilled = test_order(3, None, OrderStatus::Fulfilled, &metatron);
        let flagged = test_order(4, None, OrderStatus::Expired, &metatron);
        flagged.set_flagged();

        let txn = store.begin().unwrap();
        txn.insert_order(fulfilled.id, &fulfilled.to_entry())
            .unwrap();
        txn.insert_order(flagged.id, &flagged.to_entry()).unwrap();
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

        assert!(router.get_order(3).is_none());
        assert_eq!(
            router.cold_order(3).unwrap().status,
            OrderStatus::Fulfilled,
            "clean terminal order should seed the cold tier"
        );
        assert_eq!(
            router.get_order(4).unwrap().review(),
            Review::Flagged,
            "flagged terminal order should be restored for review"
        );
        assert_eq!(router.next_id.load(Ordering::Relaxed), 5);
    }

    #[test]
    fn restore_seeds_cold_index_for_terminal_bucket_orders() {
        let directory = tempfile::tempdir().unwrap();
        let store =
            Arc::new(Store::open(&directory.path().join("test.redb"), Chain::Regtest).unwrap());
        let metatron = Arc::new(Metatron::test_with_store(store.clone()));
        let expired = test_order(3, Some(hash_days(100.0)), OrderStatus::Expired, &metatron);
        let index = expired.bucket.as_ref().unwrap().payment.derivation_index;

        let txn = store.begin().unwrap();
        txn.insert_order(expired.id, &expired.to_entry()).unwrap();
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

        assert!(router.get_order(3).is_none());
        assert_eq!(
            router.orders.read().cold_id(index),
            Some(3),
            "cold bucket order should be indexed by derivation index"
        );
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
        assert_eq!(router.next_id.load(Ordering::Relaxed), 5);
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
        assert_eq!(router.next_id.load(Ordering::Relaxed), 5);

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

    #[test]
    fn routable_includes_unfulfilled_bucket() {
        let (metatron, _dir) = Metatron::test();
        let metatron = Arc::new(metatron);
        let mut orders = Orders::new();
        let bucket = test_order(0, Some(hash_days(100.0)), OrderStatus::Active, &metatron);

        let session = metatron.new_session(test_authorization("deadbeef", "foo"), 0);
        session.record_accepted(Difficulty::from(1000.0), Difficulty::from(1000.0));
        bucket.add_session(session, CancellationToken::new(), addr(1));

        orders.add(bucket);

        assert_eq!(ids(orders.routable()), vec![0]);
    }

    #[test]
    fn routable_excludes_fulfilled_bucket() {
        let (metatron, _dir) = Metatron::test();
        let metatron = Arc::new(metatron);
        let mut orders = Orders::new();
        let bucket = test_order(0, Some(hash_days(100.0)), OrderStatus::Active, &metatron);
        set_delivered_work(&metatron, &bucket, 100.0);

        orders.add(bucket);

        assert!(orders.routable().is_empty());
    }

    #[test]
    fn work_sums() {
        let (metatron, _dir) = Metatron::test();
        let metatron = Arc::new(metatron);
        let mut orders = Orders::new();
        orders.add(test_order(
            0,
            Some(hash_days(100.0)),
            OrderStatus::Active,
            &metatron,
        ));
        orders.add(test_order(
            1,
            Some(hash_days(200.0)),
            OrderStatus::InMempool,
            &metatron,
        ));
        orders.add(test_order(
            2,
            Some(hash_days(25.0)),
            OrderStatus::Pending,
            &metatron,
        ));
        orders.add(test_order(
            3,
            Some(hash_days(50.0)),
            OrderStatus::Cancelled,
            &metatron,
        ));
        orders.add(test_order(
            4,
            Some(hash_days(50.0)),
            OrderStatus::Fulfilled,
            &metatron,
        ));
        orders.add(test_order(5, None, OrderStatus::Active, &metatron));

        assert_eq!(orders.active_work().as_f64(), 100.0);
        assert_eq!(orders.in_flight_work().as_f64(), 200.0);
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
