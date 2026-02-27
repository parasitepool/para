use super::*;

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

    pub(crate) fn new_session(&self, session: Arc<Session>) {
        self.sessions.insert(session.id(), session);
    }

    pub(crate) fn retire_session(&self, id: SessionId) {
        if let Some((_, session)) = self.sessions.remove(&id) {
            let snapshot = session.snapshot();
            self.lifetime.lock().absorb(snapshot, Instant::now());
        }
    }

    pub(crate) fn workername(&self) -> &str {
        &self.workername
    }

    pub(crate) fn session_count(&self) -> usize {
        self.sessions.len()
    }

    pub(crate) fn sessions(&self) -> impl Iterator<Item = Arc<Session>> {
        self.sessions.iter().map(|entry| entry.value().clone())
    }

    pub(crate) fn snapshot(&self) -> Stats {
        let now = Instant::now();

        self.sessions
            .iter()
            .fold(self.lifetime.lock().clone(), |mut combined, session| {
                combined.absorb(session.snapshot(), now);
                combined
            })
    }
}
