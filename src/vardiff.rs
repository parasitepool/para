use super::*;

/// Minimum window ratio before considering adjustment.
/// Fraction of expected time (or shares) per window.
/// Derived from ckpool: 240s / 300s window = 0.8
const MIN_WINDOW_RATIO: f64 = 0.8;

/// Only decrease difficulty when rate drops below this fraction of target.
/// Copied from ckpool.
const HYSTERESIS_LOW: f64 = 0.5;

/// Only increase difficulty when rate exceeds this fraction of target.
/// Copied from ckpool.
const HYSTERESIS_HIGH: f64 = 1.33;

#[derive(Debug, Clone)]
pub(crate) struct Vardiff {
    period: Duration,
    window: Duration,
    min_shares_for_adjustment: u32,
    min_time_for_adjustment: Duration,
    dsps: DecayingAverage,
    current_diff: Difficulty,
    old_diff: Difficulty,
    first_share: Option<Instant>,
    last_diff_change: Instant,
    shares_since_change: u32,
    min_diff: Option<Difficulty>,
    max_diff: Option<Difficulty>,
    diff_change_job_id: Option<JobId>,
}

impl Vardiff {
    pub(crate) fn new(
        start_diff: Difficulty,
        period: Duration,
        window: Duration,
        min_diff: Option<Difficulty>,
        max_diff: Option<Difficulty>,
    ) -> Self {
        let window_secs = window.as_secs_f64();
        let expected_shares_per_window = window_secs / period.as_secs_f64();

        Self {
            period,
            window,
            min_shares_for_adjustment: (expected_shares_per_window * MIN_WINDOW_RATIO) as u32,
            min_time_for_adjustment: Duration::from_secs_f64(window_secs * MIN_WINDOW_RATIO),
            dsps: DecayingAverage::new(window),
            current_diff: start_diff,
            old_diff: start_diff,
            first_share: None,
            last_diff_change: Instant::now(),
            shares_since_change: 0,
            min_diff,
            max_diff,
            diff_change_job_id: None,
        }
    }

    fn clamp_difficulty(&self, diff: Difficulty, upstream_diff: Option<Difficulty>) -> Difficulty {
        let mut result = diff;

        if let Some(min) = self.min_diff {
            result = result.max(min);
        }

        if let Some(upstream) = upstream_diff {
            result = result.min(upstream);
        }

        if let Some(max) = self.max_diff {
            result = result.min(max);
        }
        result
    }

    fn target_rate(&self) -> f64 {
        1.0 / self.period.as_secs_f64()
    }

    pub(crate) fn current_diff(&self) -> Difficulty {
        self.current_diff
    }

    pub(crate) fn old_diff(&self) -> Difficulty {
        self.old_diff
    }

    pub(crate) fn diff_change_job_id(&self) -> Option<JobId> {
        self.diff_change_job_id
    }

    pub(crate) fn record_diff_change_job_id(&mut self, next_job_id: JobId) {
        self.diff_change_job_id = Some(next_job_id);
    }

    pub(crate) fn pool_diff(&self, job_id: JobId) -> Difficulty {
        let stale = self
            .diff_change_job_id
            .is_some_and(|change_id| job_id < change_id);

        if stale {
            self.old_diff.min(self.current_diff)
        } else {
            self.current_diff
        }
    }

    pub(crate) fn clamp_to_upstream(&mut self, upstream_diff: Difficulty) -> Option<Difficulty> {
        if upstream_diff < self.current_diff {
            self.old_diff = self.old_diff.min(self.current_diff);
            self.current_diff = upstream_diff;
            self.shares_since_change = 0;
            self.last_diff_change = Instant::now();

            return Some(upstream_diff);
        }

        None
    }

    pub(crate) fn dsps(&self) -> f64 {
        self.dsps.value_at(Instant::now())
    }

    pub(crate) fn shares_since_change(&self) -> u32 {
        self.shares_since_change
    }

    pub(crate) fn record_share(
        &mut self,
        pool_diff: Difficulty,
        network_diff: Difficulty,
        upstream_diff: Option<Difficulty>,
    ) -> Option<Difficulty> {
        if pool_diff != self.current_diff {
            return None;
        }

        let now = Instant::now();

        if self.first_share.is_none() {
            self.first_share = Some(now);
            self.last_diff_change = now;
        }

        self.dsps.record(pool_diff.as_f64(), now);
        self.shares_since_change = self.shares_since_change.saturating_add(1);

        self.evaluate_adjustment(network_diff, upstream_diff, now)
    }

    fn evaluate_adjustment(
        &mut self,
        network_diff: Difficulty,
        upstream_diff: Option<Difficulty>,
        now: Instant,
    ) -> Option<Difficulty> {
        let first_share = self.first_share?;
        let time_since_first = now.duration_since(first_share);
        let time_since_change = now.duration_since(self.last_diff_change);
        let enough_shares = self.shares_since_change >= self.min_shares_for_adjustment;
        let enough_time = time_since_change >= self.min_time_for_adjustment;

        if !enough_shares && !enough_time {
            debug!(
                "Skipping vardiff (shares={}/{} time={:.1}s/{:.1}s)",
                self.shares_since_change,
                self.min_shares_for_adjustment,
                time_since_change.as_secs_f64(),
                self.min_time_for_adjustment.as_secs_f64()
            );
            return None;
        }

        let bias = calculate_time_bias(time_since_first, self.window);
        let dsps = self.dsps.value_at(now) / bias;
        let diff_rate_ratio = dsps / self.current_diff.as_f64();
        let target_rate = self.target_rate();
        let low_threshold = target_rate * HYSTERESIS_LOW;
        let high_threshold = target_rate * HYSTERESIS_HIGH;

        debug!(
            "Vardiff: dsps={:.6} bias={:.4} drr={:.4} target={:.4} range=[{:.4}, {:.4}]",
            dsps, bias, diff_rate_ratio, target_rate, low_threshold, high_threshold
        );

        if diff_rate_ratio > low_threshold && diff_rate_ratio < high_threshold {
            debug!("Vardiff within hysteresis band");
            return None;
        }

        let optimal = dsps * self.period.as_secs_f64();
        assert!(optimal > 0.0, "optimal difficulty must be positive");

        let new_diff = Difficulty::from(optimal.min(network_diff.as_f64()));

        let new_diff = {
            let clamped = self.clamp_difficulty(new_diff, upstream_diff);
            if clamped != new_diff {
                debug!(
                    "Vardiff clamped {} -> {} (min={:?}, upstream={:?}, max={:?})",
                    new_diff, clamped, self.min_diff, upstream_diff, self.max_diff
                );
            }
            clamped
        };

        if self.current_diff == new_diff {
            return None;
        }

        if new_diff < self.current_diff && self.shares_since_change == 1 {
            debug!(
                "Guarding against oscillation on difficulty decrease after first share since adjustment"
            );

            self.last_diff_change = now;
            return None;
        }

        debug!(
            "Vardiff: {} -> {} (drr={:.4} outside [{:.4}, {:.4}])",
            self.current_diff, new_diff, diff_rate_ratio, low_threshold, high_threshold
        );

        self.old_diff = self.old_diff.min(self.current_diff);
        self.current_diff = new_diff;
        self.shares_since_change = 0;
        self.last_diff_change = now;

        Some(new_diff)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    fn millis(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    #[test]
    fn time_bias_starts_low() {
        let bias = calculate_time_bias(secs(1), secs(300));
        assert!(bias < 0.01, "Expected low bias, got {}", bias);
    }

    #[test]
    fn time_bias_approaches_one() {
        let bias = calculate_time_bias(secs(3000), secs(300));
        assert!(bias > 0.99, "Expected high bias, got {}", bias);
    }

    #[test]
    fn time_bias_moderate_at_half_window() {
        let bias = calculate_time_bias(secs(150), secs(300));
        assert!(
            (0.3..0.5).contains(&bias),
            "Expected moderate bias, got {}",
            bias
        );
    }

    #[test]
    fn tracks_initial_difficulty() {
        let vardiff = Vardiff::new(Difficulty::from(10), secs(5), secs(300), None, None);
        assert_eq!(vardiff.current_diff(), Difficulty::from(10));
    }

    #[test]
    fn no_change_on_first_share() {
        let mut vardiff = Vardiff::new(Difficulty::from(10), secs(5), secs(300), None, None);
        let result = vardiff.record_share(Difficulty::from(10), Difficulty::from(1_000_000), None);
        assert!(result.is_none());
    }

    #[test]
    fn respects_min_shares_threshold() {
        let mut vardiff = Vardiff::new(Difficulty::from(10), secs(5), secs(300), None, None);

        for _ in 0..10 {
            let result =
                vardiff.record_share(Difficulty::from(10), Difficulty::from(1_000_000), None);
            assert!(result.is_none(), "Should not adjust with few shares");
        }
    }

    #[test]
    fn increases_difficulty_for_fast_shares() {
        let start_diff = Difficulty::from(10);
        let mut vardiff = Vardiff::new(start_diff, secs(5), secs(10), None, None);

        let base = Instant::now();
        vardiff.first_share = Some(base);
        vardiff.last_diff_change = base;
        vardiff.dsps = DecayingAverage::with_start_time(secs(10), base);

        let mut t = base;
        for _ in 0..100 {
            t += millis(100);
            vardiff.dsps.record(10.0, t);
            vardiff.shares_since_change += 1;
        }

        if let Some(new_diff) = vardiff.evaluate_adjustment(Difficulty::from(1_000_000), None, t) {
            assert!(new_diff > start_diff);
        }
    }

    #[test]
    fn respects_network_diff_ceiling() {
        let mut vardiff = Vardiff::new(Difficulty::from(10), secs(5), secs(10), None, None);

        let base = Instant::now();
        vardiff.first_share = Some(base);
        vardiff.last_diff_change = base;
        vardiff.dsps = DecayingAverage::with_start_time(secs(10), base);

        let mut t = base;
        for _ in 0..100 {
            t += millis(10);
            vardiff.dsps.record(10.0, t);
            vardiff.shares_since_change += 1;
        }

        let network_diff = Difficulty::from(100);
        if let Some(new_diff) = vardiff.evaluate_adjustment(network_diff, None, t) {
            assert!(
                new_diff.as_f64() <= network_diff.as_f64() * 1.01,
                "Difficulty exceeded network_diff"
            );
        }
    }

    #[test]
    fn min_shares_derived_from_window_ratio() {
        let vardiff = Vardiff::new(Difficulty::from(1), secs(1), secs(60), None, None);
        assert_eq!(vardiff.min_shares_for_adjustment, 48);

        let vardiff = Vardiff::new(Difficulty::from(1), secs(1), secs(2), None, None);
        assert_eq!(vardiff.min_shares_for_adjustment, 1);
    }

    #[test]
    fn min_time_derived_from_window_ratio() {
        let vardiff = Vardiff::new(Difficulty::from(1), secs(5), secs(300), None, None);
        assert_eq!(vardiff.min_time_for_adjustment, secs(240));

        let vardiff = Vardiff::new(Difficulty::from(1), secs(1), secs(60), None, None);
        assert_eq!(vardiff.min_time_for_adjustment, secs(48));

        let vardiff = Vardiff::new(Difficulty::from(1), secs(1), secs(10), None, None);
        assert_eq!(vardiff.min_time_for_adjustment, secs(8));
    }

    #[test]
    fn respects_min_diff_floor() {
        let min_diff = Difficulty::from(5);
        let mut vardiff = Vardiff::new(
            Difficulty::from(10),
            secs(5),
            secs(10),
            Some(min_diff),
            None,
        );

        let base = Instant::now();
        vardiff.first_share = Some(base);
        vardiff.last_diff_change = base;
        vardiff.dsps = DecayingAverage::with_start_time(secs(10), base);

        let mut t = base;
        for _ in 0..100 {
            t += secs(10);
            vardiff.dsps.record(0.1, t);
            vardiff.shares_since_change += 1;
        }

        if let Some(new_diff) = vardiff.evaluate_adjustment(Difficulty::from(1_000_000), None, t) {
            assert!(
                new_diff >= min_diff,
                "Difficulty {} should not go below min_diff {}",
                new_diff,
                min_diff
            );
        }
    }

    #[test]
    fn respects_max_diff_ceiling() {
        let max_diff = Difficulty::from(50);
        let mut vardiff = Vardiff::new(
            Difficulty::from(10),
            secs(5),
            secs(10),
            None,
            Some(max_diff),
        );

        let base = Instant::now();
        vardiff.first_share = Some(base);
        vardiff.last_diff_change = base;
        vardiff.dsps = DecayingAverage::with_start_time(secs(10), base);

        let mut t = base;
        for _ in 0..100 {
            t += millis(10);
            vardiff.dsps.record(100.0, t);
            vardiff.shares_since_change += 1;
        }

        if let Some(new_diff) = vardiff.evaluate_adjustment(Difficulty::from(1_000_000), None, t) {
            assert!(
                new_diff <= max_diff,
                "Difficulty {} should not exceed max_diff {}",
                new_diff,
                max_diff
            );
        }
    }

    #[test]
    fn clamp_to_upstream_lowers_difficulty() {
        let start_diff = Difficulty::from(100);
        let mut vardiff = Vardiff::new(start_diff, secs(5), secs(300), None, None);

        let upstream_diff = Difficulty::from(50);
        let result = vardiff.clamp_to_upstream(upstream_diff);

        assert!(
            result.is_some(),
            "Should return new difficulty when clamping"
        );
        assert_eq!(result.unwrap(), upstream_diff);
        assert_eq!(vardiff.current_diff(), upstream_diff);
    }

    #[test]
    fn clamp_to_upstream_ignores_increase() {
        let start_diff = Difficulty::from(50);
        let mut vardiff = Vardiff::new(start_diff, secs(5), secs(300), None, None);

        let upstream_diff = Difficulty::from(100);
        let result = vardiff.clamp_to_upstream(upstream_diff);

        assert!(
            result.is_none(),
            "Should return None when upstream is higher"
        );
        assert_eq!(vardiff.current_diff(), start_diff);
    }

    #[test]
    fn clamp_to_upstream_resets_shares_since_change() {
        let start_diff = Difficulty::from(100);
        let mut vardiff = Vardiff::new(start_diff, secs(5), secs(300), None, None);

        vardiff.first_share = Some(Instant::now());
        vardiff.shares_since_change = 50;

        let upstream_diff = Difficulty::from(50);
        vardiff.clamp_to_upstream(upstream_diff);

        assert_eq!(
            vardiff.shares_since_change, 0,
            "Should reset shares_since_change"
        );
    }

    #[test]
    fn decreases_difficulty_for_slow_shares() {
        let start_diff = Difficulty::from(100);
        let mut vardiff = Vardiff::new(start_diff, secs(5), secs(10), None, None);

        let base = Instant::now();
        vardiff.first_share = Some(base);
        vardiff.last_diff_change = base;
        vardiff.dsps = DecayingAverage::with_start_time(secs(10), base);

        let mut t = base;
        for _ in 0..100 {
            t += secs(10);
            vardiff.dsps.record(1.0, t);
            vardiff.shares_since_change += 1;
        }

        if let Some(new_diff) = vardiff.evaluate_adjustment(Difficulty::from(1_000_000), None, t) {
            assert!(
                new_diff < start_diff,
                "Difficulty {} should decrease from {}",
                new_diff,
                start_diff
            );
        }
    }

    #[test]
    fn no_change_within_hysteresis_band() {
        let start_diff = Difficulty::from(10);
        let mut vardiff = Vardiff::new(start_diff, secs(5), secs(10), None, None);

        let base = Instant::now();
        vardiff.first_share = Some(base);
        vardiff.last_diff_change = base;
        vardiff.dsps = DecayingAverage::with_start_time(secs(10), base);

        let mut t = base;
        for _ in 0..100 {
            t += secs(5);
            vardiff.dsps.record(10.0, t);
            vardiff.shares_since_change += 1;
        }

        let result = vardiff.evaluate_adjustment(Difficulty::from(1_000_000), None, t);
        assert!(
            result.is_none(),
            "Should not adjust when drr is within hysteresis band"
        );
    }

    #[test]
    fn oscillation_guard_on_decrease() {
        let start_diff = Difficulty::from(100);
        let mut vardiff = Vardiff::new(start_diff, secs(5), secs(10), None, None);

        let base = Instant::now();
        vardiff.first_share = Some(base);
        vardiff.last_diff_change = base;
        vardiff.dsps = DecayingAverage::with_start_time(secs(10), base);

        let t = base + secs(100);
        for _ in 0..100 {
            vardiff.dsps.record(1.0, t);
        }
        vardiff.shares_since_change = 1;

        let result = vardiff.evaluate_adjustment(Difficulty::from(1_000_000), None, t);
        assert!(
            result.is_none(),
            "Should not decrease on first share after change"
        );
        assert_eq!(vardiff.current_diff(), start_diff);
    }

    #[test]
    fn min_shares_matches_ckpool_at_runtime_default() {
        let vardiff = Vardiff::new(
            Difficulty::from(1),
            Duration::from_secs_f64(3.33),
            secs(300),
            None,
            None,
        );
        assert_eq!(vardiff.min_shares_for_adjustment, 72);
    }

    #[test]
    fn pool_diff_uses_old_after_increase() {
        let start_diff = Difficulty::from(100);
        let mut vardiff = Vardiff::new(start_diff, secs(5), secs(10), None, None);

        let base = Instant::now();
        vardiff.first_share = Some(base);
        vardiff.last_diff_change = base;
        vardiff.dsps = DecayingAverage::with_start_time(secs(10), base);

        let mut t = base;
        for _ in 0..100 {
            t += millis(100);
            vardiff.dsps.record(100.0, t);
            vardiff.shares_since_change += 1;
        }

        let new_diff = vardiff
            .evaluate_adjustment(Difficulty::from(1_000_000), None, t)
            .unwrap();

        assert!(new_diff > start_diff);

        vardiff.record_diff_change_job_id(JobId::new(5));

        assert_eq!(vardiff.pool_diff(JobId::new(4)), start_diff);
        assert_eq!(vardiff.pool_diff(JobId::new(5)), new_diff);
        assert_eq!(vardiff.pool_diff(JobId::new(6)), new_diff);
    }

    #[test]
    fn pool_diff_uses_min_after_decrease() {
        let start_diff = Difficulty::from(100);
        let mut vardiff = Vardiff::new(start_diff, secs(5), secs(300), None, None);

        vardiff.clamp_to_upstream(Difficulty::from(50));
        vardiff.record_diff_change_job_id(JobId::new(5));

        assert_eq!(vardiff.pool_diff(JobId::new(4)), Difficulty::from(50));
        assert_eq!(vardiff.pool_diff(JobId::new(5)), Difficulty::from(50));
    }

    #[test]
    fn pool_diff_no_change_returns_current() {
        let vardiff = Vardiff::new(Difficulty::from(100), secs(5), secs(300), None, None);

        assert_eq!(vardiff.pool_diff(JobId::new(0)), Difficulty::from(100));
        assert_eq!(
            vardiff.pool_diff(JobId::new(u64::MAX)),
            Difficulty::from(100)
        );
    }

    #[test]
    fn pool_diff_ratchets_past_original_after_multiple_increases() {
        let mut vardiff = Vardiff::new(Difficulty::from(10), secs(5), secs(10), None, None);

        let base = Instant::now();
        vardiff.first_share = Some(base);
        vardiff.last_diff_change = base;
        vardiff.dsps = DecayingAverage::with_start_time(secs(10), base);

        let mut t = base;
        for _ in 0..100 {
            t += millis(100);
            vardiff.dsps.record(100.0, t);
            vardiff.shares_since_change += 1;
        }

        let diff_a = vardiff
            .evaluate_adjustment(Difficulty::from(1_000_000), None, t)
            .unwrap();
        assert!(diff_a > Difficulty::from(10));
        vardiff.record_diff_change_job_id(JobId::new(5));

        for _ in 0..100 {
            t += millis(100);
            vardiff.dsps.record(diff_a.as_f64() * 10.0, t);
            vardiff.shares_since_change += 1;
        }

        let diff_b = vardiff
            .evaluate_adjustment(Difficulty::from(1_000_000), None, t)
            .unwrap();
        assert!(diff_b > diff_a);
        vardiff.record_diff_change_job_id(JobId::new(10));

        assert_eq!(vardiff.pool_diff(JobId::new(4)), Difficulty::from(10));
        assert_eq!(vardiff.pool_diff(JobId::new(9)), Difficulty::from(10));
        assert_eq!(vardiff.pool_diff(JobId::new(10)), diff_b);
    }

    #[test]
    fn pool_diff_boundary_advances_on_second_record() {
        let mut vardiff = Vardiff::new(Difficulty::from(100), secs(5), secs(300), None, None);

        vardiff.clamp_to_upstream(Difficulty::from(50));
        vardiff.record_diff_change_job_id(JobId::new(5));

        assert_eq!(vardiff.pool_diff(JobId::new(6)), Difficulty::from(50));

        vardiff.record_diff_change_job_id(JobId::new(10));

        assert_eq!(vardiff.pool_diff(JobId::new(6)), Difficulty::from(50));
        assert_eq!(vardiff.pool_diff(JobId::new(10)), Difficulty::from(50));
    }

    #[test]
    fn skips_stale_diff_shares() {
        let mut vardiff = Vardiff::new(Difficulty::from(100), secs(5), secs(300), None, None);

        vardiff.record_share(Difficulty::from(50), Difficulty::from(1_000_000), None);

        assert_eq!(vardiff.shares_since_change(), 0);
    }

    #[test]
    fn upstream_diff_clamps_optimal() {
        let mut vardiff = Vardiff::new(Difficulty::from(10), secs(5), secs(10), None, None);

        let base = Instant::now();
        vardiff.first_share = Some(base);
        vardiff.last_diff_change = base;
        vardiff.dsps = DecayingAverage::with_start_time(secs(10), base);

        let mut t = base;
        for _ in 0..100 {
            t += millis(10);
            vardiff.dsps.record(100.0, t);
            vardiff.shares_since_change += 1;
        }

        let upstream_diff = Difficulty::from(50);
        if let Some(new_diff) =
            vardiff.evaluate_adjustment(Difficulty::from(1_000_000), Some(upstream_diff), t)
        {
            assert!(
                new_diff <= upstream_diff,
                "Difficulty {} should not exceed upstream_diff {}",
                new_diff,
                upstream_diff
            );
        }
    }
}
