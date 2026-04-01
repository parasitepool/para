use super::*;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OrderStatus {
    Active,
    Cancelled,
    Disconnected,
}

pub(crate) struct Order {
    pub(crate) id: u32,
    pub(crate) target: UpstreamTarget,
    pub(crate) upstream: Arc<Upstream>,
    pub(crate) allocator: Arc<EnonceAllocator>,
    pub(crate) cancel: CancellationToken,
    pub(crate) status: Mutex<OrderStatus>,
}

impl Order {
    pub(crate) async fn connect(
        id: u32,
        target: &UpstreamTarget,
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
}
