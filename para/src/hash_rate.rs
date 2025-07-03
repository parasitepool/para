use super::*;

const UNITS: &[&str] = &[
    "H/s", "kH/s", "MH/s", "GH/s", "TH/s", "PH/s", "EH/s", "ZH/s", "YH/s",
];

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct HashRate(pub f64);

impl fmt::Display for HashRate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut rate = self.0;
        let mut unit = 0;
        while rate >= 1000.0 && unit < UNITS.len() - 1 {
            rate /= 1000.0;
            unit += 1;
        }
        write!(f, "{:.2} {}", rate, UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hashrate_formatting() {
        let cases = [
            (0.0, "0.00 H/s"),
            (999.0, "999.00 H/s"),
            (1_000.0, "1.00 kH/s"),
            (1_500_000.0, "1.50 MH/s"),
            (2_000_000_000.0, "2.00 GH/s"),
            (3.5e12, "3.50 TH/s"),
            (4.2e15, "4.20 PH/s"),
            (5.1e18, "5.10 EH/s"),
            (6.6e21, "6.60 ZH/s"),
            (7.7e24, "7.70 YH/s"),
            (1e30, "1000000.00 YH/s"),
        ];

        for (input, expected) in cases {
            let hr = HashRate(input);
            assert_eq!(hr.to_string(), expected, "Failed on input {input}");
        }
    }
}
