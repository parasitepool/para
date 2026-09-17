use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DisconnectReason {
    Client,
    Server,
    Trim,
    Reject,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct DownstreamConnects {
    pub(crate) connects_1h: usize,
    pub(crate) sessions_1h: usize,
    pub(crate) probes_1h: usize,
    pub(crate) pending_1h: usize,
    pub(crate) disconnects_1h: usize,
    pub(crate) client_1h: usize,
    pub(crate) server_1h: usize,
    pub(crate) trim_1h: usize,
    pub(crate) reject_1h: usize,
    pub(crate) routing_rejects_1h: usize,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Outcome {
    Session,
    Probe,
    Reject,
}

struct Inner {
    connects: RollingCounter,
    sessions: RollingCounter,
    probes: RollingCounter,
    rejects: RollingCounter,
    client: RollingCounter,
    server: RollingCounter,
    trim: RollingCounter,
    rejected: RollingCounter,
    upstream: RollingCounter,
    orders: HashMap<u32, usize>,
}

impl Inner {
    fn new(origin: Instant) -> Self {
        Self {
            connects: RollingCounter::new(origin),
            sessions: RollingCounter::new(origin),
            probes: RollingCounter::new(origin),
            rejects: RollingCounter::new(origin),
            client: RollingCounter::new(origin),
            server: RollingCounter::new(origin),
            trim: RollingCounter::new(origin),
            rejected: RollingCounter::new(origin),
            upstream: RollingCounter::new(origin),
            orders: HashMap::new(),
        }
    }

    fn classify(&mut self, outcome: Outcome, accepted_at: Instant, now: Instant) {
        let counter = match outcome {
            Outcome::Session => &mut self.sessions,
            Outcome::Probe => &mut self.probes,
            Outcome::Reject => &mut self.rejects,
        };

        counter.record_at(1, accepted_at, now);
    }

    fn disconnect(&mut self, reason: DisconnectReason, now: Instant) {
        match reason {
            DisconnectReason::Client => self.client.record(1, now),
            DisconnectReason::Server => self.server.record(1, now),
            DisconnectReason::Trim => self.trim.record(1, now),
            DisconnectReason::Reject => self.rejected.record(1, now),
        }
    }

    fn snapshot(&self, now: Instant) -> DownstreamConnects {
        let connects_1h = self.connects.count(now);
        let sessions_1h = self.sessions.count(now);
        let probes_1h = self.probes.count(now);
        let routing_rejects_1h = self.rejects.count(now);
        let client_1h = self.client.count(now);
        let server_1h = self.server.count(now);
        let trim_1h = self.trim.count(now);
        let reject_1h = self.rejected.count(now);

        DownstreamConnects {
            connects_1h,
            sessions_1h,
            probes_1h,
            pending_1h: connects_1h.saturating_sub(sessions_1h + probes_1h + routing_rejects_1h),
            disconnects_1h: client_1h + server_1h + trim_1h + reject_1h,
            client_1h,
            server_1h,
            trim_1h,
            reject_1h,
            routing_rejects_1h,
        }
    }
}

pub(crate) struct Connections {
    inner: Mutex<Inner>,
}

impl Connections {
    pub(crate) fn new(origin: Instant) -> Self {
        Self {
            inner: Mutex::new(Inner::new(origin)),
        }
    }

    pub(crate) fn accept(self: &Arc<Self>, now: Instant) -> DownstreamConnection {
        self.inner.lock().connects.record(1, now);

        DownstreamConnection {
            connections: self.clone(),
            accepted_at: now,
            outcome: None,
            closed: false,
        }
    }

    pub(crate) fn downstream(&self) -> DownstreamConnects {
        self.inner.lock().snapshot(Instant::now())
    }

    pub(crate) fn record_upstream_disconnect(&self, order_id: u32) {
        let mut inner = self.inner.lock();

        inner.upstream.record(1, Instant::now());
        *inner.orders.entry(order_id).or_insert(0) += 1;
    }

    pub(crate) fn upstream_disconnects(&self, now: Instant) -> usize {
        self.inner.lock().upstream.count(now)
    }

    pub(crate) fn order_disconnects(&self, order_id: u32) -> usize {
        self.inner
            .lock()
            .orders
            .get(&order_id)
            .copied()
            .unwrap_or(0)
    }
}

pub(crate) struct DownstreamConnection {
    connections: Arc<Connections>,
    accepted_at: Instant,
    outcome: Option<Outcome>,
    closed: bool,
}

impl DownstreamConnection {
    pub(crate) fn start_session(&mut self) {
        self.classify(Outcome::Session, Instant::now());
    }

    pub(crate) fn reject(&mut self) {
        let now = Instant::now();

        self.classify(Outcome::Reject, now);

        if self.outcome == Some(Outcome::Reject) {
            self.finish(DisconnectReason::Reject, now);
        }
    }

    pub(crate) fn disconnect(&mut self, reason: DisconnectReason) {
        self.finish(reason, Instant::now());
    }

    fn classify(&mut self, outcome: Outcome, now: Instant) {
        if self.closed || self.outcome.is_some() {
            return;
        }

        self.connections
            .inner
            .lock()
            .classify(outcome, self.accepted_at, now);
        self.outcome = Some(outcome);
    }

    fn finish(&mut self, reason: DisconnectReason, now: Instant) {
        if self.closed {
            return;
        }

        self.closed = true;

        let mut inner = self.connections.inner.lock();

        if self.outcome.is_none() {
            inner.classify(Outcome::Probe, self.accepted_at, now);
        }

        inner.disconnect(reason, now);
    }
}

impl Drop for DownstreamConnection {
    fn drop(&mut self) {
        self.disconnect(DisconnectReason::Client);
    }
}

#[cfg(test)]
mod tests {
    use {super::*, crate::rolling::ROLLING_COUNTER_WINDOW};

    fn classify(connection: &mut DownstreamConnection, outcome: Outcome, now: Instant) {
        if outcome == Outcome::Probe {
            connection.finish(DisconnectReason::Client, now);
        } else {
            connection.classify(outcome, now);
        }
    }

    #[test]
    fn pending_becomes_a_probe_on_drop() {
        let now = Instant::now();
        let connections = Arc::new(Connections::new(now));
        let connection = connections.accept(now);

        assert_eq!(
            connections.downstream(),
            DownstreamConnects {
                connects_1h: 1,
                pending_1h: 1,
                ..DownstreamConnects::default()
            }
        );

        drop(connection);

        assert_eq!(
            connections.downstream(),
            DownstreamConnects {
                connects_1h: 1,
                probes_1h: 1,
                disconnects_1h: 1,
                client_1h: 1,
                ..DownstreamConnects::default()
            }
        );
    }

    #[test]
    fn sessions_and_disconnects_are_counted_once() {
        #[track_caller]
        fn case(reason: DisconnectReason) {
            let now = Instant::now();
            let connections = Arc::new(Connections::new(now));
            let mut connection = connections.accept(now);

            connection.start_session();
            connection.start_session();
            connection.reject();
            connection.disconnect(reason);
            connection.start_session();
            connection.disconnect(DisconnectReason::Client);
            drop(connection);

            assert_eq!(
                connections.downstream(),
                DownstreamConnects {
                    connects_1h: 1,
                    sessions_1h: 1,
                    disconnects_1h: 1,
                    client_1h: usize::from(reason == DisconnectReason::Client),
                    server_1h: usize::from(reason == DisconnectReason::Server),
                    trim_1h: usize::from(reason == DisconnectReason::Trim),
                    reject_1h: usize::from(reason == DisconnectReason::Reject),
                    ..DownstreamConnects::default()
                }
            );
        }

        case(DisconnectReason::Client);
        case(DisconnectReason::Server);
        case(DisconnectReason::Trim);
        case(DisconnectReason::Reject);
    }

    #[test]
    fn routing_reject_is_final_and_counts_as_disconnect() {
        let now = Instant::now();
        let connections = Arc::new(Connections::new(now));
        let mut connection = connections.accept(now);

        connection.reject();
        connection.reject();
        connection.start_session();
        connection.disconnect(DisconnectReason::Server);
        drop(connection);

        assert_eq!(
            connections.downstream(),
            DownstreamConnects {
                connects_1h: 1,
                disconnects_1h: 1,
                reject_1h: 1,
                routing_rejects_1h: 1,
                ..DownstreamConnects::default()
            }
        );
    }

    #[test]
    fn outcomes_expire_with_the_acceptance_bucket() {
        #[track_caller]
        fn case(outcome: Outcome) {
            let start = Instant::now();
            let connections = Arc::new(Connections::new(start));
            let mut connection = connections.accept(start + Duration::from_secs(59));
            classify(&mut connection, outcome, start + Duration::from_secs(61));

            let mut probe = connections.accept(start + Duration::from_secs(3500));
            probe.finish(DisconnectReason::Client, start + Duration::from_secs(3501));

            let before = connections
                .inner
                .lock()
                .snapshot(start + ROLLING_COUNTER_WINDOW - Duration::from_secs(1));
            assert_eq!(before.connects_1h, 2);
            assert_eq!(before.pending_1h, 0);
            assert_eq!(
                before.sessions_1h + before.probes_1h + before.routing_rejects_1h,
                2
            );

            let after = connections
                .inner
                .lock()
                .snapshot(start + ROLLING_COUNTER_WINDOW);
            assert_eq!(after.connects_1h, 1);
            assert_eq!(after.probes_1h, 1);
            assert_eq!(
                after.sessions_1h + after.routing_rejects_1h + after.pending_1h,
                0
            );
        }

        case(Outcome::Session);
        case(Outcome::Probe);
        case(Outcome::Reject);
    }

    #[test]
    fn expired_connections_cannot_overwrite_reused_buckets() {
        #[track_caller]
        fn case(outcome: Outcome) {
            let start = Instant::now();
            let connections = Arc::new(Connections::new(start));
            let mut expired = connections.accept(start);
            let now = start + ROLLING_COUNTER_WINDOW;
            let mut current = connections.accept(now);

            classify(&mut current, outcome, now);
            classify(&mut expired, outcome, now + Duration::from_secs(1));

            let snapshot = connections
                .inner
                .lock()
                .snapshot(now + Duration::from_secs(1));
            assert_eq!(snapshot.connects_1h, 1);
            assert_eq!(
                snapshot.sessions_1h,
                usize::from(outcome == Outcome::Session)
            );
            assert_eq!(snapshot.probes_1h, usize::from(outcome == Outcome::Probe));
            assert_eq!(
                snapshot.routing_rejects_1h,
                usize::from(outcome == Outcome::Reject)
            );
            assert_eq!(snapshot.pending_1h, 0);

            expired.finish(DisconnectReason::Server, now + Duration::from_secs(1));
            let snapshot = connections
                .inner
                .lock()
                .snapshot(now + Duration::from_secs(1));
            assert_eq!(
                snapshot.disconnects_1h,
                match outcome {
                    Outcome::Probe => 2,
                    Outcome::Session => 1,
                    Outcome::Reject => 1,
                }
            );
        }

        case(Outcome::Session);
        case(Outcome::Probe);
        case(Outcome::Reject);
    }

    #[test]
    fn global_upstream_counter_expires_but_per_order_total_persists() {
        let start = Instant::now();
        let connections = Connections::new(start);

        connections.record_upstream_disconnect(0);

        let expired = start + ROLLING_COUNTER_WINDOW;
        assert_eq!(connections.upstream_disconnects(expired), 0);
        assert_eq!(connections.order_disconnects(0), 1);
    }

    #[test]
    fn upstream_counters_are_per_order_and_reset_with_the_registry() {
        let connections = Connections::new(Instant::now());

        connections.record_upstream_disconnect(0);
        connections.record_upstream_disconnect(0);
        connections.record_upstream_disconnect(1);

        let now = Instant::now();
        assert_eq!(connections.upstream_disconnects(now), 3);
        assert_eq!(connections.order_disconnects(0), 2);
        assert_eq!(connections.order_disconnects(1), 1);
        assert_eq!(connections.order_disconnects(2), 0);

        let connections = Connections::new(now);
        assert_eq!(connections.upstream_disconnects(now), 0);
        assert_eq!(connections.order_disconnects(0), 0);
        assert_eq!(connections.downstream(), DownstreamConnects::default());
    }
}
