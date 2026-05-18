use super::*;

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Default, Serialize, Deserialize)]
#[serde(try_from = "f64", into = "f64")]
pub struct HashDays(f64);

impl HashDays {
    pub fn new(value: f64) -> Result<Self> {
        ensure!(
            value.is_finite() && value >= 0.0,
            "hash days must be finite and >= 0, got {value}",
        );

        Ok(Self(value))
    }

    pub fn as_f64(self) -> f64 {
        self.0
    }

    pub(crate) const fn from_raw(value: f64) -> Self {
        Self(value)
    }

    pub fn to_hash_work(self) -> HashWork {
        HashWork::from_raw(saturating_finite(
            self.0 * (SECONDS_PER_DAY / HASHES_PER_DIFF_1 as f64),
        ))
    }

    pub fn from_hash_work(work: HashWork) -> Self {
        work.to_hash_days()
    }

    pub fn target_hashrate(self) -> HashRate {
        HashRate::from_hps(self.0)
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
        let work = HashWork::new(SECONDS_PER_DAY / HASHES_PER_DIFF_1 as f64).unwrap();
        let hd = work.to_hash_days();
        assert!((hd.as_f64() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn inverse_conversion() {
        let work = HashWork::new(42.0).unwrap();
        let hd = work.to_hash_days();
        let roundtrip = hd.to_hash_work();
        assert!((roundtrip.as_f64() - 42.0).abs() < 1e-9);
    }

    #[test]
    fn from_hash_work_matches_method() {
        let work = HashWork::new(42.0).unwrap();
        assert_eq!(HashDays::from_hash_work(work), work.to_hash_days());
    }

    #[test]
    fn hash_days_to_hash_work_round_trips() {
        let hash_days = HashDays::new(1.25e15).unwrap();
        let roundtrip = HashDays::from_hash_work(hash_days.to_hash_work());
        let rel_err = ((roundtrip.as_f64() - hash_days.as_f64()) / hash_days.as_f64()).abs();

        assert!(
            rel_err < 1e-12,
            "got {}, want {}",
            roundtrip.as_f64(),
            hash_days.as_f64()
        );
    }

    #[test]
    fn to_hash_work_does_not_panic_for_extreme_finite_value() {
        let work = HashDays::new(f64::MAX).unwrap().to_hash_work();

        assert!(work.as_f64().is_finite());
    }

    #[test]
    fn to_hash_work_nan_maps_to_zero() {
        assert_eq!(HashDays::from_raw(f64::NAN).to_hash_work(), HashWork::ZERO);
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
        fn case(hash_days: f64, expected_hashrate: f64) {
            let actual = HashDays::new(hash_days).unwrap().target_hashrate().as_hps();
            let rel_err = if expected_hashrate == 0.0 {
                actual
            } else {
                ((actual - expected_hashrate) / expected_hashrate).abs()
            };
            assert!(
                rel_err < 1e-10,
                "target_hashrate({hash_days}): got {actual}, want {expected_hashrate}",
            );
        }

        case(0.0, 0.0);
        case(1e15, 1e15);
        case(2e15, 2e15);
        case(0.5e15, 0.5e15);
        case(1e12, 1e12);
    }

    #[test]
    fn target_hashrate_saturates_extreme_finite_value() {
        assert_eq!(
            HashDays::new(f64::MAX).unwrap().target_hashrate().as_hps(),
            f64::MAX,
        );
    }
}
