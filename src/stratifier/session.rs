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

    fn test_snapshot(enonce1: &str) -> SessionSnapshot {
        SessionSnapshot::new(enonce1.parse().unwrap())
    }

    #[test]
    fn new_snapshot_has_expected_fields() {
        let snapshot = test_snapshot("deadbeef");

        assert_eq!(snapshot.enonce1.to_string(), "deadbeef");
    }

    #[test]
    fn new_snapshot_is_not_expired() {
        let snapshot = test_snapshot("deadbeef");

        assert!(!snapshot.is_expired(Duration::from_secs(60)));
        assert!(!snapshot.is_expired(Duration::from_millis(100)));
    }

    #[test]
    fn snapshot_expired_with_zero_ttl() {
        let snapshot = test_snapshot("deadbeef");

        assert!(snapshot.is_expired(Duration::ZERO));
    }

    #[test]
    fn snapshot_expires_over_time() {
        let snapshot = test_snapshot("deadbeef");

        assert!(!snapshot.is_expired(Duration::from_secs(60)));

        std::thread::sleep(Duration::from_millis(15));

        assert!(!snapshot.is_expired(Duration::from_secs(60)));
        assert!(snapshot.is_expired(Duration::from_millis(10)));
    }
}
