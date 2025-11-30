use super::*;

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
        let decay_exp = (elapsed / window_secs).min(36.0);
        let decay_factor = 1.0 - (-decay_exp).exp();
        let normalizer = 1.0 + decay_factor;

        self.value = (self.value + (sample / elapsed) * decay_factor) / normalizer;
        self.last_update = now;
    }

    pub fn value(&self) -> f64 {
        self.value
    }
}

/// Configuration for the vardiff algorithm.
#[derive(Debug, Clone)]
pub struct VardiffConfig {
    /// Target time between share submissions
    pub target_interval: Duration,
    /// Time window for the rolling average
    pub window: Duration,
    /// Minimum shares before considering adjustment
    pub min_shares_for_adjustment: u32,
    /// Minimum time before considering adjustment
    pub min_time_for_adjustment: Duration,
    /// Lower bound of hysteresis band (as fraction of target rate)
    pub hysteresis_low: f64,
    /// Upper bound of hysteresis band (as fraction of target rate)
    pub hysteresis_high: f64,
}

impl VardiffConfig {
    pub fn new(target_interval: Duration, window: Duration) -> Self {
        let target_secs = target_interval.as_secs_f64();
        Self {
            target_interval,
            window,
            // Default thresholds based on ckpool: ~72 shares or ~240 seconds for 5s target
            min_shares_for_adjustment: (target_secs * 14.4) as u32,
            min_time_for_adjustment: Duration::from_secs_f64(target_secs * 48.0),
            // Hysteresis band: [0.5x, 1.33x] of target rate
            hysteresis_low: 0.5,
            hysteresis_high: 1.33,
        }
    }

    /// Target share rate (shares per second at difficulty 1).
    fn target_rate(&self) -> f64 {
        1.0 / self.target_interval.as_secs_f64()
    }
}

impl Default for VardiffConfig {
    fn default() -> Self {
        Self::new(
            Duration::from_secs(5),
            Duration::from_secs(300),
        )
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
    config: VardiffConfig,
    dsps: DecayingAverage,
    current_diff: Difficulty,
    old_diff: Difficulty,
    timing: Option<Timing>,
    shares_since_change: u32,
}

impl Vardiff {
    /// Creates a new vardiff tracker.
    pub fn new(config: VardiffConfig, start_diff: Difficulty) -> Self {
        Self {
            dsps: DecayingAverage::new(config.window),
            config,
            current_diff: start_diff,
            old_diff: start_diff,
            timing: None,
            shares_since_change: 0,
        }
    }

    /// Returns the current difficulty.
    pub fn current_diff(&self) -> Difficulty {
        self.current_diff
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
            self.config.target_rate(),
            metrics.low_threshold,
            metrics.high_threshold
        );

        // Check hysteresis - don't adjust if within acceptable range
        if metrics.is_within_hysteresis() {
            debug!("Vardiff: within hysteresis band, no adjustment needed");
            return None;
        }

        self.calculate_new_difficulty(metrics, network_diff, now)
    }

    /// Checks if enough shares/time have passed for evaluation.
    fn ready_for_evaluation(&self, time_since_change: Duration) -> bool {
        let enough_shares = self.shares_since_change >= self.config.min_shares_for_adjustment;
        let enough_time = time_since_change >= self.config.min_time_for_adjustment;

        if !enough_shares && !enough_time {
            debug!(
                "Vardiff: skipping (shares={}/{} time={:.1}s/{:.1}s)",
                self.shares_since_change,
                self.config.min_shares_for_adjustment,
                time_since_change.as_secs_f64(),
                self.config.min_time_for_adjustment.as_secs_f64()
            );
            return false;
        }
        true
    }

    /// Calculates current metrics for difficulty evaluation.
    fn calculate_metrics(&self, time_since_first: Duration) -> Metrics {
        let bias = calculate_time_bias(time_since_first, self.config.window);
        let dsps = self.dsps.value() / bias;
        let current_diff = self.current_diff.as_f64();
        let diff_rate_ratio = dsps / current_diff;
        let target_rate = self.config.target_rate();

        Metrics {
            dsps,
            bias,
            diff_rate_ratio,
            low_threshold: target_rate * self.config.hysteresis_low,
            high_threshold: target_rate * self.config.hysteresis_high,
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
        let optimal = metrics.dsps * self.config.target_interval.as_secs_f64();

        let min_diff = 0.0;
        let max_diff = network_diff.as_f64();
        let clamped = optimal.clamp(min_diff, max_diff);

        debug!(
            "Vardiff: optimal={:.6} clamped={:.6} (min={:.6}, max={:.6})",
            optimal, clamped, min_diff, max_diff
        );

        if clamped <= 0.0 {
            debug!("Vardiff: invalid clamped value, skipping");
            return None;
        }

        let new_diff = Difficulty::from(clamped);

        // No change if already at optimal
        if self.current_diff == new_diff {
            debug!("Vardiff: already at optimal difficulty {}", new_diff);
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

    /// Returns current statistics.
    pub fn stats(&self) -> VardiffStats {
        VardiffStats {
            dsps: self.dsps.value(),
            shares_since_change: self.shares_since_change,
        }
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

/// Statistics about vardiff state.
#[derive(Debug, Clone)]
pub struct VardiffStats {
    pub dsps: f64,
    pub shares_since_change: u32,
}

/// Calculates time bias based on how much history we have.
///
/// Returns a value approaching 1.0 as elapsed time exceeds the window.
fn calculate_time_bias(elapsed: Duration, window: Duration) -> f64 {
    let ratio = (elapsed.as_secs_f64() / window.as_secs_f64()).min(36.0);
    1.0 - (-ratio).exp()
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
    fn starts_low() {
        let bias = calculate_time_bias(secs(1), secs(300));
        assert!(bias < 0.01, "Expected low bias, got {}", bias);
    }

    #[test]
    fn approaches_one() {
        let bias = calculate_time_bias(secs(3000), secs(300));
        assert!(bias > 0.99, "Expected high bias, got {}", bias);
    }

    #[test]
    fn moderate_at_half_window() {
        let bias = calculate_time_bias(secs(150), secs(300));
        assert!(
            (0.3..0.5).contains(&bias),
            "Expected moderate bias, got {}",
            bias
        );
    }

    fn test_config() -> VardiffConfig {
        VardiffConfig::new(secs(5), secs(300))
    }

    #[test]
    fn tracks_initial_difficulty() {
        let vardiff = Vardiff::new(test_config(), Difficulty::from(10));
        assert_eq!(vardiff.current_diff(), Difficulty::from(10));
    }

    #[test]
    fn no_change_on_first_share() {
        let mut vardiff = Vardiff::new(test_config(), Difficulty::from(10));
        let result = vardiff.record_share(Difficulty::from(10), Difficulty::from(1_000_000));
        assert!(result.is_none());
    }

    #[test]
    fn respects_min_shares_threshold() {
        let mut vardiff = Vardiff::new(test_config(), Difficulty::from(10));

        for _ in 0..10 {
            let result = vardiff.record_share(Difficulty::from(10), Difficulty::from(1_000_000));
            assert!(result.is_none(), "Should not adjust with few shares");
        }
    }

    #[test]
    fn stats_reflect_current_state() {
        let mut vardiff = Vardiff::new(VardiffConfig::default(), Difficulty::from(42));

        let stats = vardiff.stats();
        assert_eq!(stats.shares_since_change, 0);

        vardiff.record_share(Difficulty::from(42), Difficulty::from(1_000_000));
        assert_eq!(vardiff.stats().shares_since_change, 1);
    }

    #[test]
    fn increases_difficulty_for_fast_shares() {
        let config = VardiffConfig::new(secs(5), secs(10));
        let start_diff = Difficulty::from(10);
        let mut vardiff = Vardiff::new(config, start_diff);

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
        let config = VardiffConfig::new( secs(5), secs(10));
        let mut vardiff = Vardiff::new(config, Difficulty::from(10));

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
}
