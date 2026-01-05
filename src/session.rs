use super::*;

#[derive(Debug, Clone)]
pub(crate) struct SessionSnapshot {
    pub(crate) enonce1: Extranonce,
    pub(crate) user_agent: Option<String>,
    pub(crate) version_mask: Option<Version>,
    stored_at: Instant,
}

impl SessionSnapshot {
    pub(crate) fn new(
        enonce1: Extranonce,
        user_agent: Option<String>,
        version_mask: Option<Version>,
    ) -> Self {
        Self {
            enonce1,
            user_agent,
            version_mask,
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
        SessionSnapshot::new(
            enonce1.parse().unwrap(),
            Some("TestMiner/1.0".to_string()),
            None,
        )
    }

    #[test]
    fn new_session_has_expected_fields() {
        let session = test_session("deadbeef");

        assert_eq!(session.enonce1.to_string(), "deadbeef");
        assert_eq!(session.user_agent, Some("TestMiner/1.0".to_string()));
        assert!(session.version_mask.is_none());
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

    #[test]
    fn session_with_version_mask() {
        let session = SessionSnapshot::new(
            "cafebabe".parse().unwrap(),
            None,
            Some(Version::from(0x1fffe000)),
        );

        assert!(session.version_mask.is_some());
        assert_eq!(session.version_mask.unwrap(), Version::from(0x1fffe000));
    }

    #[test]
    fn session_without_user_agent() {
        let session = SessionSnapshot::new("12345678".parse().unwrap(), None, None);

        assert!(session.user_agent.is_none());
    }
}
