use super::*;

pub(crate) const ROLLING_COUNTER_WINDOW: Duration = Duration::from_secs(60 * 60);
const BUCKET: Duration = Duration::from_secs(60);
const BUCKETS: usize = (ROLLING_COUNTER_WINDOW.as_secs() / BUCKET.as_secs()) as usize;

pub(crate) struct RollingCounter {
    anchor: Option<Instant>,
    buckets: [(u64, usize); BUCKETS],
}

impl Default for RollingCounter {
    fn default() -> Self {
        Self {
            anchor: None,
            buckets: [(0, 0); BUCKETS],
        }
    }
}

impl RollingCounter {
    pub(crate) fn new(origin: Instant) -> Self {
        Self {
            anchor: Some(origin),
            buckets: [(0, 0); BUCKETS],
        }
    }

    pub(crate) fn record(&mut self, count: usize, now: Instant) {
        self.record_at(count, now, now);
    }

    pub(crate) fn record_at(&mut self, count: usize, recorded_at: Instant, now: Instant) {
        if count == 0 {
            return;
        }

        let anchor = *self.anchor.get_or_insert(recorded_at);
        let index = recorded_at.saturating_duration_since(anchor).as_secs() / BUCKET.as_secs();
        let current = now.saturating_duration_since(anchor).as_secs() / BUCKET.as_secs();

        if index + BUCKETS as u64 <= current {
            return;
        }

        let bucket = &mut self.buckets[(index % BUCKETS as u64) as usize];

        if bucket.0 > index {
            return;
        }

        if bucket.0 != index {
            *bucket = (index, 0);
        }

        bucket.1 = bucket.1.saturating_add(count);
    }

    pub(crate) fn count(&self, now: Instant) -> usize {
        let Some(anchor) = self.anchor else {
            return 0;
        };

        let index = now.saturating_duration_since(anchor).as_secs() / BUCKET.as_secs();

        self.buckets
            .iter()
            .filter(|(bucket, _)| *bucket + BUCKETS as u64 > index)
            .fold(0usize, |total, (_, count)| total.saturating_add(*count))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn counts_a_trailing_hour_and_ignores_zero() {
        let start = Instant::now();
        let mut counter = RollingCounter::default();

        counter.record(0, start);
        assert_eq!(counter.count(start), 0);

        counter.record(2, start);
        counter.record(3, start + secs(30 * 60));

        assert_eq!(counter.count(start + ROLLING_COUNTER_WINDOW - secs(1)), 5);
        assert_eq!(counter.count(start + ROLLING_COUNTER_WINDOW), 3);
        assert_eq!(
            counter.count(start + ROLLING_COUNTER_WINDOW + secs(30 * 60)),
            0
        );
    }

    #[test]
    fn reusing_a_bucket_discards_the_stale_count() {
        let start = Instant::now();
        let mut counter = RollingCounter::default();

        counter.record(1, start);
        counter.record(3, start + secs(60));
        counter.record(5, start + secs(60 * 61));

        assert_eq!(counter.count(start + secs(60 * 61)), 5);
    }

    #[test]
    fn accumulates_many_records() {
        let start = Instant::now();
        let mut counter = RollingCounter::default();

        for i in 0..10_000u64 {
            counter.record(1, start + secs(i % 3_540));
        }

        assert_eq!(counter.count(start + secs(3_540)), 10_000);
    }

    #[test]
    fn shared_origin_counters_expire_the_same_bucket_together() {
        let origin = Instant::now();
        let mut connects = RollingCounter::new(origin);
        let mut sessions = RollingCounter::new(origin);

        connects.record(1, origin);
        sessions.record(1, origin + secs(10));

        let now = origin + ROLLING_COUNTER_WINDOW;
        assert_eq!(connects.count(now), 0);
        assert_eq!(sessions.count(now), 0);
    }
}
