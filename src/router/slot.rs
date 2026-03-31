use super::*;

pub(crate) struct Slot {
    pub(crate) id: u32,
    pub(crate) upstream: Arc<Upstream>,
    pub(crate) allocator: Arc<EnonceAllocator>,
    pub(crate) cancel: CancellationToken,
}

impl Slot {
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
            upstream,
            allocator,
            cancel,
        }))
    }
}
