use super::*;

pub(crate) struct Slot {
    pub(crate) upstream: Arc<Upstream>,
    pub(crate) metatron: Arc<Metatron>,
    pub(crate) workbase_rx: watch::Receiver<Arc<Notify>>,
    pub(crate) cancel_token: CancellationToken,
}

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
