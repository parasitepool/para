use {super::*, bdk_wallet::ChangeSet};

#[derive(Clone, Copy, Default)]
pub(crate) struct ColdStats {
    pub(crate) accepted_shares: u64,
    pub(crate) rejected_shares: u64,
    pub(crate) accepted_work: HashWork,
    pub(crate) rejected_work: HashWork,
    pub(crate) best_share: Option<Difficulty>,
    pub(crate) last_share_secs: Option<f64>,
}

#[derive(Default)]
pub(crate) struct ColdTotals {
    pub(crate) accepted_shares: u64,
    pub(crate) rejected_shares: u64,
    pub(crate) accepted_work: HashWork,
    pub(crate) rejected_work: HashWork,
    pub(crate) best_share: Option<Difficulty>,
    pub(crate) last_share_secs: Option<f64>,
    pub(crate) addresses: HashSet<Address<NetworkUnchecked>>,
}

pub(crate) struct StatusSnapshot {
    pub(crate) used: HashDays,
    pub(crate) live: Vec<Arc<Order>>,
    pub(crate) cold: ColdStats,
    pub(crate) cold_count: usize,
    pub(crate) total_users: usize,
}

impl ColdTotals {
    fn stats(&self) -> ColdStats {
        ColdStats {
            accepted_shares: self.accepted_shares,
            rejected_shares: self.rejected_shares,
            accepted_work: self.accepted_work,
            rejected_work: self.rejected_work,
            best_share: self.best_share,
            last_share_secs: self.last_share_secs,
        }
    }

    fn absorb(&mut self, entry: &entry::OrderEntry) {
        let stats = &entry.stats;

        self.accepted_shares += stats.accepted_shares;
        self.rejected_shares += stats.rejected_shares;
        self.accepted_work += stats.accepted_work;
        self.rejected_work += stats.rejected_work;

        if stats
            .best_share
            .is_some_and(|best| self.best_share.is_none_or(|current| best > current))
        {
            self.best_share = stats.best_share;
        }

        if let Some(last) = stats.last_share_secs.filter(|secs| secs.is_finite())
            && self.last_share_secs.is_none_or(|current| last > current)
        {
            self.last_share_secs = Some(last);
        }

        self.addresses
            .insert(entry.upstream_target.username().address().clone());
    }
}

pub(crate) struct Orders {
    live: BTreeMap<u32, Arc<Order>>,
    cold: BTreeMap<u32, entry::OrderEntry>,
    cold_by_index: HashMap<u32, u32>,
    cold_totals: ColdTotals,
}

pub(crate) struct OrderBook {
    orders: RwLock<Orders>,
    next_id: AtomicU32,
    logged_orphan_receipts: Mutex<HashSet<u32>>,
    settings: Arc<Settings>,
    metatron: Arc<Metatron>,
    cancel: CancellationToken,
}

impl Orders {
    pub(crate) fn new() -> Self {
        Self {
            live: BTreeMap::new(),
            cold: BTreeMap::new(),
            cold_by_index: HashMap::new(),
            cold_totals: ColdTotals::default(),
        }
    }

    pub(crate) fn add(&mut self, order: Arc<Order>) {
        self.live.insert(order.id, order);
    }

    fn add_bucket_order(
        &mut self,
        capacity: HashDays,
        target: HashDays,
        next_id: &AtomicU32,
        make_order: impl FnOnce(u32) -> RouterResult<Arc<Order>>,
    ) -> RouterResult<Arc<Order>> {
        let used = self.used_work();
        if used.as_f64() + target.as_f64() > capacity.as_f64() {
            let available = HashDays::from_raw((capacity.as_f64() - used.as_f64()).max(0.0));
            return Err(RouterError::InsufficientCapacity {
                requested: target,
                available,
            });
        }

        let id = next_id
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| RouterError::OrderIdExhausted)?;

        let order = make_order(id)?;
        self.add(order.clone());

        Ok(order)
    }

    pub(crate) fn add_cold(&mut self, id: u32, entry: entry::OrderEntry) {
        debug_assert!(!self.cold.contains_key(&id));

        if let Some(bucket) = &entry.bucket {
            self.cold_by_index.insert(bucket.derivation_index, id);
        }

        self.cold_totals.absorb(&entry);

        self.cold.insert(id, entry);
    }

    pub(crate) fn remove_cold(&mut self, id: u32) {
        let Some(entry) = self.cold.remove(&id) else {
            return;
        };

        if let Some(bucket) = &entry.bucket {
            self.cold_by_index.remove(&bucket.derivation_index);
        }

        let mut totals = ColdTotals::default();

        for entry in self.cold.values() {
            totals.absorb(entry);
        }

        self.cold_totals = totals;
    }

    pub(crate) fn cold_totals(&self) -> &ColdTotals {
        &self.cold_totals
    }

    pub(crate) fn cold_count(&self) -> usize {
        self.cold.len()
    }

    pub(crate) fn retire(&mut self, order: &Order) {
        self.live.remove(&order.id);
        self.add_cold(order.id, order.to_entry());
    }

    pub(crate) fn cold_id(&self, derivation_index: u32) -> Option<u32> {
        self.cold_by_index.get(&derivation_index).copied()
    }

    pub(crate) fn cold_entry(&self, id: u32) -> Option<&entry::OrderEntry> {
        self.cold.get(&id)
    }

    pub(crate) fn cold_entries(&self) -> impl Iterator<Item = (u32, &entry::OrderEntry)> {
        self.cold.iter().map(|(id, entry)| (*id, entry))
    }

    pub(crate) fn get(&self, id: u32) -> Option<Arc<Order>> {
        self.live.get(&id).cloned()
    }

    pub(crate) fn live(&self) -> Vec<Arc<Order>> {
        self.live.values().cloned().collect()
    }

    pub(crate) fn active(&self) -> Vec<Arc<Order>> {
        self.live
            .values()
            .filter(|order| order.status() == OrderStatus::Active)
            .cloned()
            .collect()
    }

    pub(crate) fn in_flight_work(&self) -> HashDays {
        self.work_with_status(OrderStatus::InMempool)
    }

    pub(crate) fn active_work(&self) -> HashDays {
        self.work_with_status(OrderStatus::Active)
    }

    pub(crate) fn used_work(&self) -> HashDays {
        HashDays::from_raw(self.in_flight_work().as_f64() + self.active_work().as_f64())
    }

    fn work_with_status(&self, status: OrderStatus) -> HashDays {
        self.live
            .values()
            .filter(|order| order.status() == status)
            .filter_map(|order| order.bucket.as_ref())
            .map(|bucket| bucket.target)
            .sum()
    }

    pub(crate) fn routable(&self) -> Vec<Arc<Order>> {
        self.live
            .values()
            .filter(|order| {
                order.status() == OrderStatus::Active
                    && order.has_connected_upstream()
                    && (order.is_sink() || !order.is_fulfilled())
            })
            .cloned()
            .collect()
    }
}

impl OrderBook {
    pub(crate) fn new(
        settings: Arc<Settings>,
        metatron: Arc<Metatron>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            orders: RwLock::new(Orders::new()),
            next_id: AtomicU32::new(0),
            logged_orphan_receipts: Mutex::new(HashSet::new()),
            settings,
            metatron,
            cancel,
        }
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

    pub(crate) fn status_snapshot(&self) -> StatusSnapshot {
        let orders = self.orders.read();
        let live = orders.live();
        let cold = orders.cold_totals();
        let live_addresses = live
            .iter()
            .map(|order| order.upstream_target.username().address())
            .collect::<HashSet<_>>();
        let mut total_users = cold.addresses.len();

        for address in live_addresses {
            if !cold.addresses.contains(address) {
                total_users += 1;
            }
        }

        StatusSnapshot {
            used: orders.used_work(),
            live,
            cold: cold.stats(),
            cold_count: orders.cold_count(),
            total_users,
        }
    }

    pub(crate) fn live_orders(&self) -> Vec<Arc<Order>> {
        self.orders.read().live()
    }

    pub(crate) fn active(&self) -> Vec<Arc<Order>> {
        self.orders.read().active()
    }

    pub(crate) fn get_order(&self, id: u32) -> Option<Arc<Order>> {
        self.orders.read().get(id)
    }

    pub(crate) fn cold_order(&self, id: u32) -> Option<entry::OrderEntry> {
        self.orders.read().cold_entry(id).cloned()
    }

    pub(crate) fn routable(&self) -> Vec<Arc<Order>> {
        self.orders.read().routable()
    }

    pub(crate) fn add(&self, order: Arc<Order>) {
        self.orders.write().add(order);
    }

    pub(crate) fn add_cold(&self, id: u32, entry: entry::OrderEntry) {
        self.orders.write().add_cold(id, entry);
    }

    pub(crate) fn set_next_id(&self, next_id: u32) {
        self.next_id.store(next_id, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn next_id(&self) -> u32 {
        self.next_id.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn orders(&self) -> &RwLock<Orders> {
        &self.orders
    }

    #[cfg(test)]
    pub(crate) fn cold_count(&self) -> usize {
        self.orders.read().cold_count()
    }

    pub(crate) fn restore(
        &self,
        sink_orders: &[UpstreamTarget],
        execute: &dyn Fn(Arc<Order>),
    ) -> Result {
        let entries = self.metatron.store().read_orders()?;
        let mut next_id = 0u32;

        for (id, entry) in entries {
            let candidate = id
                .checked_add(1)
                .with_context(|| format!("persisted order id {id} exhausts u32 order ids"))?;

            next_id = next_id.max(candidate);

            if entry.status.is_terminal() && entry.review != Review::Flagged {
                debug!("Not restoring terminal order {id}");
                self.add_cold(id, entry);
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

            execute(order);
        }

        self.set_next_id(next_id);

        Ok(())
    }

    pub(crate) fn allocate_id(&self) -> RouterResult<u32> {
        self.next_id
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| RouterError::OrderIdExhausted)
    }

    pub(crate) fn add_bucket_order(
        &self,
        capacity: HashDays,
        target: HashDays,
        make_order: impl FnOnce(u32) -> RouterResult<Arc<Order>>,
    ) -> RouterResult<Arc<Order>> {
        self.orders
            .write()
            .add_bucket_order(capacity, target, &self.next_id, make_order)
    }

    pub(crate) fn flag_orders(&self, wallet: &Wallet) {
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

    pub(crate) fn audit_receipts(&self, wallet: &Wallet) -> Vec<(u32, Amount)> {
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

    pub(crate) fn sweep_orphan_receipts(&self, wallet: &Wallet) {
        let mut logged = self.logged_orphan_receipts.lock();

        for (index, amount) in self.audit_receipts(wallet) {
            if logged.insert(index) {
                warn!("Received {amount} at derivation index {index} with no matching order");
            }
        }
    }

    pub(crate) fn persist(&self, wallet: Option<&Wallet>) -> Result {
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

        let result = if let Some(wallet) = wallet {
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
}

#[cfg(test)]
mod tests {
    use {super::*, crate::router::testkit::*};

    #[test]
    fn status_snapshot_includes_live_and_cold_orders() {
        let router = test_router();
        let live = test_order(
            0,
            Some(hash_days(100.0)),
            OrderStatus::Active,
            &router.metatron,
        );
        let distinct = Order::new(
            2,
            "tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx.foo@bar:3333"
                .parse()
                .unwrap(),
            None,
            CancellationToken::new(),
            router.metatron.clone(),
        );

        router
            .metatron
            .record_order_accepted(1, Difficulty::from(1.0), Difficulty::from(1.0));

        let cold = test_order(1, None, OrderStatus::Expired, &router.metatron).to_entry();

        router.book.add(live);
        router.book.add(distinct);
        router.book.add_cold(1, cold);

        let snapshot = router.book.status_snapshot();

        assert_eq!(snapshot.live.len(), 2);
        assert_eq!(snapshot.live[0].id, 0);
        assert_eq!(snapshot.live[1].id, 2);
        assert_eq!(snapshot.used.as_f64(), 100.0);
        assert_eq!(snapshot.cold_count, 1);
        assert_eq!(snapshot.cold.accepted_shares, 1);
        assert_eq!(snapshot.total_users, 2);
        assert_eq!(router.status().upstream.totals.users, 2);
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

        let mut orders = router.book.orders().write();

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

        router.book.add_cold(1, cold_entry);

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
        assert_eq!(router.book.cold_count(), 0);
        assert!(router.book.get_order(1).is_some());
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
    fn allocate_id_errors_on_exhaustion() {
        let router = test_router();
        router.book.set_next_id(u32::MAX);

        assert!(matches!(
            router.allocate_id(),
            Err(RouterError::OrderIdExhausted),
        ));
        assert_eq!(router.book.next_id(), u32::MAX);
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

        router.book.add_cold(1, cold_entry);

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
        assert_eq!(router.book.next_id(), 5);
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
            router.book.orders().read().cold_id(index),
            Some(3),
            "cold bucket order should be indexed by derivation index"
        );
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
}
