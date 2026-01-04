use super::*;

const WARN_THRESHOLD: Duration = Duration::from_secs(60);
const RECONNECT_THRESHOLD: Duration = Duration::from_secs(120);
const DROP_THRESHOLD: Duration = Duration::from_secs(180);

const TEST_WARN_THRESHOLD: Duration = Duration::from_secs(5);
const TEST_RECONNECT_THRESHOLD: Duration = Duration::from_secs(10);
const TEST_DROP_THRESHOLD: Duration = Duration::from_secs(15);

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
    first_reject: Option<Instant>,
    consecutive_rejects: u32,
    current_consequence: Consequence,
}

impl Bouncer {
    pub(crate) fn new() -> Self {
        let (warn_threshold, reconnect_threshold, drop_threshold) = if integration_test() {
            (
                TEST_WARN_THRESHOLD,
                TEST_RECONNECT_THRESHOLD,
                TEST_DROP_THRESHOLD,
            )
        } else {
            (WARN_THRESHOLD, RECONNECT_THRESHOLD, DROP_THRESHOLD)
        };

        Self {
            warn_threshold,
            reconnect_threshold,
            drop_threshold,
            first_reject: None,
            consecutive_rejects: 0,
            current_consequence: Consequence::None,
        }
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
        let warn_threshold = self.warn_threshold;
        let reconnect_threshold = self.reconnect_threshold;
        let drop_threshold = self.drop_threshold;

        *self = Self {
            warn_threshold,
            reconnect_threshold,
            drop_threshold,
            first_reject: None,
            consecutive_rejects: 0,
            current_consequence: Consequence::None,
        };
    }

    pub(crate) fn consecutive_rejects(&self) -> u32 {
        self.consecutive_rejects
    }

    pub(crate) fn reject_duration(&self) -> Option<Duration> {
        self.first_reject.map(|t| t.elapsed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_bouncer_starts_at_zero() {
        let bouncer = Bouncer::new();
        assert_eq!(bouncer.consecutive_rejects(), 0);
        assert!(bouncer.reject_duration().is_none());
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
}
