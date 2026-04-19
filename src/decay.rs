use super::*;

/// Computes `1 - e^(-x)` with numerical stability.
/// Returns 0.0 at x=0, saturates to 1.0 as x increases.
/// Used for EMA warmup bias correction.
fn exponential_saturation(x: f64) -> f64 {
    // Clamp at 40 so the result is exactly 1.0 for large inputs, not one ULP
    // shy of it. Saturation under round-to-nearest-even needs `e^(-x)` below
    // half-ULP at 1.0 (`2^-53 ≈ 1.11e-16`), which mathematically holds for
    // `x ≥ -ln(2^-53) ≈ 36.74`. In practice libm's `exp_m1` is ~1 ULP loose,
    // so it doesn't round to exactly `-1.0` until `x ≳ 37.5`; 40 gives margin.
    // Without this, callers doing `1 - exponential_saturation(x)` stay stuck
    // at the residue times the stored value instead of decaying to zero.
    -(-x.min(40.0)).exp_m1()
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

    pub(crate) fn absorb(&mut self, other: Self, now: Instant) {
        self.value = self.value_at(now) + other.value_at(now);
        self.last_update = now;
    }

    pub(crate) fn value_at(&self, now: Instant) -> f64 {
        let elapsed = now
            .saturating_duration_since(self.last_update)
            .as_secs_f64();

        if elapsed <= 0.0 {
            return self.value;
        }

        let ratio = elapsed / self.window.as_secs_f64();
        self.value * (1.0 - exponential_saturation(ratio))
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
    fn exponential_saturation_saturates_to_exact_one() {
        #[track_caller]
        fn case(x: f64) {
            assert_eq!(exponential_saturation(x), 1.0);
        }

        case(40.0);
        case(100.0);
        case(1e9);
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
    fn time_bias_saturates_to_exact_one() {
        assert_eq!(calculate_time_bias(secs(60 * 40), secs(60)), 1.0);
    }

    #[test]
    fn starts_at_zero() {
        let start = Instant::now();
        let avg = DecayingAverage::with_start_time(secs(300), start);
        assert_eq!(avg.value_at(start), 0.0);
    }

    #[test]
    fn increases_with_samples() {
        let start = Instant::now();
        let mut avg = DecayingAverage::with_start_time(secs(60), start);

        avg.record(60.0, start + secs(1));

        let value = avg.value_at(start + secs(1));
        assert!(value > 0.0);
        assert!(value < 60.0);
    }

    #[test]
    fn decays_over_time_without_samples() {
        let start = Instant::now();
        let mut avg = DecayingAverage::with_start_time(secs(60), start);

        avg.record(100.0, start + secs(1));
        let initial = avg.value_at(start + secs(1));

        let later = avg.value_at(start + secs(31));
        assert!(later < initial, "should decay: {} < {}", later, initial);
    }

    #[test]
    fn value_at_is_tick_frequency_independent() {
        let start = Instant::now();
        let mut avg = DecayingAverage::with_start_time(secs(60), start);

        avg.record(100.0, start + secs(1));

        let t = start + secs(31);
        let first_read = avg.value_at(t);
        let second_read = avg.value_at(t);
        let third_read = avg.value_at(t);

        assert_eq!(first_read, second_read);
        assert_eq!(second_read, third_read);

        let even_later = avg.value_at(start + secs(61));
        assert!(
            even_later < first_read,
            "should continue decaying: {} < {}",
            even_later,
            first_read
        );
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

        let value = avg.value_at(t);
        assert!((8.0..12.0).contains(&value), "Expected ~10, got {}", value);
    }

    #[test]
    fn ignores_zero_elapsed_time() {
        let start = Instant::now();
        let mut avg = DecayingAverage::with_start_time(secs(60), start);

        avg.record(100.0, start);
        assert_eq!(avg.value_at(start), 0.0);
    }

    #[test]
    fn negative_samples_work() {
        let start = Instant::now();
        let mut avg = DecayingAverage::with_start_time(secs(60), start);

        avg.record(-50.0, start + secs(1));
        assert!(
            avg.value_at(start + secs(1)) < 0.0,
            "expected negative value, got {}",
            avg.value_at(start + secs(1))
        );
    }

    #[test]
    fn large_elapsed_decays_to_exact_zero() {
        let start = Instant::now();
        let mut avg = DecayingAverage::with_start_time(secs(1), start);

        avg.record(1e15, start + secs(1));

        assert_eq!(avg.value_at(start + secs(1000)), 0.0);
    }

    #[test]
    fn decay_follows_exponential_curve() {
        let start = Instant::now();
        let mut avg = DecayingAverage::with_start_time(secs(60), start);

        avg.record(60.0, start + secs(1)); // Record to set a value
        let initial = avg.value_at(start + secs(1));

        let after_one_tc = avg.value_at(start + secs(61));
        let expected = initial * (-1.0_f64).exp();
        assert!(
            (after_one_tc - expected).abs() < 0.01,
            "after one time constant: {} ≈ {}",
            after_one_tc,
            expected
        );

        let after_two_tc = avg.value_at(start + secs(121));
        let expected = initial * (-2.0_f64).exp();
        assert!(
            (after_two_tc - expected).abs() < 0.01,
            "after two time constants: {} ≈ {}",
            after_two_tc,
            expected
        );
    }

    #[test]
    fn absorb_combines_values() {
        let start = Instant::now();
        let mut a = DecayingAverage::with_start_time(secs(60), start);
        let mut b = DecayingAverage::with_start_time(secs(60), start);

        a.record(100.0, start + secs(1));
        b.record(200.0, start + secs(2));

        let now = start + secs(3);
        let expected = a.value_at(now) + b.value_at(now);

        a.absorb(b, now);

        assert!(
            (a.value_at(now) - expected).abs() < 1e-10,
            "absorb: {} ≈ {}",
            a.value_at(now),
            expected
        );

        let later = now + secs(60);
        let decayed = a.value_at(later);
        assert!(decayed < a.value_at(now));
        assert!(decayed > 0.0);
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
