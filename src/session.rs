use super::*;

#[derive(Debug, Clone)]
pub(crate) struct Session {
    pub(crate) enonce1: Extranonce,
    pub(crate) address: Address,
    pub(crate) workername: String,
    pub(crate) user_agent: Option<String>,
    pub(crate) version_mask: Option<Version>,
    pub(crate) authorized_at: SystemTime,
    pub(crate) last_seen: Instant,
}

impl Session {
    pub(crate) fn new(
        enonce1: Extranonce,
        address: Address,
        workername: String,
        user_agent: Option<String>,
        version_mask: Option<Version>,
        authorized_at: SystemTime,
    ) -> Self {
        Self {
            enonce1,
            address,
            workername,
            user_agent,
            version_mask,
            authorized_at,
            last_seen: Instant::now(),
        }
    }

    pub(crate) fn is_valid(&self, ttl: Duration) -> bool {
        self.last_seen.elapsed() < ttl
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    fn test_session(enonce1: &str) -> Session {
        Session::new(
            enonce1.parse().unwrap(),
            "tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx"
                .parse::<Address<NetworkUnchecked>>()
                .unwrap()
                .assume_checked(),
            "test_worker".to_string(),
            Some("TestMiner/1.0".to_string()),
            None,
            SystemTime::now(),
        )
    }

    #[test]
    fn new_session_has_expected_fields() {
        let session = test_session("deadbeef");

        assert_eq!(session.enonce1.to_string(), "deadbeef");
        assert_eq!(session.workername, "test_worker");
        assert_eq!(session.user_agent, Some("TestMiner/1.0".to_string()));
        assert!(session.version_mask.is_none());
    }

    #[test]
    fn new_session_is_valid() {
        let session = test_session("deadbeef");

        assert!(session.is_valid(Duration::from_secs(60)));
        assert!(session.is_valid(Duration::from_millis(100)));
    }

    #[test]
    fn session_expires_after_ttl() {
        let session = test_session("deadbeef");

        assert!(!session.is_valid(Duration::ZERO));
    }

    #[test]
    fn validity_decreases_over_time() {
        let session = test_session("deadbeef");

        assert!(session.is_valid(Duration::from_secs(60)));

        thread::sleep(Duration::from_millis(15));

        assert!(session.is_valid(Duration::from_secs(60)));
        assert!(!session.is_valid(Duration::from_millis(10)));
    }

    #[test]
    fn session_with_version_mask() {
        let session = Session::new(
            "cafebabe".parse().unwrap(),
            "tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx"
                .parse::<Address<NetworkUnchecked>>()
                .unwrap()
                .assume_checked(),
            "worker1".to_string(),
            None,
            Some(Version::from(0x1fffe000)),
            SystemTime::now(),
        );

        assert!(session.version_mask.is_some());
        assert_eq!(session.version_mask.unwrap(), Version::from(0x1fffe000));
    }

    #[test]
    fn session_without_user_agent() {
        let session = Session::new(
            "12345678".parse().unwrap(),
            "tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx"
                .parse::<Address<NetworkUnchecked>>()
                .unwrap()
                .assume_checked(),
            "anonymous".to_string(),
            None,
            None,
            SystemTime::now(),
        );

        assert!(session.user_agent.is_none());
    }

    #[test]
    fn authorized_at_is_preserved() {
        let auth_time = SystemTime::UNIX_EPOCH + Duration::from_secs(1700000000);

        let session = Session::new(
            "aabbccdd".parse().unwrap(),
            "tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx"
                .parse::<Address<NetworkUnchecked>>()
                .unwrap()
                .assume_checked(),
            "worker".to_string(),
            None,
            None,
            auth_time,
        );

        assert_eq!(session.authorized_at, auth_time);
    }
}
