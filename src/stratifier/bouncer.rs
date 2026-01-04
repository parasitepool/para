use super::*;

const WARN_THRESHOLD: Duration = Duration::from_secs(60);
const RECONNECT_THRESHOLD: Duration = Duration::from_secs(120);
const DROP_THRESHOLD: Duration = Duration::from_secs(180);
const AUTH_TIMEOUT: Duration = Duration::from_secs(60);
const IDLE_TIMEOUT: Duration = Duration::from_secs(3600);
const CHECK_INTERVAL: Duration = Duration::from_secs(30);

const TEST_WARN_THRESHOLD: Duration = Duration::from_secs(1);
const TEST_RECONNECT_THRESHOLD: Duration = Duration::from_secs(2);
const TEST_DROP_THRESHOLD: Duration = Duration::from_secs(3);
const TEST_AUTH_TIMEOUT: Duration = Duration::from_secs(2);
const TEST_IDLE_TIMEOUT: Duration = Duration::from_secs(5);
const TEST_CHECK_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub(crate) enum Consequence {
    #[default]
    None,
    Warn,
    Reconnect,
    Drop,
}

pub(crate) struct Bouncer {
    warn_threshold: Duration,
    reconnect_threshold: Duration,
    drop_threshold: Duration,
    auth_timeout: Duration,
    idle_timeout: Duration,
    check_interval: Duration,
    first_reject: Option<Instant>,
    consecutive_rejects: u32,
    current_consequence: Consequence,
    connected_at: Instant,
    authorized: bool,
    last_share_at: Option<Instant>,
}

impl Bouncer {
    pub(crate) fn new() -> Self {
        let (
            warn_threshold,
            reconnect_threshold,
            drop_threshold,
            auth_timeout,
            idle_timeout,
            check_interval,
        ) = if integration_test() {
            (
                TEST_WARN_THRESHOLD,
                TEST_RECONNECT_THRESHOLD,
                TEST_DROP_THRESHOLD,
                TEST_AUTH_TIMEOUT,
                TEST_IDLE_TIMEOUT,
                TEST_CHECK_INTERVAL,
            )
        } else {
            (
                WARN_THRESHOLD,
                RECONNECT_THRESHOLD,
                DROP_THRESHOLD,
                AUTH_TIMEOUT,
                IDLE_TIMEOUT,
                CHECK_INTERVAL,
            )
        };

        Self {
            warn_threshold,
            reconnect_threshold,
            drop_threshold,
            auth_timeout,
            idle_timeout,
            check_interval,
            first_reject: None,
            consecutive_rejects: 0,
            current_consequence: Consequence::None,
            connected_at: Instant::now(),
            authorized: false,
            last_share_at: None,
        }
    }

    pub(crate) fn authorize(&mut self) {
        self.authorized = true;
        self.last_share_at = Some(Instant::now());
    }

    pub(crate) fn reject(&mut self) -> Consequence {
        self.consecutive_rejects += 1;

        let elapsed = self.first_reject.get_or_insert_with(Instant::now).elapsed();

        let new_consequence = [
            (self.drop_threshold, Consequence::Drop),
            (self.reconnect_threshold, Consequence::Reconnect),
            (self.warn_threshold, Consequence::Warn),
        ]
        .into_iter()
        .find(|(threshold, _)| elapsed >= *threshold)
        .map(|(_, consequence)| consequence)
        .unwrap_or(Consequence::None);

        if new_consequence > self.current_consequence {
            self.current_consequence = new_consequence;
            new_consequence
        } else {
            Consequence::None
        }
    }

    pub(crate) fn accept(&mut self) {
        self.first_reject = None;
        self.consecutive_rejects = 0;
        self.current_consequence = Consequence::None;
        self.last_share_at = Some(Instant::now());
    }

    pub(crate) fn check(&self) -> Consequence {
        if !self.authorized && self.connected_at.elapsed() > self.auth_timeout {
            return Consequence::Drop;
        }

        if let Some(last_share) = self.last_share_at
            && last_share.elapsed() > self.idle_timeout
        {
            return Consequence::Drop;
        }

        Consequence::None
    }

    pub(crate) fn is_authorized(&self) -> bool {
        self.authorized
    }

    pub(crate) fn consecutive_rejects(&self) -> u32 {
        self.consecutive_rejects
    }

    pub(crate) fn reject_duration(&self) -> Option<Duration> {
        self.first_reject.map(|t| t.elapsed())
    }

    pub(crate) fn auth_timeout(&self) -> Duration {
        self.auth_timeout
    }

    pub(crate) fn idle_timeout(&self) -> Duration {
        self.idle_timeout
    }

    pub(crate) fn check_interval(&self) -> Duration {
        self.check_interval
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn new_bouncer_starts_at_zero() {
        let bouncer = Bouncer::new();
        assert_eq!(bouncer.consecutive_rejects(), 0);
        assert!(bouncer.reject_duration().is_none());
        assert!(!bouncer.is_authorized());
    }

    #[test]
    fn reject_before_warn_threshold_returns_none() {
        let mut bouncer = Bouncer::new();

        let consequence = bouncer.reject();
        assert_eq!(consequence, Consequence::None);
        assert_eq!(bouncer.consecutive_rejects(), 1);
    }

    #[test]
    fn accept_resets_consecutive_rejects() {
        let mut bouncer = Bouncer::new();

        bouncer.reject();
        bouncer.reject();
        assert_eq!(bouncer.consecutive_rejects(), 2);

        bouncer.accept();
        assert_eq!(bouncer.consecutive_rejects(), 0);
    }

    #[test]
    fn authorize_sets_authorized_flag() {
        let mut bouncer = Bouncer::new();
        assert!(!bouncer.is_authorized());

        bouncer.authorize();
        assert!(bouncer.is_authorized());
    }

    #[test]
    fn check_returns_none_when_authorized() {
        let mut bouncer = Bouncer::new();
        bouncer.authorize();

        assert_eq!(bouncer.check(), Consequence::None);
    }

    #[test]
    fn check_returns_drop_when_not_authorized_after_timeout() {
        let mut bouncer = Bouncer::new();
        bouncer.auth_timeout = Duration::from_millis(10);

        thread::sleep(Duration::from_millis(15));

        assert_eq!(bouncer.check(), Consequence::Drop);
    }

    #[test]
    fn check_returns_drop_when_idle_after_timeout() {
        let mut bouncer = Bouncer::new();
        bouncer.authorize();
        bouncer.idle_timeout = Duration::from_millis(10);

        thread::sleep(Duration::from_millis(15));

        assert_eq!(bouncer.check(), Consequence::Drop);
    }

    #[test]
    fn accept_updates_last_share_time() {
        let mut bouncer = Bouncer::new();
        bouncer.authorize();
        bouncer.idle_timeout = Duration::from_millis(50);

        thread::sleep(Duration::from_millis(30));

        bouncer.accept();

        assert_eq!(bouncer.check(), Consequence::None);

        thread::sleep(Duration::from_millis(60));

        assert_eq!(bouncer.check(), Consequence::Drop);
    }
}
