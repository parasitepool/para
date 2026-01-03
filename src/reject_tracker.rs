use std::time::{Duration, Instant};

/// Escalation level for reject tracking
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EscalationLevel {
    /// No escalation - rejects within threshold
    None,
    /// First threshold - warn and send fresh job
    Warn,
    /// Second threshold - suggest client reconnect
    Reconnect,
    /// Third threshold - force disconnect
    Drop,
}

/// Configuration for reject escalation thresholds
#[derive(Debug, Clone)]
pub(crate) struct RejectConfig {
    /// Duration after which to warn (default 60s)
    pub warn_threshold: Duration,
    /// Duration after which to suggest reconnect (default 120s)
    pub reconnect_threshold: Duration,
    /// Duration after which to force disconnect (default 180s)
    pub drop_threshold: Duration,
}

impl Default for RejectConfig {
    fn default() -> Self {
        Self {
            warn_threshold: Duration::from_secs(60),
            reconnect_threshold: Duration::from_secs(120),
            drop_threshold: Duration::from_secs(180),
        }
    }
}

/// Tracks consecutive invalid shares with timestamps for escalation
pub(crate) struct RejectTracker {
    config: RejectConfig,
    /// Timestamp of first reject in current consecutive run
    first_reject: Option<Instant>,
    /// Count of consecutive rejects (resets on valid share)
    consecutive_rejects: u32,
    /// Current escalation level (to avoid re-triggering same level)
    current_level: EscalationLevel,
}

impl RejectTracker {
    pub(crate) fn new(config: RejectConfig) -> Self {
        Self {
            config,
            first_reject: None,
            consecutive_rejects: 0,
            current_level: EscalationLevel::None,
        }
    }

    /// Records a rejected share and returns the current escalation level.
    /// Only returns a level higher than the previous call to avoid repeated triggers.
    pub(crate) fn record_reject(&mut self) -> EscalationLevel {
        self.consecutive_rejects += 1;

        // Start tracking time from first reject in this run
        let first_reject = *self.first_reject.get_or_insert_with(Instant::now);
        let elapsed = first_reject.elapsed();

        // Determine escalation level based on elapsed time
        let new_level = if elapsed >= self.config.drop_threshold {
            EscalationLevel::Drop
        } else if elapsed >= self.config.reconnect_threshold {
            EscalationLevel::Reconnect
        } else if elapsed >= self.config.warn_threshold {
            EscalationLevel::Warn
        } else {
            EscalationLevel::None
        };

        // Only return a level if it's higher than the current level
        // This prevents re-triggering the same action multiple times
        if new_level as u8 > self.current_level as u8 {
            self.current_level = new_level;
            new_level
        } else {
            EscalationLevel::None
        }
    }

    /// Records an accepted share, resetting consecutive reject tracking.
    pub(crate) fn record_accept(&mut self) {
        self.first_reject = None;
        self.consecutive_rejects = 0;
        // Reset escalation level so warnings can trigger again if rejects resume
        self.current_level = EscalationLevel::None;
    }

    /// Returns the current escalation level without recording anything
    pub(crate) fn current_level(&self) -> EscalationLevel {
        self.current_level
    }

    /// Returns the number of consecutive rejects
    pub(crate) fn consecutive_rejects(&self) -> u32 {
        self.consecutive_rejects
    }

    /// Returns duration since first reject in current consecutive run, if any
    pub(crate) fn reject_duration(&self) -> Option<Duration> {
        self.first_reject.map(|t| t.elapsed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_tracker_starts_at_none() {
        let tracker = RejectTracker::new(RejectConfig::default());
        assert_eq!(tracker.current_level(), EscalationLevel::None);
        assert_eq!(tracker.consecutive_rejects(), 0);
    }

    #[test]
    fn reject_before_warn_threshold_returns_none() {
        let config = RejectConfig {
            warn_threshold: Duration::from_secs(60),
            reconnect_threshold: Duration::from_secs(120),
            drop_threshold: Duration::from_secs(180),
        };
        let mut tracker = RejectTracker::new(config);

        // First reject should return None (not enough time elapsed)
        let level = tracker.record_reject();
        assert_eq!(level, EscalationLevel::None);
        assert_eq!(tracker.consecutive_rejects(), 1);
    }

    #[test]
    fn accept_resets_consecutive_rejects() {
        let mut tracker = RejectTracker::new(RejectConfig::default());

        tracker.record_reject();
        tracker.record_reject();
        assert_eq!(tracker.consecutive_rejects(), 2);

        tracker.record_accept();
        assert_eq!(tracker.consecutive_rejects(), 0);
    }

    #[test]
    fn escalation_levels_progress_correctly() {
        let config = RejectConfig {
            warn_threshold: Duration::from_millis(10),
            reconnect_threshold: Duration::from_millis(20),
            drop_threshold: Duration::from_millis(30),
        };
        let mut tracker = RejectTracker::new(config);

        // First reject starts the timer
        tracker.record_reject();

        // Wait and check escalation
        std::thread::sleep(Duration::from_millis(15));
        let level = tracker.record_reject();
        assert_eq!(level, EscalationLevel::Warn);

        // Same level shouldn't trigger again
        let level = tracker.record_reject();
        assert_eq!(level, EscalationLevel::None);

        std::thread::sleep(Duration::from_millis(10));
        let level = tracker.record_reject();
        assert_eq!(level, EscalationLevel::Reconnect);

        std::thread::sleep(Duration::from_millis(15));
        let level = tracker.record_reject();
        assert_eq!(level, EscalationLevel::Drop);
    }

    #[test]
    fn accept_allows_re_escalation() {
        let config = RejectConfig {
            warn_threshold: Duration::from_millis(10),
            reconnect_threshold: Duration::from_millis(20),
            drop_threshold: Duration::from_millis(30),
        };
        let mut tracker = RejectTracker::new(config);

        // Escalate to Warn
        tracker.record_reject();
        std::thread::sleep(Duration::from_millis(15));
        let level = tracker.record_reject();
        assert_eq!(level, EscalationLevel::Warn);

        // Accept resets
        tracker.record_accept();
        assert_eq!(tracker.current_level(), EscalationLevel::None);

        // Can escalate again
        tracker.record_reject();
        std::thread::sleep(Duration::from_millis(15));
        let level = tracker.record_reject();
        assert_eq!(level, EscalationLevel::Warn);
    }
}
