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
        Self {
            value: 0.0,
            window,
            last_update: Instant::now(),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_start_time(window: Duration, start: Instant) -> Self {
        Self {
            value: 0.0,
            window,
            last_update: start,
        }
    }

    pub(crate) fn record(&mut self, sample: f64, now: Instant) {
        let elapsed = now.duration_since(self.last_update).as_secs_f64();
        if elapsed <= 0.0 {
            return;
        }

        let window_secs = self.window.as_secs_f64();
        let decay_factor = exponential_saturation(elapsed / window_secs);
        let normalizer = 1.0 + decay_factor;

        self.value = (self.value + (sample / elapsed) * decay_factor) / normalizer;
        self.last_update = now;
    }

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
}
