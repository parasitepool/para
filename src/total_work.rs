use super::*;

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Default, Serialize, Deserialize)]
#[serde(try_from = "f64", into = "f64")]
pub struct TotalWork(f64);

impl TotalWork {
    pub const ZERO: Self = Self(0.0);

    pub fn new(value: f64) -> Result<Self> {
        ensure!(
            value.is_finite() && value >= 0.0,
            "total work must be finite and >= 0, got {value}",
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
        HashDays::new(self.as_f64() * HASHES_PER_DIFF_1 as f64 / 86_400.0)
            .expect("total work conversion overflowed")
    }
}

impl TryFrom<f64> for TotalWork {
    type Error = Error;

    fn try_from(value: f64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<TotalWork> for f64 {
    fn from(value: TotalWork) -> Self {
        value.0
    }
}

impl Display for TotalWork {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        format_si(self.0, "", f)
    }
}

impl Add for TotalWork {
    type Output = Self;

    fn add(self, rhs: Self) -> Self {
        Self::new(self.0 + rhs.0).expect("total work add overflowed")
    }
}

impl AddAssign for TotalWork {
    fn add_assign(&mut self, rhs: Self) {
        *self = *self + rhs;
    }
}

impl Sub for TotalWork {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self {
        Self((self.0 - rhs.0).max(0.0))
    }
}

impl SubAssign for TotalWork {
    fn sub_assign(&mut self, rhs: Self) {
        *self = *self - rhs;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display() {
        assert_eq!(TotalWork::new(0.0).unwrap().to_string(), "0");
        assert_eq!(
            TotalWork::new(3_161_600_000.0).unwrap().to_string(),
            "3.16G"
        );
        assert_eq!(TotalWork::new(1e6).unwrap().to_string(), "1M");
    }

    #[test]
    fn arithmetic() {
        let a = TotalWork::new(100.0).unwrap();
        let b = TotalWork::new(200.0).unwrap();
        assert_eq!((a + b).as_f64(), 300.0);
        assert_eq!((b - a).as_f64(), 100.0);
        assert_eq!(a - b, TotalWork::ZERO);

        let mut c = TotalWork::ZERO;
        c += a;
        c += b;
        assert_eq!(c.as_f64(), 300.0);
        c -= a;
        assert_eq!(c.as_f64(), 200.0);
    }

    #[test]
    fn new_rejects_invalid_values() {
        assert!(TotalWork::new(-1.0).is_err());
        assert!(TotalWork::new(f64::NAN).is_err());
        assert!(TotalWork::new(f64::INFINITY).is_err());
    }

    #[test]
    fn serde_roundtrip() {
        let work = TotalWork::new(1234.5).unwrap();
        let json = serde_json::to_string(&work).unwrap();
        assert_eq!(json, "1234.5");
        let parsed: TotalWork = serde_json::from_str(&json).unwrap();
        assert_eq!(work, parsed);
    }

    #[test]
    fn serde_rejects_invalid_values() {
        assert!(serde_json::from_str::<TotalWork>("-1.0").is_err());
    }
}
