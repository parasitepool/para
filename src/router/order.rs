use super::*;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OrderStatus {
    Active,
    Fulfilled,
    Cancelled,
    Disconnected,
}

pub struct Order {
    pub(crate) id: u32,
    pub(crate) target: UpstreamTarget,
    pub(crate) target_work: Option<HashDays>,
    pub(crate) upstream: Arc<Upstream>,
    pub(crate) allocator: Arc<EnonceAllocator>,
    pub(crate) cancel: CancellationToken,
    pub(crate) status: Mutex<OrderStatus>,
}

impl Order {
    pub(crate) async fn connect(
        id: u32,
        target: &UpstreamTarget,
        target_work: Option<HashDays>,
        timeout: Duration,
        enonce1_extension_size: usize,
        cancel: CancellationToken,
        tasks: &TaskTracker,
    ) -> Result<Arc<Self>> {
        let upstream = Upstream::connect(id, target, timeout, cancel.clone(), tasks).await?;

        let proxy_extranonces = ProxyExtranonces::new(
            upstream.enonce1().clone(),
            upstream.enonce2_size(),
            enonce1_extension_size,
        )?;

        let allocator = Arc::new(EnonceAllocator::new(
            Extranonces::Proxy(proxy_extranonces),
            id,
        ));

        info!("Upstream {target} connected");

        Ok(Arc::new(Self {
            id,
            target: target.clone(),
            target_work,
            upstream,
            allocator,
            cancel,
            status: Mutex::new(OrderStatus::Active),
        }))
    }

    pub(crate) fn status(&self) -> OrderStatus {
        *self.status.lock()
    }

    pub(crate) fn set_status(&self, status: OrderStatus) {
        *self.status.lock() = status;
    }

    pub(crate) fn is_active(&self) -> bool {
        self.status() == OrderStatus::Active
    }

    pub(crate) fn is_fulfilled(&self) -> bool {
        let target = match self.target_work {
            Some(target) => target,
            None => return false,
        };

        let accepted = self.upstream.accepted_work().to_hash_days();

        accepted >= target
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_order(target_work: Option<HashDays>) -> Arc<Order> {
        Arc::new(Order {
            id: 0,
            target: "foo@bar:3333".parse().unwrap(),
            target_work,
            upstream: Upstream::test(0),
            allocator: Arc::new(EnonceAllocator::new(
                Extranonces::Pool(PoolExtranonces::new(4, 4).unwrap()),
                0,
            )),
            cancel: CancellationToken::new(),
            status: Mutex::new(OrderStatus::Active),
        })
    }

    #[tokio::test]
    async fn is_fulfilled_without_target() {
        let order = test_order(None);
        assert!(!order.is_fulfilled());
    }

    #[tokio::test]
    async fn is_fulfilled_below_target() {
        let order = test_order(Some(HashDays(1e15)));
        assert!(!order.is_fulfilled());
    }

    #[tokio::test]
    async fn is_fulfilled_at_target() {
        let target = HashDays(1e12);
        let order = test_order(Some(target));
        order.upstream.set_accepted_work(target.to_total_work());
        assert!(order.is_fulfilled());
    }

    #[tokio::test]
    async fn is_fulfilled_above_target() {
        let target = HashDays(1e12);
        let order = test_order(Some(target));
        order
            .upstream
            .set_accepted_work(HashDays(2e12).to_total_work());
        assert!(order.is_fulfilled());
    }

    #[test]
    fn status_serde() {
        #[track_caller]
        fn case(status: OrderStatus, expected: &str) {
            assert_eq!(serde_json::to_string(&status).unwrap(), expected);
        }

        case(OrderStatus::Active, "\"active\"");
        case(OrderStatus::Fulfilled, "\"fulfilled\"");
        case(OrderStatus::Cancelled, "\"cancelled\"");
        case(OrderStatus::Disconnected, "\"disconnected\"");
    }
}
