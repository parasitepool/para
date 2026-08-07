use {super::*, crate::store::entry::WorkerEntry};

pub(crate) struct Worker {
    workername: String,
    sessions: DashMap<SessionId, Arc<Session>>,
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

    pub(crate) fn from_entry(entry: WorkerEntry) -> Result<Self> {
        Ok(Self {
            workername: entry.workername,
            sessions: DashMap::new(),
            lifetime: Mutex::new(Stats::from_entry(entry.stats)?),
        })
    }

    pub(super) fn new_session(&self, session: Arc<Session>) {
        self.sessions.insert(session.id(), session);
    }

    pub(super) fn retire_session(&self, id: SessionId) -> bool {
        let Some(session) = self.sessions.get(&id).map(|entry| entry.value().clone()) else {
            return false;
        };

        let snapshot = session.snapshot();
        let mut lifetime = self.lifetime.lock();
        lifetime.absorb(snapshot, Instant::now());
        self.sessions.remove(&id);

        true
    }

    pub(crate) fn workername(&self) -> &str {
        &self.workername
    }

    pub(crate) fn session_count(&self) -> usize {
        self.sessions.len()
    }

    pub(crate) fn has_accepted(&self) -> bool {
        self.lifetime.lock().accepted_shares > 0
            || self.sessions.iter().any(|session| session.has_accepted())
    }

    pub(crate) fn sessions(&self) -> impl Iterator<Item = Arc<Session>> {
        self.sessions.iter().map(|entry| entry.value().clone())
    }

    pub(crate) fn snapshot(&self) -> Stats {
        let now = Instant::now();
        let lifetime = self.lifetime.lock();

        self.sessions
            .iter()
            .fold(lifetime.clone(), |mut combined, session| {
                combined.absorb(session.snapshot(), now);
                combined
            })
    }

    pub(crate) fn to_entry(&self, now: Instant) -> WorkerEntry {
        WorkerEntry {
            workername: self.workername.clone(),
            stats: self.snapshot().to_entry(now),
        }
    }
}
