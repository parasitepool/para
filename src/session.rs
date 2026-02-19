use super::*;

#[allow(dead_code)]
pub(crate) struct Session {
    enonce1: Extranonce,
    socket_addr: SocketAddr,
    connected_at: Instant,
    difficulty: Mutex<Difficulty>,
    accepted: AtomicU64,
    rejected: AtomicU64,
    last_share: Mutex<Option<Instant>>,
    active: AtomicBool,
}

impl Session {
    pub(crate) fn new(enonce1: Extranonce, socket_addr: SocketAddr) -> Self {
        Self {
            enonce1,
            socket_addr,
            connected_at: Instant::now(),
            difficulty: Mutex::new(Difficulty::default()),
            accepted: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
            last_share: Mutex::new(None),
            active: AtomicBool::new(true),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn enonce1(&self) -> &Extranonce {
        &self.enonce1
    }

    #[allow(dead_code)]
    pub(crate) fn socket_addr(&self) -> SocketAddr {
        self.socket_addr
    }

    #[allow(dead_code)]
    pub(crate) fn connected_at(&self) -> Instant {
        self.connected_at
    }

    #[allow(dead_code)]
    pub(crate) fn difficulty(&self) -> Difficulty {
        *self.difficulty.lock()
    }

    pub(crate) fn set_difficulty(&self, diff: Difficulty) {
        *self.difficulty.lock() = diff;
    }

    #[allow(dead_code)]
    pub(crate) fn accepted(&self) -> u64 {
        self.accepted.load(Ordering::Relaxed)
    }

    #[allow(dead_code)]
    pub(crate) fn rejected(&self) -> u64 {
        self.rejected.load(Ordering::Relaxed)
    }

    pub(crate) fn last_share(&self) -> Option<Instant> {
        *self.last_share.lock()
    }

    pub(crate) fn is_active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }

    pub(crate) fn record_accepted(&self, pool_diff: Difficulty, share_diff: Difficulty) {
        let _ = share_diff;
        let now = Instant::now();
        *self.last_share.lock() = Some(now);
        self.set_difficulty(pool_diff);
        self.accepted.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_rejected(&self) {
        self.rejected.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn deactivate(&self) {
        self.active.store(false, Ordering::Relaxed);
    }

    pub(crate) fn activate(&self) {
        self.active.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_enonce1() -> Extranonce {
        "deadbeef".parse().unwrap()
    }

    #[test]
    fn new_session_defaults() {
        let session = Session::new(test_enonce1(), "127.0.0.1:1".parse().unwrap());

        assert!(session.is_active());
        assert_eq!(session.accepted(), 0);
        assert_eq!(session.rejected(), 0);
        assert!(session.last_share().is_none());
    }

    #[test]
    fn record_updates_share_counts() {
        let session = Session::new(test_enonce1(), "127.0.0.1:1".parse().unwrap());

        session.record_accepted(Difficulty::from(2), Difficulty::from(3));
        session.record_rejected();

        assert_eq!(session.accepted(), 1);
        assert_eq!(session.rejected(), 1);
        assert!(session.last_share().is_some());
    }

    #[test]
    fn deactivate_marks_inactive() {
        let session = Session::new(test_enonce1(), "127.0.0.1:1".parse().unwrap());

        session.deactivate();

        assert!(!session.is_active());
    }
}
