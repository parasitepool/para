use super::*;

pub(crate) struct Orders {
    all: BTreeMap<u32, Arc<Order>>,
    active: Vec<Arc<Order>>,
}

impl Orders {
    pub(crate) fn new() -> Self {
        Self {
            all: BTreeMap::new(),
            active: Vec::new(),
        }
    }

    pub(crate) fn add(&mut self, order: Arc<Order>) {
        self.all.insert(order.id, order);
    }

    pub(crate) fn activate(&mut self, id: u32) {
        if let Some(order) = self.all.get(&id) {
            self.active.push(order.clone());
        }
    }

    pub(crate) fn deactivate(&mut self, id: u32) {
        self.active.retain(|order| order.id != id);
    }

    pub(crate) fn get(&self, id: u32) -> Option<Arc<Order>> {
        self.all.get(&id).cloned()
    }

    pub(crate) fn all(&self) -> Vec<Arc<Order>> {
        self.all.values().cloned().collect()
    }

    pub(crate) fn match_round_robin(&self, counter: u64) -> Option<Arc<Order>> {
        if self.active.is_empty() {
            return None;
        }

        let idx = counter as usize % self.active.len();

        Some(self.active[idx].clone())
    }

    #[cfg(test)]
    pub(super) fn all_len(&self) -> usize {
        self.all.len()
    }

    #[cfg(test)]
    pub(super) fn active_len(&self) -> usize {
        self.active.len()
    }

    #[cfg(test)]
    pub(super) fn contains(&self, id: u32) -> bool {
        self.all.contains_key(&id)
    }

    #[cfg(test)]
    pub(super) fn active_id(&self, idx: usize) -> u32 {
        self.active[idx].id
    }
}
