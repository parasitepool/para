use super::*;

pub(crate) struct Orders {
    all: BTreeMap<u32, Arc<Order>>,
    active_paid: Vec<Arc<Order>>,
    active_default: Vec<Arc<Order>>,
}

impl Orders {
    pub(crate) fn new() -> Self {
        Self {
            all: BTreeMap::new(),
            active_paid: Vec::new(),
            active_default: Vec::new(),
        }
    }

    pub(crate) fn add(&mut self, order: Arc<Order>) {
        self.all.insert(order.id, order);
    }

    pub(crate) fn activate(&mut self, id: u32) {
        if let Some(order) = self.all.get(&id) {
            if order.default {
                self.active_default.push(order.clone());
            } else {
                self.active_paid.push(order.clone());
            }
        }
    }

    pub(crate) fn deactivate(&mut self, id: u32) {
        self.active_paid.retain(|order| order.id != id);
        self.active_default.retain(|order| order.id != id);
    }

    pub(crate) fn active_paid(&self) -> &[Arc<Order>] {
        &self.active_paid
    }

    pub(crate) fn active_default(&self) -> &[Arc<Order>] {
        &self.active_default
    }

    pub(crate) fn get(&self, id: u32) -> Option<Arc<Order>> {
        self.all.get(&id).cloned()
    }

    pub(crate) fn all(&self) -> Vec<Arc<Order>> {
        self.all.values().cloned().collect()
    }

    pub(crate) fn match_paid(&self, counter: u64) -> Option<Arc<Order>> {
        let pool = if self.active_paid.is_empty() {
            &self.active_default
        } else {
            &self.active_paid
        };

        if pool.is_empty() {
            return None;
        }

        Some(pool[counter as usize % pool.len()].clone())
    }

    pub(crate) fn match_any(&self, counter: u64) -> Option<Arc<Order>> {
        let total = self.active_paid.len() + self.active_default.len();

        if total == 0 {
            return None;
        }

        self.active_paid
            .iter()
            .chain(&self.active_default)
            .nth(counter as usize % total)
            .cloned()
    }

    #[cfg(test)]
    pub(super) fn all_len(&self) -> usize {
        self.all.len()
    }

    #[cfg(test)]
    pub(super) fn active_len(&self) -> usize {
        self.active_paid.len() + self.active_default.len()
    }

    #[cfg(test)]
    pub(super) fn contains(&self, id: u32) -> bool {
        self.all.contains_key(&id)
    }

    #[cfg(test)]
    pub(super) fn active_id(&self, idx: usize) -> u32 {
        self.active_paid
            .iter()
            .chain(&self.active_default)
            .nth(idx)
            .unwrap()
            .id
    }
}
