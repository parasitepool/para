use super::*;

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Default, Serialize, Deserialize)]
#[serde(try_from = "f64", into = "f64")]
pub struct HashDays(f64);

impl HashDays {
    pub fn new(value: f64) -> Result<Self> {
        ensure!(
            value.is_finite() && value >= 0.0,
            "hashdays must be finite and >= 0, got {value}",
        );

        Ok(Self(value))
    }

    pub fn as_f64(self) -> f64 {
        self.0
    }

    pub fn to_total_work(self) -> TotalWork {
        TotalWork::from_raw(self.0 * 86_400.0 / HASHES_PER_DIFF_1 as f64)
    }

    pub fn target_hashrate(self) -> HashRate {
        HashRate::from_dsps(self.to_total_work().as_f64() / 86_400.0)
    }
}

impl TryFrom<f64> for HashDays {
    type Error = Error;

    fn try_from(value: f64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<HashDays> for f64 {
    fn from(value: HashDays) -> Self {
        value.0
    }
}

impl Display for HashDays {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        format_si(self.0, "Hd", f)
    }
}

impl FromStr for HashDays {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(parse_si(s, &["Hd"])?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversion() {
        let work = TotalWork::new(86_400.0 / HASHES_PER_DIFF_1 as f64).unwrap();
        let hd = work.to_hash_days();
        assert!((hd.as_f64() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn inverse_conversion() {
        let work = TotalWork::new(42.0).unwrap();
        let hd = work.to_hash_days();
        let roundtrip = hd.to_total_work();
        assert!((roundtrip.as_f64() - 42.0).abs() < 1e-9);
    }

    #[test]
    fn display() {
        #[track_caller]
        fn case(value: f64, expected: &str) {
            assert_eq!(HashDays::new(value).unwrap().to_string(), expected);
        }

        case(0.0, "0 Hd");
        case(1e3, "1 KHd");
        case(1e6, "1 MHd");
        case(1e9, "1 GHd");
        case(1e12, "1 THd");
        case(1e15, "1 PHd");
        case(1e18, "1 EHd");
        case(1.5e12, "1.5 THd");
    }

    #[test]
    fn parse() {
        #[track_caller]
        fn case(input: &str, expected: f64) {
            let hd: HashDays = input.parse().unwrap();
            let rel_err = if expected == 0.0 {
                hd.as_f64()
            } else {
                ((hd.as_f64() - expected) / expected).abs()
            };
            assert!(
                rel_err < 1e-10,
                "parse({input}): got {}, want {expected}",
                hd.as_f64(),
            );
        }

        case("0", 0.0);
        case("0 Hd", 0.0);
        case("1 KHd", 1e3);
        case("1.5 THd", 1.5e12);
        case("1 PHd", 1e15);
        case("1 EHd", 1e18);
        case("500T", 500e12);
    }

    #[test]
    fn parse_errors() {
        #[track_caller]
        fn case(input: &str) {
            assert!(input.parse::<HashDays>().is_err(), "should reject: {input}");
        }

        case("");
        case("abc");
        case("-1");
        case("NaN");
        case("Infinity");
    }

    #[test]
    fn new_rejects_invalid_values() {
        assert!(HashDays::new(-1.0).is_err());
        assert!(HashDays::new(f64::NAN).is_err());
        assert!(HashDays::new(f64::INFINITY).is_err());
    }

    #[test]
    fn serde_roundtrip() {
        let hd = HashDays::new(1.5e12).unwrap();
        let json = serde_json::to_string(&hd).unwrap();
        let parsed: HashDays = serde_json::from_str(&json).unwrap();
        assert_eq!(hd, parsed);
    }

    #[test]
    fn serde_rejects_invalid_values() {
        assert!(serde_json::from_str::<HashDays>("-1.0").is_err());
    }

    #[test]
    fn target_hashrate() {
        #[track_caller]
        fn case(hashdays: f64, expected_hashrate: f64) {
            let actual = HashDays::new(hashdays).unwrap().target_hashrate().0;
            let rel_err = if expected_hashrate == 0.0 {
                actual
            } else {
                ((actual - expected_hashrate) / expected_hashrate).abs()
            };
            assert!(
                rel_err < 1e-10,
                "target_hashrate({hashdays}): got {actual}, want {expected_hashrate}",
            );
        }

        case(0.0, 0.0);
        case(1e15, 1e15);
        case(2e15, 2e15);
        case(0.5e15, 0.5e15);
        case(1e12, 1e12);
    }
}
