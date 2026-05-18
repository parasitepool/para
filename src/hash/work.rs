use super::*;

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Default, Serialize, Deserialize)]
#[serde(try_from = "f64", into = "f64")]
pub struct HashWork(f64);

impl HashWork {
    pub const ZERO: Self = Self(0.0);

    pub fn new(value: f64) -> Result<Self> {
        ensure!(
            value.is_finite() && value >= 0.0,
            "hash work must be finite and >= 0, got {value}",
        );

        Ok(Self(value))
    }

    pub(crate) const fn from_raw(value: f64) -> Self {
        Self(value)
    }

    pub fn from_difficulty(difficulty: Difficulty) -> Self {
        Self::from_raw(difficulty.as_f64())
    }

    pub fn as_f64(self) -> f64 {
        self.0
    }

    pub fn to_hash_days(self) -> HashDays {
        HashDays::from_raw(saturating_finite(
            self.as_f64() * (HASHES_PER_DIFF_1 as f64 / SECONDS_PER_DAY),
        ))
    }
}

impl TryFrom<f64> for HashWork {
    type Error = Error;

    fn try_from(value: f64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<HashWork> for f64 {
    fn from(value: HashWork) -> Self {
        value.0
    }
}

impl Display for HashWork {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        format_si(self.0, "", f)
    }
}

impl Add for HashWork {
    type Output = Self;

    fn add(self, rhs: Self) -> Self {
        Self::from_raw(saturating_finite(self.0 + rhs.0))
    }
}

impl AddAssign for HashWork {
    fn add_assign(&mut self, rhs: Self) {
        *self = *self + rhs;
    }
}

impl Sub for HashWork {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self {
        Self((self.0 - rhs.0).max(0.0))
    }
}

impl SubAssign for HashWork {
    fn sub_assign(&mut self, rhs: Self) {
        *self = *self - rhs;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display() {
        assert_eq!(HashWork::new(0.0).unwrap().to_string(), "0");
        assert_eq!(HashWork::new(3_161_600_000.0).unwrap().to_string(), "3.16G");
        assert_eq!(HashWork::new(1e6).unwrap().to_string(), "1M");
    }

    #[test]
    fn arithmetic() {
        let a = HashWork::new(100.0).unwrap();
        let b = HashWork::new(200.0).unwrap();
        assert_eq!((a + b).as_f64(), 300.0);
        assert_eq!((b - a).as_f64(), 100.0);
        assert_eq!(a - b, HashWork::ZERO);

        let mut c = HashWork::ZERO;
        c += a;
        c += b;
        assert_eq!(c.as_f64(), 300.0);
        c -= a;
        assert_eq!(c.as_f64(), 200.0);
    }

    #[test]
    fn addition_saturates_extreme_finite_values() {
        assert_eq!(
            (HashWork::new(f64::MAX).unwrap() + HashWork::new(f64::MAX).unwrap()).as_f64(),
            f64::MAX
        );
    }

    #[test]
    fn addition_with_nan_maps_to_zero() {
        assert_eq!(
            (HashWork::from_raw(f64::NAN) + HashWork::ZERO),
            HashWork::ZERO,
        );
    }

    #[test]
    fn new_rejects_invalid_values() {
        assert!(HashWork::new(-1.0).is_err());
        assert!(HashWork::new(f64::NAN).is_err());
        assert!(HashWork::new(f64::INFINITY).is_err());
    }

    #[test]
    fn serde_roundtrip() {
        let work = HashWork::new(1234.5).unwrap();
        let json = serde_json::to_string(&work).unwrap();
        assert_eq!(json, "1234.5");
        let parsed: HashWork = serde_json::from_str(&json).unwrap();
        assert_eq!(work, parsed);
    }

    #[test]
    fn serde_rejects_invalid_values() {
        assert!(serde_json::from_str::<HashWork>("-1.0").is_err());
    }

    #[test]
    fn hash_work_to_hash_days_round_trips() {
        let work = HashWork::new(42.0).unwrap();
        let roundtrip = work.to_hash_days().to_hash_work();

        assert!((roundtrip.as_f64() - work.as_f64()).abs() < 1e-9);
    }

    #[test]
    fn to_hash_days_saturates_extreme_finite_value() {
        assert_eq!(
            HashWork::new(f64::MAX).unwrap().to_hash_days().as_f64(),
            f64::MAX,
        );
    }

    #[test]
    fn one_phd_maps_to_expected_hash_work() {
        let work = HashDays::new(PETA).unwrap().to_hash_work();
        let expected = PETA * SECONDS_PER_DAY / HASHES_PER_DIFF_1 as f64;
        let rel_err = ((work.as_f64() - expected) / expected).abs();

        assert!(rel_err < 1e-12, "got {}, want {expected}", work.as_f64());
    }
}
