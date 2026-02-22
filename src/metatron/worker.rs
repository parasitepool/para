use {super::*, dashmap::DashMap};

struct LifetimeStats {
    total_work: f64,
    accepted: u64,
    rejected: u64,
    best_ever: Option<Difficulty>,
    last_share: Option<Instant>,
}

pub(crate) struct Worker {
    workername: String,
    sessions: DashMap<u64, Arc<Session>>,
    lifetime: Mutex<LifetimeStats>,
}

impl Worker {
    pub(crate) fn new(workername: String) -> Self {
        Self {
            workername,
            sessions: DashMap::new(),
            lifetime: Mutex::new(LifetimeStats {
                total_work: 0.0,
                accepted: 0,
                rejected: 0,
                best_ever: None,
                last_share: None,
            }),
        }
    }

    pub(crate) fn new_session(&self, session: Arc<Session>) {
        self.sessions.insert(session.id(), session);
    }

    pub(crate) fn retire_session(&self, id: u64) {
        if let Some((_, session)) = self.sessions.remove(&id) {
            let mut lifetime = self.lifetime.lock();
            lifetime.total_work += session.total_work();
            lifetime.accepted += session.accepted();
            lifetime.rejected += session.rejected();
            if session
                .best_ever()
                .is_some_and(|d| lifetime.best_ever.is_none_or(|best| d > best))
            {
                lifetime.best_ever = session.best_ever();
            }
            let last = session.last_share();
            if last.is_some_and(|l| lifetime.last_share.is_none_or(|prev| l > prev)) {
                lifetime.last_share = last;
            }
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
        self.sessions
            .iter()
            .map(|session| session.hashrate_1m())
            .fold(HashRate::ZERO, |acc, r| acc + r)
    }

    pub(crate) fn hashrate_5m(&self) -> HashRate {
        self.sessions
            .iter()
            .map(|session| session.hashrate_5m())
            .fold(HashRate::ZERO, |acc, r| acc + r)
    }

    pub(crate) fn hashrate_15m(&self) -> HashRate {
        self.sessions
            .iter()
            .map(|session| session.hashrate_15m())
            .fold(HashRate::ZERO, |acc, r| acc + r)
    }

    pub(crate) fn hashrate_1hr(&self) -> HashRate {
        self.sessions
            .iter()
            .map(|session| session.hashrate_1hr())
            .fold(HashRate::ZERO, |acc, r| acc + r)
    }

    pub(crate) fn hashrate_6hr(&self) -> HashRate {
        self.sessions
            .iter()
            .map(|session| session.hashrate_6hr())
            .fold(HashRate::ZERO, |acc, r| acc + r)
    }

    pub(crate) fn hashrate_1d(&self) -> HashRate {
        self.sessions
            .iter()
            .map(|session| session.hashrate_1d())
            .fold(HashRate::ZERO, |acc, r| acc + r)
    }

    pub(crate) fn hashrate_7d(&self) -> HashRate {
        self.sessions
            .iter()
            .map(|session| session.hashrate_7d())
            .fold(HashRate::ZERO, |acc, r| acc + r)
    }

    pub(crate) fn sps_1m(&self) -> f64 {
        self.sessions.iter().map(|session| session.sps_1m()).sum()
    }

    pub(crate) fn sps_5m(&self) -> f64 {
        self.sessions.iter().map(|session| session.sps_5m()).sum()
    }

    pub(crate) fn sps_15m(&self) -> f64 {
        self.sessions.iter().map(|session| session.sps_15m()).sum()
    }

    pub(crate) fn sps_1hr(&self) -> f64 {
        self.sessions.iter().map(|session| session.sps_1hr()).sum()
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
