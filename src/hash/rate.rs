use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(try_from = "f64", into = "f64")]
pub struct HashRate(f64);

impl Eq for HashRate {}

impl Ord for HashRate {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.partial_cmp(&other.0).unwrap()
    }
}

impl PartialOrd for HashRate {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl HashRate {
    pub const ZERO: Self = Self(0.0);

    pub fn new(value: f64) -> Result<Self> {
        ensure!(
            value.is_finite() && value >= 0.0,
            "hash rate must be finite and >= 0, got {value}",
        );

        Ok(Self(value))
    }

    pub fn from_hps(hps: f64) -> Self {
        Self(saturating_finite(hps))
    }

    pub fn as_hps(self) -> f64 {
        self.0
    }

    pub fn from_dsps(dsps: f64) -> Self {
        Self::from_hps(dsps * HASHES_PER_DIFF_1 as f64)
    }

    pub fn as_dsps(self) -> f64 {
        self.0 / HASHES_PER_DIFF_1 as f64
    }

    #[cfg(test)]
    pub(crate) fn estimate(total_difficulty: f64, window: Duration) -> Self {
        if window.is_zero() {
            return Self::ZERO;
        }

        Self::from_hps(total_difficulty * HASHES_PER_DIFF_1 as f64 / window.as_secs_f64())
    }
}

impl TryFrom<f64> for HashRate {
    type Error = Error;

    fn try_from(value: f64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<HashRate> for f64 {
    fn from(value: HashRate) -> Self {
        value.0
    }
}

impl Display for HashRate {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        format_si(self.0, "H/s", f)
    }
}

impl FromStr for HashRate {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(parse_si(s, &["H/s", "H"])?)
    }
}

impl Add for HashRate {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self::from_hps(self.0 + rhs.0)
    }
}

impl AddAssign for HashRate {
    fn add_assign(&mut self, rhs: Self) {
        *self = *self + rhs;
    }
}

impl Sub for HashRate {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        Self::from_hps(self.0 - rhs.0)
    }
}

impl SubAssign for HashRate {
    fn sub_assign(&mut self, rhs: Self) {
        *self = *self - rhs;
    }
}

impl Mul<f64> for HashRate {
    type Output = Self;
    fn mul(self, rhs: f64) -> Self {
        Self::from_hps(self.0 * rhs)
    }
}

impl Div<f64> for HashRate {
    type Output = Self;
    fn div(self, rhs: f64) -> Self {
        if rhs == 0.0 {
            Self::ZERO
        } else {
            Self::from_hps(self.0 / rhs)
        }
    }
}

impl Sum for HashRate {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::ZERO, Add::add)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashrate_from_dsps() {
        let rate = HashRate::from_dsps(1.0);
        assert_eq!(rate.as_hps(), HASHES_PER_DIFF_1 as f64);
        assert_eq!(rate.as_dsps(), 1.0);

        let rate = HashRate::from_dsps(20.0);
        assert_eq!(rate.as_hps(), 20.0 * HASHES_PER_DIFF_1 as f64);
        assert_eq!(rate.as_dsps(), 20.0);
    }

    #[test]
    fn hashrate_from_hps() {
        let rate = HashRate::from_hps(1e12);
        assert_eq!(rate.as_hps(), 1e12);
    }

    #[test]
    fn computed_constructors_saturate_invalid_values() {
        assert_eq!(HashRate::from_hps(f64::INFINITY).as_hps(), f64::MAX);
        assert_eq!(HashRate::from_dsps(f64::MAX).as_hps(), f64::MAX);
        assert_eq!(HashRate::from_hps(f64::NAN), HashRate::ZERO);
        assert_eq!(HashRate::from_hps(-1.0), HashRate::ZERO);
    }

    #[test]
    fn hashrate_estimate() {
        let rate = HashRate::estimate(60.0, Duration::from_secs(60));
        assert_eq!(rate.as_hps(), HASHES_PER_DIFF_1 as f64);

        let rate = HashRate::estimate(100.0, Duration::ZERO);
        assert_eq!(rate, HashRate::ZERO);
    }

    #[test]
    fn hashrate_display_formatting() {
        let cases = [
            (0.0, "0 H/s"),
            (1e3, "1 KH/s"),
            (1e6, "1 MH/s"),
            (1e9, "1 GH/s"),
            (1e12, "1 TH/s"),
            (1e15, "1 PH/s"),
            (1e18, "1 EH/s"),
            (314e15, "314 PH/s"),
            (1.5e12, "1.5 TH/s"),
            (1.567e12, "1.56 TH/s"),
            (45.6e12, "45.6 TH/s"),
            (456e12, "456 TH/s"),
            (123.456e12, "123.45 TH/s"),
            (9999.0, "9.99 KH/s"),
        ];

        for (value, expected) in cases {
            let rate = HashRate::from_hps(value);
            assert_eq!(rate.to_string(), expected, "for value {value}");
        }
    }

    #[test]
    fn hashrate_parse() {
        let cases = [
            ("0", 0.0),
            ("0 H/s", 0.0),
            ("1K", 1e3),
            ("1 KH/s", 1e3),
            ("1.5M", 1.5e6),
            ("1.5 MH/s", 1.5e6),
            ("100G", 1e11),
            ("100 GH/s", 1e11),
            ("1T", 1e12),
            ("1 TH/s", 1e12),
            ("314P", 314e15),
            ("314 PH/s", 314e15),
            ("1E", 1e18),
            ("1 EH/s", 1e18),
        ];

        for (input, expected) in cases {
            let rate: HashRate = input.parse().unwrap();
            let actual = rate.as_hps();
            let rel_err = if expected == 0.0 {
                actual
            } else {
                ((actual - expected) / expected).abs()
            };
            assert!(
                rel_err < 1e-10,
                "parse({input}): got {actual}, want {expected}"
            );
        }
    }

    #[test]
    fn hashrate_parse_errors() {
        let invalid = ["", "abc", "-1", "NaN", "Infinity", "1.8e308 EH/s"];
        for input in invalid {
            assert!(input.parse::<HashRate>().is_err(), "should reject: {input}");
        }
    }

    #[test]
    fn hashrate_new_rejects_invalid_values() {
        assert!(HashRate::new(-1.0).is_err());
        assert!(HashRate::new(f64::NAN).is_err());
        assert!(HashRate::new(f64::INFINITY).is_err());
    }

    #[test]
    fn hashrate_arithmetic() {
        let a = HashRate::from_hps(1e12);
        let b = HashRate::from_hps(2e12);

        assert_eq!((a + b).as_hps(), 3e12);
        assert_eq!((b - a).as_hps(), 1e12);
        assert_eq!((a * 2.0).as_hps(), 2e12);
        assert_eq!((b / 2.0).as_hps(), 1e12);
    }

    #[test]
    fn hashrate_arithmetic_saturates() {
        let max = HashRate::from_hps(f64::MAX);

        assert_eq!((max + max).as_hps(), f64::MAX);
        assert_eq!((max * 2.0).as_hps(), f64::MAX);
        assert_eq!((HashRate::from_hps(1.0) * f64::NAN), HashRate::ZERO);
    }

    #[test]
    fn hashrate_subtraction_clamps() {
        let a = HashRate::from_hps(1e12);
        let b = HashRate::from_hps(2e12);
        assert_eq!((a - b).as_hps(), 0.0);
    }

    #[test]
    fn hashrate_serde_roundtrip() {
        let rate = HashRate::from_hps(1.5e12);
        let json = serde_json::to_string(&rate).unwrap();
        let parsed: HashRate = serde_json::from_str(&json).unwrap();
        assert_eq!(rate, parsed);
    }

    #[test]
    fn hashrate_serde_rejects_invalid_values() {
        assert!(serde_json::from_str::<HashRate>("-1.0").is_err());
    }
}
