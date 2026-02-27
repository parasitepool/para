use {super::*, slot::Slot};

mod slot;

pub(crate) struct Router {
    slots: RwLock<Vec<Arc<Slot>>>,
    counter: AtomicU64,
}

impl Router {
    pub(crate) fn new(slots: Vec<Arc<Slot>>) -> Self {
        Self {
            slots: RwLock::new(slots),
            counter: AtomicU64::new(0),
        }
    }

    pub(crate) fn assign_to_slot(&self) -> Option<Arc<Slot>> {
        let slots = self.slots.read();
        if slots.is_empty() {
            return None;
        }

        let idx = self.counter.fetch_add(1, Ordering::Relaxed) as usize % slots.len();

        Some(slots[idx].clone())
    }

    pub(crate) fn remove_slot(&self, slot: &Arc<Slot>) {
        let mut slots = self.slots.write();
        slots.retain(|s| !Arc::ptr_eq(s, slot));
    }

    pub(crate) fn slots(&self) -> Vec<Arc<Slot>> {
        self.slots.read().clone()
    }

    pub(crate) fn slot_by_upstream_id(&self, id: u32) -> Option<Arc<Slot>> {
        self.slots
            .read()
            .iter()
            .find(|s| s.upstream.id() == id)
            .cloned()
    }

    pub(crate) async fn connect(
        targets: &[UpstreamTarget],
        timeout: Duration,
        enonce1_extension_size: usize,
        endpoint: &str,
        cancel_token: &CancellationToken,
        tasks: &mut JoinSet<()>,
    ) -> Result<Arc<Self>, Error> {
        let mut slots = Vec::new();

        for (upstream_id, target) in targets.iter().enumerate() {
            match Slot::connect(
                upstream_id as u32,
                target,
                timeout,
                enonce1_extension_size,
                endpoint,
                cancel_token.child_token(),
                tasks,
            )
            .await
            {
                Ok(slot) => slots.push(slot),
                Err(err) => {
                    warn!("Skipping upstream {target}: {err}");
                }
            }
        }

        ensure!(!slots.is_empty(), "all upstream connections failed");

        Ok(Arc::new(Self::new(slots)))
    }

    pub(crate) fn spawn(self: &Arc<Self>, cancel: CancellationToken, tasks: &mut JoinSet<()>) {
        for slot in &self.slots() {
            let slot = slot.clone();
            let router = self.clone();
            let cancel = cancel.clone();
            tasks.spawn(async move {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {}
                    _ = slot.upstream.disconnected() => {
                        warn!(
                            "Upstream {} disconnected, removing slot",
                            slot.upstream.endpoint()
                        );
                        slot.cancel_token.cancel();
                        router.remove_slot(&slot);
                        if router.slots().is_empty() {
                            error!("All upstreams disconnected, shutting down");
                            cancel.cancel();
                        }
                    }
                }
            });
        }
    }
}

impl StatusLine for Router {
    fn status_line(&self) -> String {
        let now = Instant::now();
        let slots = self.slots();
        let mut total_sessions = 0;
        let mut total_hashrate = 0.0;
        let mut connected = 0;

        for slot in &slots {
            let stats = slot.metatron.snapshot();
            total_sessions += slot.metatron.total_sessions();
            total_hashrate += stats.hashrate_1m(now).0;
            if slot.upstream.is_connected() {
                connected += 1;
            }
        }

        format!(
            "upstreams={}/{}  sessions={}  hashrate={:.2}",
            connected,
            slots.len(),
            total_sessions,
            HashRate(total_hashrate),
        )
    }
}
