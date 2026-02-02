use super::*;

pub const SI_PREFIXES: &[(&str, f64)] = &[
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

pub fn format_si(value: f64, unit: &str, f: &mut Formatter<'_>) -> fmt::Result {
    if value == 0.0 {
        return if unit.is_empty() {
            write!(f, "0")
        } else {
            write!(f, "0 {unit}")
        };
    }

    let (prefix, divisor) = SI_PREFIXES
        .iter()
        .rev()
        .find(|(_, div)| value.abs() >= *div)
        .unwrap_or(&SI_PREFIXES[0]);

    let scaled = value / divisor;
    let precision = f.precision().unwrap_or(2);
    let s = format!("{scaled:.precision$}");
    let trimmed = s.trim_end_matches('0').trim_end_matches('.');

    let suffix = format!("{prefix}{unit}");

    if suffix.is_empty() {
        write!(f, "{trimmed}")
    } else {
        write!(f, "{trimmed} {suffix}")
    }
}

pub fn parse_si(s: &str, units: &[&str]) -> Result<f64> {
    let s = s.trim();

    if s.is_empty() {
        return Err(InternalError::InvalidValue {
            reason: "empty string".to_string(),
        });
    }

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

    let num: f64 = num_str.parse().map_err(|_| InternalError::Parse {
        message: "invalid number".to_string(),
    })?;

    if !num.is_finite() || num < 0.0 {
        return Err(InternalError::InvalidValue {
            reason: "invalid value".to_string(),
        });
    }

    let result = num * mult;

    if !result.is_finite() {
        return Err(InternalError::InvalidValue {
            reason: "value overflow after SI prefix scaling".to_string(),
        });
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FormatSi(f64, &'static str);

    impl Display for FormatSi {
        fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
            format_si(self.0, self.1, f)
        }
    }

    #[test]
    fn format() {
        #[track_caller]
        fn case(value: f64, unit: &'static str, expected: &str) {
            assert_eq!(FormatSi(value, unit).to_string(), expected);
        }

        case(0.0, "", "0");
        case(0.0, "H/s", "0 H/s");
        case(1.0, "", "1");
        case(42.0, "", "42");
        case(999.0, "", "999");
        case(1e3, "", "1 K");
        case(1.5e3, "", "1.5 K");
        case(1e6, "", "1 M");
        case(1e9, "", "1 G");
        case(1e12, "", "1 T");
        case(1e15, "", "1 P");
        case(1e18, "", "1 E");
        case(1.567e12, "H/s", "1.57 TH/s");
        case(123.456e12, "", "123.46 T");
    }

    #[test]
    fn parse() {
        #[track_caller]
        fn case(input: &str, units: &[&str], expected: f64) {
            let got = parse_si(input, units).unwrap();
            let rel_err = if expected == 0.0 {
                got
            } else {
                ((got - expected) / expected).abs()
            };
            assert!(
                rel_err < 1e-10,
                "parse({input}): got {got}, want {expected}"
            );
        }

        case("0", &[], 0.0);
        case("1", &[], 1.0);
        case("42", &[], 42.0);
        case("1K", &[], 1e3);
        case("1 K", &[], 1e3);
        case("1k", &[], 1e3);
        case("1.5M", &[], 1.5e6);
        case("100G", &[], 1e11);
        case("1T", &[], 1e12);
        case("314P", &[], 314e15);
        case("1E", &[], 1e18);
        case("1 TH/s", &["H/s"], 1e12);
        case("1 TH", &["H/s", "H"], 1e12);
    }

    #[test]
    fn parse_errors() {
        #[track_caller]
        fn case(input: &str) {
            assert!(parse_si(input, &[]).is_err(), "should reject: {input}");
        }

        case("");
        case("   ");
        case("abc");
        case("-1");
        case("-1K");
        case("NaN");
        case("Infinity");
    }
}
