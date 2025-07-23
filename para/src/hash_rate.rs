use super::*;

const UNITS: &[&str] = &["", "K", "M", "G", "T", "P", "E", "Z", "Y"];

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, DeserializeFromStr, SerializeDisplay)]
pub struct HashRate(pub f64);

impl fmt::Display for HashRate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut rate = self.0;
        let mut unit = 0;
        while rate >= 1000.0 && unit < UNITS.len() - 1 {
            rate /= 1000.0;
            unit += 1;
        }

        if rate == 0.0 {
            return write!(f, "0{}", UNITS[unit]);
        }

        let precision = if rate >= 10.0 {
            0
        } else if rate >= 1.0 {
            1
        } else {
            2
        };

        write!(f, "{rate:.precision$}{}", UNITS[unit])
    }
}

impl FromStr for HashRate {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.is_empty() {
            return Err("Empty string".to_string());
        }

        let bytes = s.as_bytes();
        let last = bytes[bytes.len() - 1] as char;

        let (num_str, mult) = if last.is_ascii_digit() || last == '.' {
            (s, 1.0)
        } else {
            let suffix = last.to_ascii_uppercase();
            let m = match suffix {
                'H' => 1.0,
                'K' => 1e3,
                'M' => 1e6,
                'G' => 1e9,
                'T' => 1e12,
                'P' => 1e15,
                'E' => 1e18,
                'Z' => 1e21,
                'Y' => 1e24,
                _ => return Err(format!("Invalid suffix: {suffix}")),
            };
            (&s[0..s.len() - 1], m)
        };

        let num: f64 = num_str.parse().map_err(|e| format!("Parse error: {e}"))?;

        Ok(Self(num * mult))
    }
}

impl Add for HashRate {
    type Output = Self;

    fn add(self, rhs: Self) -> Self {
        Self(self.0 + rhs.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hashrate_formatting() {
        let cases = [
            (0.0, "0"),
            (999.0, "999"),
            (1_000.0, "1.0K"),
            (1_500_000.0, "1.5M"),
            (2_000_000_000.0, "2.0G"),
            (3.5e12, "3.5T"),
            (4.2e15, "4.2P"),
            (5.1e18, "5.1E"),
            (6.6e21, "6.6Z"),
            (7.7e24, "7.7Y"),
            (1.2e27, "1200Y"),
        ];

        for (input, expected) in cases {
            let hr = HashRate(input);
            assert_eq!(hr.to_string(), expected, "Failed on input {input}");
        }
    }

    #[test]
    fn test_hashrate_parsing() {
        let cases = [
            ("0", 0.0),
            ("999", 999.0),
            ("1K", 1_000.0),
            ("1.5M", 1_500_000.0),
            ("2G", 2_000_000_000.0),
            ("3.5T", 3.5e12),
            ("4.2P", 4.2e15),
            ("5.1E", 5.1e18),
            ("6.6Z", 6.6e21),
            ("7.7Y", 7.7e24),
            ("314P", 314.0 * 1e15),
            ("1.23k", 1.23e3),
            ("100H", 100.0),
        ];

        for (input, expected) in cases {
            let hr = HashRate::from_str(input).unwrap();
            assert_eq!(hr.0, expected, "Failed parsing {input}");
        }
    }

    #[test]
    fn test_hashrate_parsing_errors() {
        let invalid = ["", "abc", "1Q", "1.2.3P"];
        for input in invalid {
            assert!(HashRate::from_str(input).is_err(), "Should fail on {input}");
        }
    }

    #[test]
    fn test_hashrate_addition() {
        let cases = [
            (0.0, 0.0, 0.0),
            (999.0, 1.0, 1000.0),
            (1e3, 1e6, 1_001_000.0),
            (3.5e12, 4.2e15, 4.2035e15),
            (5.1e18, 6.6e21, 6.6051e21),
        ];

        for (a, b, expected) in cases {
            let hr_a = HashRate(a);
            let hr_b = HashRate(b);
            let sum = hr_a + hr_b;
            assert_eq!(sum.0, expected, "Failed adding {a} + {b}");
        }

        let hr1 = HashRate::from_str("314P").unwrap();
        let hr2 = HashRate::from_str("1.23E").unwrap();
        let sum = hr1 + hr2;
        assert_eq!(sum.0, 314e15 + 1.23e18);
    }
}
