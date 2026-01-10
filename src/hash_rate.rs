use super::*;

/// Expected hashes per difficulty-1 share: 2^32 =~ 4.29 billion.
/// The precise value is 2^256/target_1 =~ 4,295,032,833 (~0.0015% higher),
/// but 2^32 is the standard approximation used across the mining ecosystem.
const HASHES_PER_DIFF_1: u64 = 1 << 32;

const SI_PREFIXES: &[(&str, f64)] = &[
    ("", 1.0),
    ("K", 1e3),
    ("M", 1e6),
    ("G", 1e9),
    ("T", 1e12),
    ("P", 1e15),
    ("E", 1e18),
    ("Z", 1e21),
    ("Y", 1e24),
];

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Default)]
pub struct HashRate(pub f64);

impl HashRate {
    pub const ZERO: Self = Self(0.0);

    pub fn from_dsps(dsps: f64) -> Self {
        Self(dsps * HASHES_PER_DIFF_1 as f64)
    }

    #[cfg(test)]
    pub(crate) fn estimate(total_difficulty: f64, window: Duration) -> Self {
        if window.is_zero() {
            return Self::ZERO;
        }

        Self(total_difficulty * HASHES_PER_DIFF_1 as f64 / window.as_secs_f64())
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
        Ok(Self(parse_si(s, &["H/s", "H"])?))
    }
}

impl Serialize for HashRate {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for HashRate {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

impl Add for HashRate {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self(self.0 + rhs.0)
    }
}

impl AddAssign for HashRate {
    fn add_assign(&mut self, rhs: Self) {
        self.0 += rhs.0;
    }
}

impl Sub for HashRate {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        Self((self.0 - rhs.0).max(0.0))
    }
}

impl SubAssign for HashRate {
    fn sub_assign(&mut self, rhs: Self) {
        self.0 = (self.0 - rhs.0).max(0.0);
    }
}

impl Mul<f64> for HashRate {
    type Output = Self;
    fn mul(self, rhs: f64) -> Self {
        Self((self.0 * rhs).max(0.0))
    }
}

impl Div<f64> for HashRate {
    type Output = Self;
    fn div(self, rhs: f64) -> Self {
        if rhs == 0.0 {
            Self::ZERO
        } else {
            Self((self.0 / rhs).max(0.0))
        }
    }
}

fn format_si(value: f64, unit: &str, f: &mut Formatter<'_>) -> fmt::Result {
    if value == 0.0 {
        return write!(f, "0 {unit}");
    }

    let (prefix, divisor) = SI_PREFIXES
        .iter()
        .rev()
        .find(|(_, div)| value.abs() >= *div)
        .unwrap_or(&SI_PREFIXES[0]);

    let scaled = value / divisor;
    let s = format!("{scaled:.3}");
    let trimmed = s.trim_end_matches('0').trim_end_matches('.');

    write!(f, "{trimmed} {prefix}{unit}")
}

fn parse_si(s: &str, units: &[&str]) -> Result<f64> {
    let s = s.trim();
    ensure!(!s.is_empty(), "empty string");

    let s = units
        .iter()
        .find_map(|unit| s.strip_suffix(unit))
        .unwrap_or(s)
        .trim();

    let (num_str, mult) = SI_PREFIXES
        .iter()
        .rev()
        .find_map(|(prefix, mult)| {
            if prefix.is_empty() {
                return None;
            }
            s.strip_suffix(prefix)
                .or_else(|| s.strip_suffix(&prefix.to_lowercase()))
                .map(|n| (n.trim(), *mult))
        })
        .unwrap_or((s, 1.0));

    let num: f64 = num_str.parse().context("invalid number")?;
    ensure!(num.is_finite() && num >= 0.0, "invalid value");

    let result = num * mult;
    ensure!(result.is_finite(), "value overflow after SI prefix scaling");

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashrate_from_dsps() {
        let rate = HashRate::from_dsps(1.0);
        assert_eq!(rate.0, HASHES_PER_DIFF_1 as f64);

        let rate = HashRate::from_dsps(20.0);
        assert_eq!(rate.0, 20.0 * HASHES_PER_DIFF_1 as f64);
    }

    #[test]
    fn hashrate_estimate() {
        let rate = HashRate::estimate(60.0, Duration::from_secs(60));
        assert_eq!(rate.0, HASHES_PER_DIFF_1 as f64);

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
            (1.567e12, "1.567 TH/s"),
            (45.6e12, "45.6 TH/s"),
            (456e12, "456 TH/s"),
            (123.456e12, "123.456 TH/s"),
        ];

        for (value, expected) in cases {
            let rate = HashRate(value);
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
            let actual = rate.0;
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
    fn hashrate_arithmetic() {
        let a = HashRate(1e12);
        let b = HashRate(2e12);

        assert_eq!((a + b).0, 3e12);
        assert_eq!((b - a).0, 1e12);
        assert_eq!((a * 2.0).0, 2e12);
        assert_eq!((b / 2.0).0, 1e12);
    }

    #[test]
    fn hashrate_subtraction_clamps() {
        let a = HashRate(1e12);
        let b = HashRate(2e12);
        assert_eq!((a - b).0, 0.0);
    }

    #[test]
    fn hashrate_serde_roundtrip() {
        let rate = HashRate(1.5e12);
        let json = serde_json::to_string(&rate).unwrap();
        let parsed: HashRate = serde_json::from_str(&json).unwrap();
        assert_eq!(rate, parsed);
    }
}
