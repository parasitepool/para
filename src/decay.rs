use {super::*, parking_lot::Mutex};

const MIN_1: Duration = Duration::from_secs(60);
const MIN_5: Duration = Duration::from_secs(300);
const HOUR_1: Duration = Duration::from_secs(3600);
const DAY_1: Duration = Duration::from_secs(86400);
const WEEK_1: Duration = Duration::from_secs(604800);

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

pub(crate) struct HashRates {
    dsps_1m: DecayingAverage,
    dsps_5m: DecayingAverage,
    dsps_1h: DecayingAverage,
    dsps_1d: DecayingAverage,
    dsps_7d: DecayingAverage,
}

impl HashRates {
    pub(crate) fn new() -> Self {
        Self {
            dsps_1m: DecayingAverage::new(MIN_1),
            dsps_5m: DecayingAverage::new(MIN_5),
            dsps_1h: DecayingAverage::new(HOUR_1),
            dsps_1d: DecayingAverage::new(DAY_1),
            dsps_7d: DecayingAverage::new(WEEK_1),
        }
    }

    pub(crate) fn record(&mut self, difficulty: f64, now: Instant) {
        self.dsps_1m.record(difficulty, now);
        self.dsps_5m.record(difficulty, now);
        self.dsps_1h.record(difficulty, now);
        self.dsps_1d.record(difficulty, now);
        self.dsps_7d.record(difficulty, now);
    }

    pub(crate) fn dsps_1m(&self, now: Instant) -> f64 {
        self.dsps_1m.value_at(now)
    }

    pub(crate) fn dsps_5m(&self, now: Instant) -> f64 {
        self.dsps_5m.value_at(now)
    }

    pub(crate) fn dsps_1h(&self, now: Instant) -> f64 {
        self.dsps_1h.value_at(now)
    }

    pub(crate) fn dsps_1d(&self, now: Instant) -> f64 {
        self.dsps_1d.value_at(now)
    }

    pub(crate) fn dsps_7d(&self, now: Instant) -> f64 {
        self.dsps_7d.value_at(now)
    }

    pub(crate) fn hash_rate_1m(&self) -> HashRate {
        HashRate::from_dsps(self.dsps_1m(Instant::now()))
    }

    pub(crate) fn hash_rate_5m(&self) -> HashRate {
        HashRate::from_dsps(self.dsps_5m(Instant::now()))
    }

    pub(crate) fn hash_rate_1h(&self) -> HashRate {
        HashRate::from_dsps(self.dsps_1h(Instant::now()))
    }

    pub(crate) fn hash_rate_1d(&self) -> HashRate {
        HashRate::from_dsps(self.dsps_1d(Instant::now()))
    }

    pub(crate) fn hash_rate_7d(&self) -> HashRate {
        HashRate::from_dsps(self.dsps_7d(Instant::now()))
    }
}

impl Default for HashRates {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) struct SharedHashRates(Mutex<HashRates>);

impl SharedHashRates {
    pub(crate) fn new() -> Self {
        Self(Mutex::new(HashRates::new()))
    }

    pub(crate) fn record(&self, difficulty: f64) {
        self.0.lock().record(difficulty, Instant::now());
    }

    pub(crate) fn hash_rate_1m(&self) -> HashRate {
        self.0.lock().hash_rate_1m()
    }

    pub(crate) fn hash_rate_5m(&self) -> HashRate {
        self.0.lock().hash_rate_5m()
    }

    pub(crate) fn hash_rate_1h(&self) -> HashRate {
        self.0.lock().hash_rate_1h()
    }

    pub(crate) fn hash_rate_1d(&self) -> HashRate {
        self.0.lock().hash_rate_1d()
    }

    pub(crate) fn hash_rate_7d(&self) -> HashRate {
        self.0.lock().hash_rate_7d()
    }
}

impl Default for SharedHashRates {
    fn default() -> Self {
        Self::new()
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
    fn large_elapsed_saturates_and_stays_finite() {
        let start = Instant::now();
        let mut avg = DecayingAverage::with_start_time(secs(1), start);

        avg.record(100.0, start + secs(1));

        let value = avg.value_at(start + secs(1000));
        assert!(value.is_finite(), "value should be finite");
        assert!(value < 1e-10, "value should be effectively zero: {}", value);
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
