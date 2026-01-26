use {
    super::*,
    std::fmt::{self, Formatter},
};

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

pub fn parse_si(s: &str, units: &[&str]) -> Result<f64> {
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
