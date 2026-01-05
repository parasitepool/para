use super::*;

#[derive(Debug, Clone)]
pub(crate) struct SessionSnapshot {
    pub(crate) enonce1: Extranonce,
    stored_at: Instant,
}

impl SessionSnapshot {
    pub(crate) fn new(enonce1: Extranonce) -> Self {
        Self {
            enonce1,
            stored_at: Instant::now(),
        }
    }

    pub(crate) fn is_expired(&self, ttl: Duration) -> bool {
        self.stored_at.elapsed() >= ttl
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_session(enonce1: &str) -> SessionSnapshot {
        SessionSnapshot::new(enonce1.parse().unwrap())
    }

    #[test]
    fn new_session_has_expected_fields() {
        let session = test_session("deadbeef");

        assert_eq!(session.enonce1.to_string(), "deadbeef");
    }

    #[test]
    fn new_session_is_not_expired() {
        let session = test_session("deadbeef");

        assert!(!session.is_expired(Duration::from_secs(60)));
        assert!(!session.is_expired(Duration::from_millis(100)));
    }

    #[test]
    fn session_expired_with_zero_ttl() {
        let session = test_session("deadbeef");

        assert!(session.is_expired(Duration::ZERO));
    }

    #[test]
    fn session_expires_over_time() {
        let session = test_session("deadbeef");

        assert!(!session.is_expired(Duration::from_secs(60)));

        std::thread::sleep(Duration::from_millis(15));

        assert!(!session.is_expired(Duration::from_secs(60)));
        assert!(session.is_expired(Duration::from_millis(10)));
    }
}
