use super::*;

struct BouncerConfig {
    warn_threshold: Duration,
    reconnect_threshold: Duration,
    drop_threshold: Duration,
    auth_timeout: Duration,
    idle_timeout: Duration,
    check_interval: Duration,
}

static CONFIG: LazyLock<BouncerConfig> = LazyLock::new(|| {
    if integration_test() {
        BouncerConfig {
            warn_threshold: Duration::from_secs(1),
            reconnect_threshold: Duration::from_secs(2),
            drop_threshold: Duration::from_secs(3),
            auth_timeout: Duration::from_secs(2),
            idle_timeout: Duration::from_secs(5),
            check_interval: Duration::from_secs(1),
        }
    } else {
        // Values derived from ckpool
        BouncerConfig {
            warn_threshold: Duration::from_secs(60),
            reconnect_threshold: Duration::from_secs(120),
            drop_threshold: Duration::from_secs(180),
            auth_timeout: Duration::from_secs(60),
            idle_timeout: Duration::from_secs(600),
            check_interval: Duration::from_secs(30),
        }
    }
});

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub(crate) enum Consequence {
    #[default]
    None,
    Warn,
    Reconnect,
    Drop,
}

pub(crate) struct Bouncer {
    disabled: bool,
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
    last_interaction: Instant,
}

impl Bouncer {
    pub(crate) fn new(disabled: bool) -> Self {
        Self {
            disabled,
            warn_threshold: CONFIG.warn_threshold,
            reconnect_threshold: CONFIG.reconnect_threshold,
            drop_threshold: CONFIG.drop_threshold,
            auth_timeout: CONFIG.auth_timeout,
            idle_timeout: CONFIG.idle_timeout,
            check_interval: CONFIG.check_interval,
            first_reject: None,
            consecutive_rejects: 0,
            current_consequence: Consequence::None,
            connected_at: Instant::now(),
            authorized: false,
            last_interaction: Instant::now(),
        }
    }

    pub(crate) fn authorize(&mut self) {
        self.authorized = true;
        self.last_interaction = Instant::now();
    }

    pub(crate) fn reject(&mut self) -> Consequence {
        if self.disabled {
            return Consequence::None;
        }

        self.consecutive_rejects += 1;
        self.last_interaction = Instant::now();

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
        self.last_interaction = Instant::now();
    }

    pub(crate) fn idle_check(&self) -> Consequence {
        if self.disabled {
            return Consequence::None;
        }

        if !self.authorized && self.connected_at.elapsed() > self.auth_timeout {
            return Consequence::Drop;
        }

        if self.last_interaction.elapsed() > self.idle_timeout {
            return Consequence::Drop;
        }

        Consequence::None
    }

    #[cfg(test)]
    pub(crate) fn is_authorized(&self) -> bool {
        self.authorized
    }

    pub(crate) fn consecutive_rejects(&self) -> u32 {
        self.consecutive_rejects
    }

    pub(crate) fn reject_duration(&self) -> Option<Duration> {
        self.first_reject.map(|t| t.elapsed())
    }

    pub(crate) fn check_interval(&self) -> Duration {
        self.check_interval
    }

    pub(crate) fn last_interaction_since(&self) -> Duration {
        self.last_interaction.elapsed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn new_bouncer_starts_at_zero() {
        let bouncer = Bouncer::new(false);
        assert_eq!(bouncer.consecutive_rejects(), 0);
        assert!(bouncer.reject_duration().is_none());
        assert!(!bouncer.is_authorized());
    }

    #[test]
    fn reject_before_warn_threshold_returns_none() {
        let mut bouncer = Bouncer::new(false);

        let consequence = bouncer.reject();
        assert_eq!(consequence, Consequence::None);
        assert_eq!(bouncer.consecutive_rejects(), 1);
    }

    #[test]
    fn accept_resets_consecutive_rejects() {
        let mut bouncer = Bouncer::new(false);

        bouncer.reject();
        bouncer.reject();
        assert_eq!(bouncer.consecutive_rejects(), 2);

        bouncer.accept();
        assert_eq!(bouncer.consecutive_rejects(), 0);
    }

    #[test]
    fn authorize_sets_authorized_flag() {
        let mut bouncer = Bouncer::new(false);
        assert!(!bouncer.is_authorized());

        bouncer.authorize();
        assert!(bouncer.is_authorized());
    }

    #[test]
    fn check_returns_none_when_authorized() {
        let mut bouncer = Bouncer::new(false);
        bouncer.authorize();

        assert_eq!(bouncer.idle_check(), Consequence::None);
    }

    #[test]
    fn check_returns_drop_when_not_authorized_after_timeout() {
        let mut bouncer = Bouncer::new(false);
        bouncer.auth_timeout = Duration::from_millis(10);

        thread::sleep(Duration::from_millis(15));

        assert_eq!(bouncer.idle_check(), Consequence::Drop);
    }

    #[test]
    fn check_returns_drop_when_idle_after_timeout() {
        let mut bouncer = Bouncer::new(false);
        bouncer.authorize();
        bouncer.idle_timeout = Duration::from_millis(10);

        thread::sleep(Duration::from_millis(15));

        assert_eq!(bouncer.idle_check(), Consequence::Drop);
    }

    #[test]
    fn accept_updates_last_share_time() {
        let mut bouncer = Bouncer::new(false);
        bouncer.authorize();
        bouncer.idle_timeout = Duration::from_millis(50);

        thread::sleep(Duration::from_millis(30));

        bouncer.accept();

        assert_eq!(bouncer.idle_check(), Consequence::None);

        thread::sleep(Duration::from_millis(60));

        assert_eq!(bouncer.idle_check(), Consequence::Drop);
    }
}
