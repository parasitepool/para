use super::*;

pub(crate) struct Slot {
    pub(crate) upstream: Arc<Upstream>,
    pub(crate) metatron: Arc<Metatron>,
    pub(crate) cancel_token: CancellationToken,
}

impl Slot {
    pub(crate) async fn connect(
        upstream_id: u32,
        target: &UpstreamTarget,
        timeout: Duration,
        enonce1_extension_size: usize,
        endpoint: &str,
        slot_cancel: CancellationToken,
        tasks: &mut JoinSet<()>,
    ) -> Result<Arc<Self>> {
        let upstream =
            Upstream::connect(upstream_id, target, timeout, slot_cancel.clone(), tasks).await?;

        let proxy_extranonces = ProxyExtranonces::new(
            upstream.enonce1().clone(),
            upstream.enonce2_size(),
            enonce1_extension_size,
        )?;

        let metatron = Arc::new(Metatron::new(
            Extranonces::Proxy(proxy_extranonces),
            endpoint.to_string(),
            upstream_id,
        ));

        metatron.clone().spawn(slot_cancel.clone(), tasks);

        info!("Upstream {target} connected");

        Ok(Arc::new(Self {
            upstream,
            metatron,
            cancel_token: slot_cancel,
        }))
    }
}
