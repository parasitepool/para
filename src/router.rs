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

    pub(crate) async fn connect(
        targets: &[UpstreamTarget],
        timeout: Duration,
        enonce1_extension_size: usize,
        endpoint: &str,
        cancel_token: &CancellationToken,
        tasks: &mut JoinSet<()>,
    ) -> Arc<Self> {
        let mut slots = Vec::new();

        for target in targets {
            match Slot::connect(
                target,
                timeout,
                enonce1_extension_size,
                endpoint,
                cancel_token,
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

        Arc::new(Self::new(slots))
    }

    pub(crate) fn spawn(self: &Arc<Self>, tasks: &mut JoinSet<()>) {
        for slot in &self.slots() {
            let slot = slot.clone();
            let router = self.clone();
            tasks.spawn(async move {
                slot.upstream.disconnected().await;
                warn!(
                    "Upstream {} disconnected, removing slot",
                    slot.upstream.endpoint()
                );
                slot.cancel_token.cancel();
                router.remove_slot(&slot);
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
