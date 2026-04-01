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
        self.all.insert(order.id, order.clone());
        self.active.push(order);
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_order(id: u32) -> Arc<Order> {
        Arc::new(Order {
            id,
            target: "foo@bar:3333".parse().unwrap(),
            upstream: Upstream::test(id),
            allocator: Arc::new(EnonceAllocator::new(
                Extranonces::Pool(PoolExtranonces::new(4, 4).unwrap()),
                id,
            )),
            cancel: CancellationToken::new(),
            status: Mutex::new(OrderStatus::Active),
        })
    }

    #[tokio::test]
    async fn add() {
        let mut orders = Orders::new();
        let order = test_order(0);

        orders.add(order.clone());

        assert_eq!(orders.all.len(), 1);
        assert_eq!(orders.active.len(), 1);
        assert!(orders.all.contains_key(&0));
        assert_eq!(orders.active[0].id, 0);
    }

    #[tokio::test]
    async fn deactivate_removes_from_active_but_not_all() {
        let mut orders = Orders::new();
        orders.add(test_order(0));
        orders.add(test_order(1));

        orders.deactivate(0);

        assert_eq!(orders.all.len(), 2);
        assert_eq!(orders.active.len(), 1);
        assert_eq!(orders.active[0].id, 1);
    }

    #[tokio::test]
    async fn get() {
        let mut orders = Orders::new();
        orders.add(test_order(0));

        assert!(orders.get(0).is_some());
        assert!(orders.get(1).is_none());
    }

    #[tokio::test]
    async fn match_round_robin() {
        #[track_caller]
        fn case(orders: &Orders, counter: u64, expected: Option<u32>) {
            assert_eq!(
                orders.match_round_robin(counter).map(|order| order.id),
                expected,
            );
        }

        let mut orders = Orders::new();
        case(&orders, 0, None);

        orders.add(test_order(0));
        orders.add(test_order(1));
        orders.add(test_order(2));

        case(&orders, 0, Some(0));
        case(&orders, 1, Some(1));
        case(&orders, 2, Some(2));
        case(&orders, 3, Some(0));
    }
}
