use super::*;

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

impl ColdTotals {
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
