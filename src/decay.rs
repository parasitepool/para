use super::*;

/// Computes `1 - e^(-x)` with numerical stability.
/// Returns 0.0 at x=0, saturates to 1.0 as x increases.
/// Used for EMA warmup bias correction.
fn exponential_saturation(x: f64) -> f64 {
    // Maximum ratio where `1 - e^(-x)` is distinguishable from 1.0.
    // Beyond this, `e^(-x) < f64::EPSILON` and the subtraction rounds to exactly 1.0.
    // Derived from: `-ln(f64::EPSILON) = 36.04`
    -(-x.min(36.0)).exp_m1()
}

/// Calculates time bias based on how much history we have.
/// Returns a value approaching 1.0 as elapsed time exceeds the window.
pub(crate) fn calculate_time_bias(elapsed: Duration, window: Duration) -> f64 {
    assert!(!window.is_zero(), "window must be non-zero");
    exponential_saturation(elapsed.as_secs_f64() / window.as_secs_f64())
}

#[derive(Debug, Clone)]
pub(crate) struct DecayingAverage {
    value: f64,
    window: Duration,
    last_update: Instant,
}

impl DecayingAverage {
    pub(crate) fn new(window: Duration) -> Self {
        assert!(!window.is_zero(), "window must be non-zero");
        Self {
            value: 0.0,
            window,
            last_update: Instant::now(),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_start_time(window: Duration, start: Instant) -> Self {
        assert!(!window.is_zero(), "window must be non-zero");
        Self {
            value: 0.0,
            window,
            last_update: start,
        }
    }

    pub(crate) fn record(&mut self, sample: f64, now: Instant) {
        let elapsed = now
            .saturating_duration_since(self.last_update)
            .as_secs_f64();

        if elapsed <= 0.0 {
            return;
        }

        let window_secs = self.window.as_secs_f64();
        let decay_factor = exponential_saturation(elapsed / window_secs);
        let normalizer = 1.0 + decay_factor;

        self.value = (self.value + (sample / elapsed) * decay_factor) / normalizer;
        self.last_update = now;
    }

    #[must_use]
    pub(crate) fn value(&self) -> f64 {
        self.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn exponential_saturation_at_zero() {
        assert_eq!(exponential_saturation(0.0), 0.0);
    }

    #[test]
    fn exponential_saturation_at_one() {
        let result = exponential_saturation(1.0);
        assert!(
            (result - 0.632).abs() < 0.01,
            "expected ~0.632, got {result}"
        );
    }

    #[test]
    fn exponential_saturation_saturates_at_cap() {
        let at_cap = exponential_saturation(36.0);
        assert!((at_cap - 1.0).abs() < 1e-10, "expected ~1.0, got {at_cap}");
    }

    #[test]
    fn exponential_saturation_clamps_large_values() {
        let beyond_cap = exponential_saturation(100.0);
        assert!(
            (beyond_cap - 1.0).abs() < 1e-10,
            "expected ~1.0, got {beyond_cap}"
        );
    }

    #[test]
    fn time_bias_zero_elapsed() {
        let bias = calculate_time_bias(Duration::ZERO, secs(60));
        assert_eq!(bias, 0.0);
    }

    #[test]
    fn time_bias_approaches_one() {
        let bias = calculate_time_bias(secs(600), secs(60));
        assert!(bias > 0.99, "expected near 1.0, got {bias}");
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
    fn negative_samples_work() {
        let start = Instant::now();
        let mut avg = DecayingAverage::with_start_time(secs(60), start);

        avg.record(-50.0, start + secs(1));
        assert!(
            avg.value() < 0.0,
            "expected negative value, got {}",
            avg.value()
        );
    }

    #[test]
    fn large_elapsed_saturates_and_stays_finite() {
        let start = Instant::now();
        let mut avg = DecayingAverage::with_start_time(secs(1), start);

        avg.record(100.0, start + secs(1));
        avg.record(100.0, start + secs(1000));

        assert!(avg.value().is_finite(), "value should be finite");
    }

    #[test]
    #[should_panic(expected = "window must be non-zero")]
    fn zero_window_panics() {
        DecayingAverage::new(Duration::ZERO);
    }

    #[test]
    #[should_panic(expected = "window must be non-zero")]
    fn zero_window_with_start_time_panics() {
        DecayingAverage::with_start_time(Duration::ZERO, Instant::now());
    }
}
