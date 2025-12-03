use super::*;

/// Expected hashes per difficulty-1 share: 2^32 = 4.29 billion.
/// The precise value is 2^256/target_1 ≈ 4,295,032,833 (~0.0015% higher),
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

    pub fn from_difficulty_rate(difficulty: f64, shares_per_sec: f64) -> Self {
        Self(difficulty * shares_per_sec * HASHES_PER_DIFF_1 as f64)
    }

    pub fn estimate(total_difficulty: f64, window: Duration) -> Self {
        if window.is_zero() {
            return Self::ZERO;
        }

        Self(total_difficulty * HASHES_PER_DIFF_1 as f64 / window.as_secs_f64())
    }
}

impl Display for HashRate {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        format_with_si_suffix(self.0, "H/s", f)
    }
}

impl FromStr for HashRate {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(parse_with_si_prefix(
            s,
            &["H/s", "h/s", "H", "h", ""],
        )?))
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

#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub message: String,
}

impl Display for ParseError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ParseError {}

fn format_with_si_suffix(value: f64, unit: &str, f: &mut Formatter<'_>) -> fmt::Result {
    if value == 0.0 {
        return write!(f, "0 {unit}");
    }

    let (prefix, divisor) = SI_PREFIXES
        .iter()
        .rev()
        .find(|(_, div)| value.abs() >= *div * 0.9999)
        .unwrap_or(&SI_PREFIXES[0]);

    let scaled = value / divisor;

    // Use 3 significant figures
    if scaled >= 100.0 {
        write!(f, "{:.0} {prefix}{unit}", scaled)
    } else if scaled >= 10.0 {
        write!(f, "{:.1} {prefix}{unit}", scaled)
    } else {
        write!(f, "{:.2} {prefix}{unit}", scaled)
    }
}

fn parse_with_si_prefix(s: &str, valid_suffixes: &[&str]) -> Result<f64, ParseError> {
    let s = s.trim();
    if s.is_empty() {
        return Err(ParseError {
            message: "empty string".to_string(),
        });
    }

    // Find and strip any valid suffix
    let mut num_part = s;
    for suffix in valid_suffixes {
        if let Some(stripped) = s.strip_suffix(suffix) {
            num_part = stripped.trim();
            break;
        }
    }

    // Check for SI prefix at the end
    let (num_str, multiplier) = if let Some(last_char) = num_part.chars().last() {
        let upper = last_char.to_ascii_uppercase();
        if let Some((_, mult)) = SI_PREFIXES
            .iter()
            .find(|(p, _)| !p.is_empty() && p.chars().next().unwrap().to_ascii_uppercase() == upper)
        {
            (&num_part[..num_part.len() - last_char.len_utf8()], *mult)
        } else if last_char.is_ascii_digit() || last_char == '.' {
            (num_part, 1.0)
        } else {
            return Err(ParseError {
                message: format!("invalid suffix: {last_char}"),
            });
        }
    } else {
        (num_part, 1.0)
    };

    let num_str = num_str.trim();
    let num: f64 = num_str.parse().map_err(|e| ParseError {
        message: format!("invalid number: {e}"),
    })?;

    if !num.is_finite() || num < 0.0 {
        return Err(ParseError {
            message: "value must be finite and non-negative".to_string(),
        });
    }

    Ok(num * multiplier)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashrate_from_difficulty_rate() {
        let rate = HashRate::from_difficulty_rate(1.0, 1.0);
        assert_eq!(rate.0, HASHES_PER_DIFF_1 as f64);

        let rate = HashRate::from_difficulty_rate(100.0, 0.2);
        assert_eq!(rate.0, 20.0 * HASHES_PER_DIFF_1 as f64);
    }

    #[test]
    fn hashrate_estimate() {
        let rate = HashRate::estimate(60.0, Duration::from_secs(60));
        assert_eq!(rate.0, HASHES_PER_DIFF_1 as f64);
    }

    #[test]
    fn hashrate_display_formatting() {
        let cases = [
            (0.0, "0 H/s"),
            (1e3, "1.00 KH/s"),
            (1e6, "1.00 MH/s"),
            (1e9, "1.00 GH/s"),
            (1e12, "1.00 TH/s"),
            (1e15, "1.00 PH/s"),
            (1e18, "1.00 EH/s"),
            (314e15, "314 PH/s"),
            (1.5e12, "1.50 TH/s"),
            (45.6e12, "45.6 TH/s"),
            (456e12, "456 TH/s"),
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
        let invalid = ["", "abc", "-1", "NaN", "Infinity"];
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
