use {super::*, dashmap::DashMap};

pub(crate) struct Worker {
    workername: String,
    sessions: DashMap<u64, Arc<Session>>,
    lifetime: Mutex<Stats>,
}

impl Worker {
    pub(crate) fn new(workername: String) -> Self {
        Self {
            workername,
            sessions: DashMap::new(),
            lifetime: Mutex::new(Stats::new()),
        }
    }

    pub(crate) fn new_session(&self, session: Arc<Session>) {
        self.sessions.insert(session.id(), session);
    }

    pub(crate) fn retire_session(&self, id: u64) {
        if let Some((_, session)) = self.sessions.remove(&id) {
            let now = Instant::now();
            let snapshot = session.stats.lock().clone();
            self.lifetime.lock().absorb(&snapshot, now);
        }
    }

    pub(crate) fn workername(&self) -> &str {
        &self.workername
    }

    pub(crate) fn session_count(&self) -> usize {
        self.sessions
            .iter()
            .filter(|session| session.is_active())
            .count()
    }

    pub(crate) fn hashrate_1m(&self) -> HashRate {
        let now = Instant::now();
        let from_sessions = self
            .sessions
            .iter()
            .map(|session| session.hashrate_1m())
            .fold(HashRate::ZERO, |acc, r| acc + r);
        from_sessions + HashRate::from_dsps(self.lifetime.lock().dsps_1m.value_at(now))
    }

    pub(crate) fn hashrate_5m(&self) -> HashRate {
        let now = Instant::now();
        let from_sessions = self
            .sessions
            .iter()
            .map(|session| session.hashrate_5m())
            .fold(HashRate::ZERO, |acc, r| acc + r);
        from_sessions + HashRate::from_dsps(self.lifetime.lock().dsps_5m.value_at(now))
    }

    pub(crate) fn hashrate_15m(&self) -> HashRate {
        let now = Instant::now();
        let from_sessions = self
            .sessions
            .iter()
            .map(|session| session.hashrate_15m())
            .fold(HashRate::ZERO, |acc, r| acc + r);
        from_sessions + HashRate::from_dsps(self.lifetime.lock().dsps_15m.value_at(now))
    }

    pub(crate) fn hashrate_1hr(&self) -> HashRate {
        let now = Instant::now();
        let from_sessions = self
            .sessions
            .iter()
            .map(|session| session.hashrate_1hr())
            .fold(HashRate::ZERO, |acc, r| acc + r);
        from_sessions + HashRate::from_dsps(self.lifetime.lock().dsps_1hr.value_at(now))
    }

    pub(crate) fn hashrate_6hr(&self) -> HashRate {
        let now = Instant::now();
        let from_sessions = self
            .sessions
            .iter()
            .map(|session| session.hashrate_6hr())
            .fold(HashRate::ZERO, |acc, r| acc + r);
        from_sessions + HashRate::from_dsps(self.lifetime.lock().dsps_6hr.value_at(now))
    }

    pub(crate) fn hashrate_1d(&self) -> HashRate {
        let now = Instant::now();
        let from_sessions = self
            .sessions
            .iter()
            .map(|session| session.hashrate_1d())
            .fold(HashRate::ZERO, |acc, r| acc + r);
        from_sessions + HashRate::from_dsps(self.lifetime.lock().dsps_1d.value_at(now))
    }

    pub(crate) fn hashrate_7d(&self) -> HashRate {
        let now = Instant::now();
        let from_sessions = self
            .sessions
            .iter()
            .map(|session| session.hashrate_7d())
            .fold(HashRate::ZERO, |acc, r| acc + r);
        from_sessions + HashRate::from_dsps(self.lifetime.lock().dsps_7d.value_at(now))
    }

    pub(crate) fn sps_1m(&self) -> f64 {
        let now = Instant::now();
        let from_sessions: f64 = self.sessions.iter().map(|session| session.sps_1m()).sum();
        from_sessions + self.lifetime.lock().sps_1m.value_at(now)
    }

    pub(crate) fn sps_5m(&self) -> f64 {
        let now = Instant::now();
        let from_sessions: f64 = self.sessions.iter().map(|session| session.sps_5m()).sum();
        from_sessions + self.lifetime.lock().sps_5m.value_at(now)
    }

    pub(crate) fn sps_15m(&self) -> f64 {
        let now = Instant::now();
        let from_sessions: f64 = self.sessions.iter().map(|session| session.sps_15m()).sum();
        from_sessions + self.lifetime.lock().sps_15m.value_at(now)
    }

    pub(crate) fn sps_1hr(&self) -> f64 {
        let now = Instant::now();
        let from_sessions: f64 = self.sessions.iter().map(|session| session.sps_1hr()).sum();
        from_sessions + self.lifetime.lock().sps_1hr.value_at(now)
    }

    pub(crate) fn accepted(&self) -> u64 {
        let from_sessions: u64 = self.sessions.iter().map(|session| session.accepted()).sum();
        from_sessions + self.lifetime.lock().accepted
    }

    pub(crate) fn rejected(&self) -> u64 {
        let from_sessions: u64 = self.sessions.iter().map(|session| session.rejected()).sum();
        from_sessions + self.lifetime.lock().rejected
    }

    pub(crate) fn best_ever(&self) -> Option<Difficulty> {
        let from_sessions = self
            .sessions
            .iter()
            .filter_map(|session| session.best_ever())
            .max();
        let from_lifetime = self.lifetime.lock().best_ever;
        match (from_sessions, from_lifetime) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        }
    }

    pub(crate) fn last_share(&self) -> Option<Instant> {
        let from_sessions = self
            .sessions
            .iter()
            .filter_map(|session| session.last_share())
            .max();
        let from_lifetime = self.lifetime.lock().last_share;
        match (from_sessions, from_lifetime) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        }
    }

    pub(crate) fn total_work(&self) -> f64 {
        let from_sessions: f64 = self
            .sessions
            .iter()
            .map(|session| session.total_work())
            .sum();
        from_sessions + self.lifetime.lock().total_work
    }
}
