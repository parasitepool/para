use {super::*, crate::session::Session};

struct Stats {
    dsps_1m: DecayingAverage,
    dsps_5m: DecayingAverage,
    dsps_15m: DecayingAverage,
    dsps_1hr: DecayingAverage,
    dsps_6hr: DecayingAverage,
    dsps_1d: DecayingAverage,
    dsps_7d: DecayingAverage,
    sps_1m: DecayingAverage,
    sps_5m: DecayingAverage,
    sps_15m: DecayingAverage,
    sps_1hr: DecayingAverage,
    best_ever: Option<Difficulty>,
    last_share: Option<Instant>,
    total_work: f64,
}

pub(crate) struct Worker {
    workername: String,
    sessions: DashMap<Extranonce, Arc<Session>>,
    stats: Mutex<Stats>,
    accepted: AtomicU64,
    rejected: AtomicU64,
}

impl Worker {
    pub(crate) fn new(workername: String) -> Self {
        Self {
            workername,
            sessions: DashMap::new(),
            stats: Mutex::new(Stats {
                dsps_1m: DecayingAverage::new(Duration::from_mins(1)),
                dsps_5m: DecayingAverage::new(Duration::from_mins(5)),
                dsps_15m: DecayingAverage::new(Duration::from_mins(15)),
                dsps_1hr: DecayingAverage::new(Duration::from_hours(1)),
                dsps_6hr: DecayingAverage::new(Duration::from_hours(6)),
                dsps_1d: DecayingAverage::new(Duration::from_hours(24)),
                dsps_7d: DecayingAverage::new(Duration::from_hours(24 * 7)),
                sps_1m: DecayingAverage::new(Duration::from_mins(1)),
                sps_5m: DecayingAverage::new(Duration::from_mins(5)),
                sps_15m: DecayingAverage::new(Duration::from_mins(15)),
                sps_1hr: DecayingAverage::new(Duration::from_hours(1)),
                best_ever: None,
                last_share: None,
                total_work: 0.0,
            }),
            accepted: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
        }
    }

    pub(crate) fn get_or_create_session(
        &self,
        enonce1: Extranonce,
        socket_addr: SocketAddr,
    ) -> Arc<Session> {
        let session = self
            .sessions
            .entry(enonce1.clone())
            .or_insert_with(|| Arc::new(Session::new(enonce1, socket_addr)))
            .clone();
        session.activate();
        session
    }

    pub(crate) fn get_session(&self, enonce1: &Extranonce) -> Option<Arc<Session>> {
        self.sessions
            .get(enonce1)
            .map(|entry| entry.value().clone())
    }

    pub(crate) fn active_session_count(&self) -> u64 {
        self.sessions
            .iter()
            .filter(|entry| entry.value().is_active())
            .count() as u64
    }

    pub(crate) fn session_count(&self) -> usize {
        self.sessions.len()
    }

    pub(crate) fn record_accepted(&self, pool_diff: Difficulty, share_diff: Difficulty) {
        let now = Instant::now();
        let mut stats = self.stats.lock();
        let diff = pool_diff.as_f64();
        stats.dsps_1m.record(diff, now);
        stats.dsps_5m.record(diff, now);
        stats.dsps_15m.record(diff, now);
        stats.dsps_1hr.record(diff, now);
        stats.dsps_6hr.record(diff, now);
        stats.dsps_1d.record(diff, now);
        stats.dsps_7d.record(diff, now);
        stats.sps_1m.record(1.0, now);
        stats.sps_5m.record(1.0, now);
        stats.sps_15m.record(1.0, now);
        stats.sps_1hr.record(1.0, now);
        stats.total_work += diff;
        stats.last_share = Some(now);
        if stats.best_ever.is_none_or(|best| share_diff > best) {
            stats.best_ever = Some(share_diff);
        }
        drop(stats);
        self.accepted.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_rejected(&self) {
        self.rejected.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn workername(&self) -> &str {
        &self.workername
    }

    pub(crate) fn hashrate_1m(&self) -> HashRate {
        HashRate::from_dsps(self.stats.lock().dsps_1m.value_at(Instant::now()))
    }

    pub(crate) fn hashrate_5m(&self) -> HashRate {
        HashRate::from_dsps(self.stats.lock().dsps_5m.value_at(Instant::now()))
    }

    pub(crate) fn hashrate_15m(&self) -> HashRate {
        HashRate::from_dsps(self.stats.lock().dsps_15m.value_at(Instant::now()))
    }

    pub(crate) fn hashrate_1hr(&self) -> HashRate {
        HashRate::from_dsps(self.stats.lock().dsps_1hr.value_at(Instant::now()))
    }

    pub(crate) fn hashrate_6hr(&self) -> HashRate {
        HashRate::from_dsps(self.stats.lock().dsps_6hr.value_at(Instant::now()))
    }

    pub(crate) fn hashrate_1d(&self) -> HashRate {
        HashRate::from_dsps(self.stats.lock().dsps_1d.value_at(Instant::now()))
    }

    pub(crate) fn hashrate_7d(&self) -> HashRate {
        HashRate::from_dsps(self.stats.lock().dsps_7d.value_at(Instant::now()))
    }

    pub(crate) fn sps_1m(&self) -> f64 {
        self.stats.lock().sps_1m.value_at(Instant::now())
    }

    pub(crate) fn sps_5m(&self) -> f64 {
        self.stats.lock().sps_5m.value_at(Instant::now())
    }

    pub(crate) fn sps_15m(&self) -> f64 {
        self.stats.lock().sps_15m.value_at(Instant::now())
    }

    pub(crate) fn sps_1hr(&self) -> f64 {
        self.stats.lock().sps_1hr.value_at(Instant::now())
    }

    pub(crate) fn accepted(&self) -> u64 {
        self.accepted.load(Ordering::Relaxed)
    }

    pub(crate) fn rejected(&self) -> u64 {
        self.rejected.load(Ordering::Relaxed)
    }

    pub(crate) fn best_ever(&self) -> Option<Difficulty> {
        self.stats.lock().best_ever
    }

    pub(crate) fn last_share(&self) -> Option<Instant> {
        self.stats.lock().last_share
    }

    pub(crate) fn total_work(&self) -> f64 {
        self.stats.lock().total_work
    }

    pub(crate) fn sessions(&self) -> &DashMap<Extranonce, Arc<Session>> {
        &self.sessions
    }
}
