use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub(crate) enum Consequence {
    #[default]
    None,
    Warn,
    Reconnect,
    Drop,
}

#[derive(Default)]
pub(crate) struct Bouncer {
    first_reject: Option<Instant>,
    consecutive_rejects: u32,
    current_consequence: Consequence,
}

impl Bouncer {
    pub(crate) fn reject(&mut self) -> Consequence {
        self.consecutive_rejects += 1;

        let elapsed = self.first_reject.get_or_insert_with(Instant::now).elapsed();

        let new_consequence = [
            (drop_threshold(), Consequence::Drop),
            (reconnect_threshold(), Consequence::Reconnect),
            (warn_threshold(), Consequence::Warn),
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
        *self = Self::default();
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
        let bouncer = Bouncer::default();
        assert_eq!(bouncer.consecutive_rejects(), 0);
        assert!(bouncer.reject_duration().is_none());
    }

    #[test]
    fn reject_before_warn_threshold_returns_none() {
        let mut bouncer = Bouncer::default();

        let consequence = bouncer.reject();
        assert_eq!(consequence, Consequence::None);
        assert_eq!(bouncer.consecutive_rejects(), 1);
    }

    #[test]
    fn accept_resets_consecutive_rejects() {
        let mut bouncer = Bouncer::default();

        bouncer.reject();
        bouncer.reject();
        assert_eq!(bouncer.consecutive_rejects(), 2);

        bouncer.accept();
        assert_eq!(bouncer.consecutive_rejects(), 0);
    }
}
