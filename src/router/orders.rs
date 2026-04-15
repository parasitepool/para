use super::*;

pub(crate) struct Orders {
    inner: BTreeMap<u32, Arc<Order>>,
}

impl Orders {
    pub(crate) fn new() -> Self {
        Self {
            inner: BTreeMap::new(),
        }
    }

    pub(crate) fn add(&mut self, order: Arc<Order>) {
        self.inner.insert(order.id, order);
    }

    pub(crate) fn get(&self, id: u32) -> Option<Arc<Order>> {
        self.inner.get(&id).cloned()
    }

    pub(crate) fn all(&self) -> Vec<Arc<Order>> {
        self.inner.values().cloned().collect()
    }

    pub(crate) fn active(&self) -> Vec<Arc<Order>> {
        self.inner
            .values()
            .filter(|order| order.status() == OrderStatus::Active)
            .cloned()
            .collect()
    }

    pub(crate) fn routable(&self, now: Instant) -> Vec<Arc<Order>> {
        self.inner
            .values()
            .filter(|order| {
                order.status() == OrderStatus::Active && (order.is_sink() || order.is_starving(now))
            })
            .cloned()
            .collect()
    }
}
