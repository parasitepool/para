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

    pub(crate) fn active_paid(&self) -> Vec<Arc<Order>> {
        self.inner
            .values()
            .filter(|order| order.is_active() && !order.is_default())
            .cloned()
            .collect()
    }

    pub(crate) fn active_default(&self) -> Vec<Arc<Order>> {
        self.inner
            .values()
            .filter(|order| order.is_active() && order.is_default())
            .cloned()
            .collect()
    }

    pub(crate) fn get(&self, id: u32) -> Option<Arc<Order>> {
        self.inner.get(&id).cloned()
    }

    pub(crate) fn all(&self) -> Vec<Arc<Order>> {
        self.inner.values().cloned().collect()
    }

    #[cfg(test)]
    pub(super) fn all_len(&self) -> usize {
        self.inner.len()
    }

    #[cfg(test)]
    pub(super) fn active_len(&self) -> usize {
        self.inner
            .values()
            .filter(|order| order.is_active())
            .count()
    }

    #[cfg(test)]
    pub(super) fn contains(&self, id: u32) -> bool {
        self.inner.contains_key(&id)
    }

    #[cfg(test)]
    pub(super) fn active_id(&self, idx: usize) -> u32 {
        let active_paid = self.active_paid();
        let active_default = self.active_default();

        active_paid
            .iter()
            .chain(&active_default)
            .nth(idx)
            .unwrap()
            .id
    }
}
