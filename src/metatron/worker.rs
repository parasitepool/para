use {super::*, dashmap::DashMap};

pub(crate) struct Worker {
    workername: String,
    sessions: DashMap<u64, Arc<Session>>,
}

impl Worker {
    pub(crate) fn new(workername: String) -> Self {
        Self {
            workername,
            sessions: DashMap::new(),
        }
    }

    pub(crate) fn new_session(&self, session: Arc<Session>) {
        self.sessions.insert(session.id(), session);
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
        self.sessions.iter().map(|session| session.accepted()).sum()
    }

    pub(crate) fn rejected(&self) -> u64 {
        self.sessions.iter().map(|session| session.rejected()).sum()
    }

    pub(crate) fn best_ever(&self) -> Option<Difficulty> {
        self.sessions
            .iter()
            .filter_map(|session| session.best_ever())
            .max()
    }

    pub(crate) fn last_share(&self) -> Option<Instant> {
        self.sessions
            .iter()
            .filter_map(|session| session.last_share())
            .max()
    }

    pub(crate) fn total_work(&self) -> f64 {
        self.sessions
            .iter()
            .map(|session| session.total_work())
            .sum()
    }
}
