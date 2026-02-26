use super::*;

pub(crate) struct Slot {
    pub(crate) upstream: Arc<Upstream>,
    pub(crate) metatron: Arc<Metatron>,
    pub(crate) workbase_rx: watch::Receiver<Arc<Notify>>,
    pub(crate) cancel_token: CancellationToken,
}

impl Slot {
    pub(crate) async fn connect(
        target: &UpstreamTarget,
        timeout: Duration,
        enonce1_extension_size: usize,
        endpoint: &str,
        cancel_token: &CancellationToken,
        tasks: &mut JoinSet<()>,
    ) -> Result<Arc<Self>> {
        let (upstream, events) = Upstream::connect(target.clone(), timeout).await?;
        let upstream = Arc::new(upstream);

        let slot_cancel = cancel_token.child_token();

        let workbase_rx = upstream
            .clone()
            .spawn(events, slot_cancel.clone(), tasks)
            .await?;

        let proxy_extranonces = ProxyExtranonces::new(
            upstream.enonce1().clone(),
            upstream.enonce2_size(),
            enonce1_extension_size,
        )?;

        let metatron = Arc::new(Metatron::new(
            Extranonces::Proxy(proxy_extranonces),
            endpoint.to_string(),
        ));

        metatron.clone().spawn(cancel_token.clone(), tasks);

        info!("Upstream {target} connected");

        Ok(Arc::new(Self {
            upstream,
            metatron,
            workbase_rx,
            cancel_token: slot_cancel,
        }))
    }
}
