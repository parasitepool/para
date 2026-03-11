use {super::*, slot::Slot};

mod slot;

pub(crate) struct Router {
    metatron: Arc<Metatron>,
    slots: RwLock<Vec<Arc<Slot>>>,
    counter: AtomicU64,
}

impl Router {
    pub(crate) fn new(metatron: Arc<Metatron>, slots: Vec<Arc<Slot>>) -> Self {
        Self {
            metatron,
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

    pub(crate) fn slot_by_index(&self, index: usize) -> Option<Arc<Slot>> {
        self.slots.read().iter().find(|s| s.index == index).cloned()
    }

    pub(crate) async fn connect(
        metatron: Arc<Metatron>,
        targets: &[UpstreamTarget],
        timeout: Duration,
        enonce1_extension_size: usize,
        cancel_token: &CancellationToken,
        tasks: &TaskTracker,
    ) -> Result<Arc<Self>, Error> {
        let mut slots = Vec::new();
        for (index, target) in targets.iter().enumerate() {
            match Slot::connect(
                index,
                index as u32,
                target,
                timeout,
                enonce1_extension_size,
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

        Ok(Arc::new(Self::new(metatron, slots)))
    }

    pub(crate) fn spawn(self: &Arc<Self>, cancel: CancellationToken, tasks: &TaskTracker) {
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
                        slot.cancel.cancel();
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

    pub(crate) fn metatron(&self) -> Arc<Metatron> {
        self.metatron.clone()
    }

    pub(crate) fn upstream_sessions(&self, upstream_id: u32) -> Vec<Arc<Session>> {
        self.metatron
            .users()
            .iter()
            .flat_map(|user| user.sessions())
            .filter(|session| session.id().upstream_id() == upstream_id)
            .collect()
    }

    pub(crate) fn upstream_session_count(&self, upstream_id: u32) -> usize {
        self.metatron
            .users()
            .iter()
            .flat_map(|user| user.sessions())
            .filter(|session| session.id().upstream_id() == upstream_id)
            .count()
    }

    pub(crate) fn upstream_idle_count(&self, upstream_id: u32) -> usize {
        let now = Instant::now();
        self.metatron
            .users()
            .iter()
            .flat_map(|user| user.sessions())
            .filter(|session| session.id().upstream_id() == upstream_id && session.is_idle(now))
            .count()
    }

    pub(crate) fn upstream_snapshot(&self, upstream_id: u32) -> Stats {
        let now = Instant::now();
        self.metatron
            .users()
            .iter()
            .flat_map(|user| user.sessions())
            .filter(|session| session.id().upstream_id() == upstream_id)
            .fold(Stats::new(), |mut combined, session| {
                combined.absorb(session.snapshot(), now);
                combined
            })
    }

    pub(crate) fn upstream_user_count(&self, upstream_id: u32) -> usize {
        self.metatron
            .users()
            .iter()
            .filter(|user| {
                user.workers()
                    .any(|worker| worker.upstream_session_count(upstream_id) > 0)
            })
            .count()
    }

    pub(crate) fn upstream_worker_count(&self, upstream_id: u32) -> usize {
        self.metatron
            .users()
            .iter()
            .map(|user| {
                user.workers()
                    .filter(|worker| worker.upstream_session_count(upstream_id) > 0)
                    .count()
            })
            .sum()
    }

    pub(crate) fn upstream_disconnected_count(&self, upstream_id: u32) -> usize {
        self.metatron
            .disconnected()
            .iter()
            .filter(|entry| entry.value().0.id().upstream_id() == upstream_id)
            .count()
    }
}

impl StatusLine for Router {
    fn status_line(&self) -> String {
        let now = Instant::now();
        let slots = self.slots();
        let stats = self.metatron.snapshot();
        let mut connected = 0;

        for slot in &slots {
            if slot.upstream.is_connected() {
                connected += 1;
            }
        }

        format!(
            "upstreams={}/{}  sessions={}  hashrate={:.2}",
            connected,
            slots.len(),
            self.metatron.total_sessions(),
            stats.hashrate_1m(now),
        )
    }
}

#[cfg(test)]
mod tests {
    use {super::*, crate::stratifier::state::Authorization};

    fn test_address() -> Address {
        "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc"
            .parse::<Address<bitcoin::address::NetworkUnchecked>>()
            .unwrap()
            .assume_checked()
    }

    fn test_auth(enonce1: &str, workername: &str) -> Arc<Authorization> {
        Arc::new(Authorization {
            enonce1: enonce1.parse().unwrap(),
            address: test_address(),
            workername: workername.into(),
            username: Username::new(format!(
                "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.{workername}"
            )),
            version_mask: None,
        })
    }

    fn test_router() -> Router {
        Router::new(Arc::new(Metatron::new()), vec![])
    }

    #[test]
    fn upstream_counts_are_isolated() {
        let router = test_router();
        let metatron = router.metatron();

        let s1 = metatron.new_session(test_auth("deadbeef", "foo"), 0);
        let s2 = metatron.new_session(test_auth("cafebabe", "bar"), 1);

        assert_eq!(router.upstream_session_count(0), 1);
        assert_eq!(router.upstream_session_count(1), 1);
        assert_eq!(router.upstream_sessions(0)[0].id(), s1.id());
        assert_eq!(router.upstream_sessions(1)[0].id(), s2.id());
    }

    #[test]
    fn upstream_user_and_worker_counts_are_filtered() {
        let router = test_router();
        let metatron = router.metatron();

        metatron.new_session(test_auth("deadbeef", "foo"), 0);
        metatron.new_session(test_auth("cafebabe", "bar"), 0);
        metatron.new_session(test_auth("facefeed", "foo"), 1);

        assert_eq!(router.upstream_user_count(0), 1);
        assert_eq!(router.upstream_worker_count(0), 2);
        assert_eq!(router.upstream_user_count(1), 1);
        assert_eq!(router.upstream_worker_count(1), 1);
    }

    #[test]
    fn upstream_snapshot_only_includes_requested_upstream() {
        let router = test_router();
        let metatron = router.metatron();

        let s1 = metatron.new_session(test_auth("deadbeef", "foo"), 0);
        let s2 = metatron.new_session(test_auth("cafebabe", "bar"), 1);

        s1.record_accepted(Difficulty::from(100.0), Difficulty::from(200.0));
        s2.record_rejected(Difficulty::from(300.0));

        let upstream_0 = router.upstream_snapshot(0);
        let upstream_1 = router.upstream_snapshot(1);

        assert_eq!(upstream_0.accepted_shares, 1);
        assert_eq!(upstream_0.rejected_shares, 0);
        assert_eq!(upstream_1.accepted_shares, 0);
        assert_eq!(upstream_1.rejected_shares, 1);
    }

    #[test]
    fn upstream_disconnected_count_is_filtered() {
        let router = test_router();
        let metatron = router.metatron();

        let s1 = metatron.new_session(test_auth("deadbeef", "foo"), 0);
        let s2 = metatron.new_session(test_auth("cafebabe", "bar"), 1);

        metatron.retire_session(s1);
        metatron.retire_session(s2);

        assert_eq!(router.upstream_disconnected_count(0), 1);
        assert_eq!(router.upstream_disconnected_count(1), 1);
    }

    #[test]
    fn retire_removes_session_from_upstream_queries() {
        let router = test_router();
        let metatron = router.metatron();

        let session = metatron.new_session(test_auth("deadbeef", "foo"), 0);
        assert_eq!(router.upstream_session_count(0), 1);

        metatron.retire_session(session);

        assert_eq!(router.upstream_session_count(0), 0);
        assert!(router.upstream_sessions(0).is_empty());
    }
}
