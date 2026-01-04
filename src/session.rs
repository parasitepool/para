use super::*;
use std::sync::Mutex as StdMutex;

#[derive(Debug, Clone)]
pub(crate) struct StoredSession {
    pub enonce1: Extranonce,
    pub address: Address<NetworkUnchecked>,
    pub workername: String,
    pub user_agent: Option<String>,
    pub version_mask: Option<Version>,
    pub last_seen: Instant,
    pub authorized_at: Option<SystemTime>,
}

impl StoredSession {
    pub(crate) fn new(
        enonce1: Extranonce,
        address: Address<NetworkUnchecked>,
        workername: String,
        user_agent: Option<String>,
        version_mask: Option<Version>,
        authorized_at: Option<SystemTime>,
    ) -> Self {
        Self {
            enonce1,
            address,
            workername,
            user_agent,
            version_mask,
            last_seen: Instant::now(),
            authorized_at,
        }
    }

    fn is_valid(&self, ttl: Duration) -> bool {
        self.last_seen.elapsed() < ttl
    }
}

pub(crate) struct SessionStore {
    sessions: DashMap<Extranonce, StoredSession>,
    ttl: Duration,
    last_cleanup: StdMutex<Instant>,
}

impl SessionStore {
    pub(crate) fn new(ttl: Duration) -> Self {
        Self {
            sessions: DashMap::new(),
            ttl,
            last_cleanup: StdMutex::new(Instant::now()),
        }
    }

    pub(crate) fn store(&self, session: StoredSession) {
        info!(
            "Storing session for {} ({:?}) with enonce1 {}",
            session.workername, session.address, session.enonce1
        );
        self.sessions.insert(session.enonce1.clone(), session);
        self.maybe_cleanup();
    }

    pub(crate) fn take(&self, enonce1: &Extranonce) -> Option<StoredSession> {
        let (_, session) = self.sessions.remove(enonce1)?;
        if session.is_valid(self.ttl) {
            Some(session)
        } else {
            None
        }
    }

    fn maybe_cleanup(&self) {
        let mut last_cleanup = self.last_cleanup.lock().unwrap();
        if last_cleanup.elapsed() < Duration::from_secs(60) {
            return;
        }
        *last_cleanup = Instant::now();
        drop(last_cleanup);

        let ttl = self.ttl;
        let before = self.sessions.len();
        self.sessions.retain(|_, session| session.is_valid(ttl));
        let removed = before.saturating_sub(self.sessions.len());
        if removed > 0 {
            info!("Cleaned up {} expired sessions", removed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_session(enonce1: &str) -> StoredSession {
        StoredSession::new(
            enonce1.parse().unwrap(),
            "tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx"
                .parse::<Address<NetworkUnchecked>>()
                .unwrap(),
            "test_worker".to_string(),
            Some("TestMiner/1.0".to_string()),
            None,
            None,
        )
    }

    #[test]
    fn store_and_take_session() {
        let store = SessionStore::new(Duration::from_secs(60));
        let session = test_session("deadbeef");
        let enonce1 = session.enonce1.clone();

        store.store(session);

        let taken = store.take(&enonce1);
        assert!(taken.is_some());
        assert_eq!(taken.unwrap().workername, "test_worker");
    }

    #[test]
    fn expired_session_not_taken() {
        let store = SessionStore::new(Duration::from_millis(10));
        let session = test_session("deadbeef");
        let enonce1 = session.enonce1.clone();

        store.store(session);
        std::thread::sleep(Duration::from_millis(20));

        let taken = store.take(&enonce1);
        assert!(taken.is_none());
    }

    #[test]
    fn take_nonexistent_returns_none() {
        let store = SessionStore::new(Duration::from_secs(60));
        let enonce1: Extranonce = "deadbeef".parse().unwrap();

        assert!(store.take(&enonce1).is_none());
    }

    #[test]
    fn take_removes_session() {
        let store = SessionStore::new(Duration::from_secs(60));
        let session = test_session("deadbeef");
        let enonce1 = session.enonce1.clone();

        store.store(session);

        // First take succeeds
        assert!(store.take(&enonce1).is_some());
        // Second take fails (session was removed)
        assert!(store.take(&enonce1).is_none());
    }
}
