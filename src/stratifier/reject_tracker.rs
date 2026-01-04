use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub(crate) enum EscalationLevel {
    #[default]
    None,
    Warn,
    Reconnect,
    Drop,
}

#[derive(Default)]
pub(crate) struct RejectTracker {
    first_reject: Option<Instant>,
    consecutive_rejects: u32,
    current_level: EscalationLevel,
}

impl RejectTracker {
    pub(crate) fn record_reject(&mut self) -> EscalationLevel {
        self.consecutive_rejects += 1;

        let elapsed = self.first_reject.get_or_insert_with(Instant::now).elapsed();

        let new_level = [
            (DROP_THRESHOLD, EscalationLevel::Drop),
            (RECONNECT_THRESHOLD, EscalationLevel::Reconnect),
            (WARN_THRESHOLD, EscalationLevel::Warn),
        ]
        .into_iter()
        .find(|(threshold, _)| elapsed >= *threshold)
        .map(|(_, level)| level)
        .unwrap_or(EscalationLevel::None);

        if new_level > self.current_level {
            self.current_level = new_level;
            new_level
        } else {
            EscalationLevel::None
        }
    }

    pub(crate) fn record_accept(&mut self) {
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
    fn new_tracker_starts_at_zero() {
        let tracker = RejectTracker::default();
        assert_eq!(tracker.consecutive_rejects(), 0);
        assert!(tracker.reject_duration().is_none());
    }

    #[test]
    fn reject_before_warn_threshold_returns_none() {
        let mut tracker = RejectTracker::default();

        let level = tracker.record_reject();
        assert_eq!(level, EscalationLevel::None);
        assert_eq!(tracker.consecutive_rejects(), 1);
    }

    #[test]
    fn accept_resets_consecutive_rejects() {
        let mut tracker = RejectTracker::default();

        tracker.record_reject();
        tracker.record_reject();
        assert_eq!(tracker.consecutive_rejects(), 2);

        tracker.record_accept();
        assert_eq!(tracker.consecutive_rejects(), 0);
    }
}
