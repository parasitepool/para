use super::*;

/// Maximum ratio where `1 - e^(-x)` is distinguishable from 1.0.
/// Beyond this, `e^(-x) < f64::EPSILON` and the subtraction rounds to exactly 1.0.
/// Derived from: `-ln(f64::EPSILON) ≈ 36.04`
const EXP_SATURATION_LIMIT: f64 = 36.0;

/// Minimum time before considering difficulty adjustment, as a fraction of the window.
/// Derived from ckpool: 240s min_time / 300s window = 0.8
const MIN_TIME_WINDOW_RATIO: f64 = 0.8;

/// Minimum shares before considering adjustment, as a multiple of expected shares per window.
/// Derived from ckpool: 72 shares / (300s window / 5s period) = 72 / 60 = 1.2
const MIN_SHARES_WINDOW_RATIO: f64 = 1.2;

/// Lower hysteresis bound: don't decrease difficulty unless rate drops below this fraction of target.
/// From ckpool.
const HYSTERESIS_LOW: f64 = 0.5;

/// Upper hysteresis bound: don't increase difficulty unless rate exceeds this fraction of target.
/// From ckpool.
const HYSTERESIS_HIGH: f64 = 1.33;

/// Computes `1 - e^(-x)` with numerical stability.
/// Returns 0.0 at x=0, approaches 1.0 as x approaches infinity.
/// Uses `exp_m1` for accuracy with small x, caps input at [`EXP_SATURATION_LIMIT`].
fn exponential_fill_fraction(x: f64) -> f64 {
    -(-x.min(EXP_SATURATION_LIMIT)).exp_m1()
}

/// Calculates time bias based on how much history we have.
/// Returns a value approaching 1.0 as elapsed time exceeds the window.
fn calculate_time_bias(elapsed: Duration, window: Duration) -> f64 {
    exponential_fill_fraction(elapsed.as_secs_f64() / window.as_secs_f64())
}

#[derive(Debug, Clone)]
pub struct DecayingAverage {
    value: f64,
    window: Duration,
    last_update: Instant,
}

impl DecayingAverage {
    pub fn new(window: Duration) -> Self {
        Self {
            value: 0.0,
            window,
            last_update: Instant::now(),
        }
    }

    #[cfg(test)]
    fn with_start_time(window: Duration, start: Instant) -> Self {
        Self {
            value: 0.0,
            window,
            last_update: start,
        }
    }

    pub fn record(&mut self, sample: f64, now: Instant) {
        let elapsed = now.duration_since(self.last_update).as_secs_f64();
        if elapsed <= 0.0 {
            return;
        }

        let window_secs = self.window.as_secs_f64();
        let decay_factor = exponential_fill_fraction(elapsed / window_secs);
        let normalizer = 1.0 + decay_factor;

        self.value = (self.value + (sample / elapsed) * decay_factor) / normalizer;
        self.last_update = now;
    }

    pub fn value(&self) -> f64 {
        self.value
    }
}

/// Tracks timing for vardiff decisions.
#[derive(Debug, Clone)]
struct Timing {
    first_share: Instant,
    last_diff_change: Instant,
}

/// Variable difficulty state for a miner connection.
#[derive(Debug, Clone)]
pub struct Vardiff {
    target_interval: Duration,
    window: Duration,
    min_shares_for_adjustment: u32,
    min_time_for_adjustment: Duration,
    dsps: DecayingAverage,
    current_diff: Difficulty,
    old_diff: Difficulty,
    timing: Option<Timing>,
    shares_since_change: u32,
}

impl Vardiff {
    /// Creates a new vardiff tracker.
    pub fn new(target_interval: Duration, window: Duration, start_diff: Difficulty) -> Self {
        let target_secs = target_interval.as_secs_f64();
        let window_secs = window.as_secs_f64();
        let expected_shares_per_window = window_secs / target_secs;

        Self {
            target_interval,
            window,
            min_shares_for_adjustment: (expected_shares_per_window * MIN_SHARES_WINDOW_RATIO)
                as u32,
            min_time_for_adjustment: Duration::from_secs_f64(window_secs * MIN_TIME_WINDOW_RATIO),
            dsps: DecayingAverage::new(window),
            current_diff: start_diff,
            old_diff: start_diff,
            timing: None,
            shares_since_change: 0,
        }
    }

    /// Target share rate (shares per second at difficulty 1).
    fn target_rate(&self) -> f64 {
        1.0 / self.target_interval.as_secs_f64()
    }

    /// Returns the current difficulty.
    pub fn current_diff(&self) -> Difficulty {
        self.current_diff
    }

    /// Returns the current difficulty-weighted shares per second.
    pub fn dsps(&self) -> f64 {
        self.dsps.value()
    }

    /// Returns the number of shares since the last difficulty change.
    pub fn shares_since_change(&self) -> u32 {
        self.shares_since_change
    }

    /// Records a share and returns a new difficulty if adjustment is needed.
    pub fn record_share(
        &mut self,
        share_diff: Difficulty,
        network_diff: Difficulty,
    ) -> Option<Difficulty> {
        let now = Instant::now();

        // Initialize timing on first share
        if self.timing.is_none() {
            self.timing = Some(Timing {
                first_share: now,
                last_diff_change: now,
            });
        }

        self.dsps.record(share_diff.as_f64(), now);
        self.shares_since_change = self.shares_since_change.saturating_add(1);

        self.evaluate_adjustment(network_diff, now)
    }

    /// Evaluates whether difficulty should be adjusted.
    fn evaluate_adjustment(
        &mut self,
        network_diff: Difficulty,
        now: Instant,
    ) -> Option<Difficulty> {
        let timing = self.timing.as_ref()?;

        let time_since_first = now.duration_since(timing.first_share);
        let time_since_change = now.duration_since(timing.last_diff_change);

        // Check if we have enough data to make a decision
        if !self.ready_for_evaluation(time_since_change) {
            return None;
        }

        let metrics = self.calculate_metrics(time_since_first);

        debug!(
            "Vardiff: evaluating | dsps={:.6} bias={:.4} drr={:.4} target={:.4} range=[{:.4}, {:.4}]",
            metrics.dsps,
            metrics.bias,
            metrics.diff_rate_ratio,
            self.target_rate(),
            metrics.low_threshold,
            metrics.high_threshold
        );

        if metrics.is_within_hysteresis() {
            debug!("Vardiff within hysteresis band, no adjustment needed");
            return None;
        }

        self.calculate_new_difficulty(metrics, network_diff, now)
    }

    fn ready_for_evaluation(&self, time_since_change: Duration) -> bool {
        let enough_shares = self.shares_since_change >= self.min_shares_for_adjustment;
        let enough_time = time_since_change >= self.min_time_for_adjustment;

        if !enough_shares && !enough_time {
            debug!(
                "Skipping vardiff adjustment (shares={}/{} time={:.1}s/{:.1}s)",
                self.shares_since_change,
                self.min_shares_for_adjustment,
                time_since_change.as_secs_f64(),
                self.min_time_for_adjustment.as_secs_f64()
            );

            return false;
        }
        true
    }

    fn calculate_metrics(&self, time_since_first: Duration) -> Metrics {
        let bias = calculate_time_bias(time_since_first, self.window);
        let dsps = self.dsps.value() / bias;
        let current_diff = self.current_diff.as_f64();
        let diff_rate_ratio = dsps / current_diff;
        let target_rate = self.target_rate();

        Metrics {
            dsps,
            bias,
            diff_rate_ratio,
            low_threshold: target_rate * HYSTERESIS_LOW,
            high_threshold: target_rate * HYSTERESIS_HIGH,
        }
    }

    /// Calculates and applies new difficulty if appropriate.
    fn calculate_new_difficulty(
        &mut self,
        metrics: Metrics,
        network_diff: Difficulty,
        now: Instant,
    ) -> Option<Difficulty> {
        // Calculate optimal difficulty: dsps * target_interval
        let optimal = metrics.dsps * self.target_interval.as_secs_f64();

        let min_diff = 0.0;
        let max_diff = network_diff.as_f64();
        let clamped = optimal.clamp(min_diff, max_diff);

        debug!(
            "Vardiff: optimal={:.6} clamped={:.6} (min={:.6}, max={:.6})",
            optimal, clamped, min_diff, max_diff
        );

        if clamped <= 0.0 {
            debug!("Skipping vardiff: invalid clamped value");
            return None;
        }

        let new_diff = Difficulty::from(clamped);

        if self.current_diff == new_diff {
            debug!("Vardiff already at optimal difficulty {}", new_diff);
            return None;
        }

        // Guard against oscillation on difficulty decrease
        if new_diff < self.current_diff && self.shares_since_change == 1 {
            debug!("Vardiff: first share after potential decrease, deferring");
            if let Some(ref mut timing) = self.timing {
                timing.last_diff_change = now;
            }
            return None;
        }

        debug!(
            "Vardiff: adjusting {} -> {} (drr={:.4} outside [{:.4}, {:.4}])",
            self.current_diff,
            new_diff,
            metrics.diff_rate_ratio,
            metrics.low_threshold,
            metrics.high_threshold
        );

        self.apply_difficulty_change(new_diff, now);
        Some(new_diff)
    }

    /// Applies a difficulty change and resets tracking state.
    fn apply_difficulty_change(&mut self, new_diff: Difficulty, now: Instant) {
        self.old_diff = self.current_diff;
        self.current_diff = new_diff;
        self.shares_since_change = 0;
        if let Some(ref mut timing) = self.timing {
            timing.last_diff_change = now;
        }
    }
}

impl Default for Vardiff {
    fn default() -> Self {
        Self::new(
            Duration::from_secs(5),
            Duration::from_secs(300),
            Difficulty::from(1),
        )
    }
}

/// Metrics used for difficulty evaluation.
struct Metrics {
    dsps: f64,
    bias: f64,
    diff_rate_ratio: f64,
    low_threshold: f64,
    high_threshold: f64,
}

impl Metrics {
    fn is_within_hysteresis(&self) -> bool {
        self.diff_rate_ratio > self.low_threshold && self.diff_rate_ratio < self.high_threshold
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
    fn starts_at_zero() {
        let avg = DecayingAverage::new(secs(300));
        assert_eq!(avg.value(), 0.0);
    }

    #[test]
    fn increases_with_samples() {
        let start = Instant::now();
        let mut avg = DecayingAverage::with_start_time(secs(60), start);

        avg.record(60.0, start + secs(1));

        assert!(avg.value() > 0.0);
        assert!(avg.value() < 60.0);
    }

    #[test]
    fn decays_over_time() {
        let start = Instant::now();
        let mut avg = DecayingAverage::with_start_time(secs(60), start);

        avg.record(100.0, start + secs(1));
        let initial = avg.value();

        // Decay by recording zero
        avg.record(0.0, start + secs(31));
        assert!(avg.value() < initial);
    }

    #[test]
    fn stabilizes_with_constant_input() {
        let start = Instant::now();
        let mut avg = DecayingAverage::with_start_time(secs(60), start);

        let mut t = start;
        for _ in 0..120 {
            t += secs(1);
            avg.record(10.0, t);
        }

        let value = avg.value();
        assert!((8.0..12.0).contains(&value), "Expected ~10, got {}", value);
    }

    #[test]
    fn ignores_zero_elapsed_time() {
        let start = Instant::now();
        let mut avg = DecayingAverage::with_start_time(secs(60), start);

        avg.record(100.0, start);
        assert_eq!(avg.value(), 0.0);
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
        let vardiff = Vardiff::new(secs(5), secs(300), Difficulty::from(10));
        assert_eq!(vardiff.current_diff(), Difficulty::from(10));
    }

    #[test]
    fn no_change_on_first_share() {
        let mut vardiff = Vardiff::new(secs(5), secs(300), Difficulty::from(10));
        let result = vardiff.record_share(Difficulty::from(10), Difficulty::from(1_000_000));
        assert!(result.is_none());
    }

    #[test]
    fn respects_min_shares_threshold() {
        let mut vardiff = Vardiff::new(secs(5), secs(300), Difficulty::from(10));

        for _ in 0..10 {
            let result = vardiff.record_share(Difficulty::from(10), Difficulty::from(1_000_000));
            assert!(result.is_none(), "Should not adjust with few shares");
        }
    }

    #[test]
    fn stats_reflect_current_state() {
        let mut vardiff = Vardiff::default();

        assert_eq!(vardiff.shares_since_change, 0);

        vardiff.record_share(Difficulty::from(42), Difficulty::from(1_000_000));
        assert_eq!(vardiff.shares_since_change, 1);
    }

    #[test]
    fn increases_difficulty_for_fast_shares() {
        let start_diff = Difficulty::from(10);
        let mut vardiff = Vardiff::new(secs(5), secs(10), start_diff);

        // Simulate fast share submission
        let past = Instant::now() - secs(300);
        vardiff.timing = Some(Timing {
            first_share: past,
            last_diff_change: past,
        });
        vardiff.dsps = DecayingAverage::with_start_time(secs(10), past);

        let mut t = past;
        for _ in 0..100 {
            t += millis(100);
            vardiff.dsps.record(10.0, t);
            vardiff.shares_since_change += 1;
        }

        if let Some(new_diff) = vardiff.evaluate_adjustment(Difficulty::from(1_000_000), t) {
            assert!(new_diff > start_diff);
        }
    }

    #[test]
    fn respects_network_diff_ceiling() {
        let mut vardiff = Vardiff::new(secs(5), secs(10), Difficulty::from(10));

        let past = Instant::now() - secs(300);
        vardiff.timing = Some(Timing {
            first_share: past,
            last_diff_change: past,
        });
        vardiff.dsps = DecayingAverage::with_start_time(secs(10), past);

        let mut t = past;
        for _ in 0..100 {
            t += millis(10);
            vardiff.dsps.record(10.0, t);
            vardiff.shares_since_change += 1;
        }

        let network_diff = Difficulty::from(100);
        if let Some(new_diff) = vardiff.evaluate_adjustment(network_diff, t) {
            assert!(
                new_diff.as_f64() <= network_diff.as_f64() * 1.01,
                "Difficulty exceeded network_diff"
            );
        }
    }

    #[test]
    fn min_shares_derived_from_window_ratio() {
        // min_shares = (window / period) * 1.2

        // 60s window, 1s period → 60 expected shares → 72 min
        let vardiff = Vardiff::new(secs(1), secs(60), Difficulty::from(1));
        assert_eq!(vardiff.min_shares_for_adjustment, 72);

        // 300s window, 5s period → 60 expected shares → 72 min (ckpool default)
        let vardiff = Vardiff::new(secs(5), secs(300), Difficulty::from(1));
        assert_eq!(vardiff.min_shares_for_adjustment, 72);

        // 2s window, 1s period → 2 expected shares → 2.4 → 2
        let vardiff = Vardiff::new(secs(1), secs(2), Difficulty::from(1));
        assert_eq!(vardiff.min_shares_for_adjustment, 2);
    }

    #[test]
    fn min_time_derived_from_window_ratio() {
        // min_time = window * 0.8

        // 300s window → 240s min_time (ckpool default)
        let vardiff = Vardiff::new(secs(5), secs(300), Difficulty::from(1));
        assert_eq!(vardiff.min_time_for_adjustment, secs(240));

        // 60s window → 48s min_time
        let vardiff = Vardiff::new(secs(1), secs(60), Difficulty::from(1));
        assert_eq!(vardiff.min_time_for_adjustment, secs(48));

        // 10s window → 8s min_time
        let vardiff = Vardiff::new(secs(1), secs(10), Difficulty::from(1));
        assert_eq!(vardiff.min_time_for_adjustment, secs(8));
    }
}
