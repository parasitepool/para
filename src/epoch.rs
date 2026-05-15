use super::*;

fn duration_from_finite_secs(secs: f64) -> Duration {
    let secs = secs.max(0.0);

    if secs >= Duration::MAX.as_secs() as f64 {
        return Duration::MAX;
    }

    Duration::from_secs_f64(secs)
}

pub(crate) fn duration_from_secs_ago(secs: f64, field: &str) -> Result<Duration> {
    ensure!(secs.is_finite(), "{field} must be finite");
    Ok(duration_from_finite_secs(secs))
}

pub(crate) fn instant_to_epoch_secs(time: Instant, now: Instant) -> f64 {
    let epoch_now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before UNIX epoch")
        .as_secs_f64();
    let elapsed = now.saturating_duration_since(time).as_secs_f64();

    epoch_now - elapsed
}

pub(crate) fn epoch_secs_to_instant(secs: f64) -> Instant {
    let now = Instant::now();
    let epoch_now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before UNIX epoch")
        .as_secs_f64();

    if !secs.is_finite() || secs >= epoch_now {
        return now;
    }

    now.checked_sub(duration_from_finite_secs(epoch_now - secs))
        .unwrap_or(now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_secs_to_instant_returns_now_for_non_finite() {
        let before = Instant::now();
        let restored = epoch_secs_to_instant(f64::NAN);
        let after = Instant::now();

        assert!(restored >= before);
        assert!(restored <= after);
    }

    #[test]
    fn epoch_secs_to_instant_returns_now_for_future() {
        let before = Instant::now();
        let restored = epoch_secs_to_instant(f64::MAX);
        let after = Instant::now();

        assert!(restored >= before);
        assert!(restored <= after);
    }

    #[test]
    fn epoch_secs_to_instant_handles_far_past_without_panicking() {
        let finite = epoch_secs_to_instant(-1e30);
        let negative_infinity = epoch_secs_to_instant(f64::NEG_INFINITY);

        assert!(finite <= Instant::now());
        assert!(negative_infinity <= Instant::now());
    }

    #[test]
    fn duration_from_secs_ago_clamps_overflow() {
        let duration = duration_from_secs_ago(1e30, "test").unwrap();

        assert_eq!(duration, Duration::MAX);
    }
}
