use super::*;

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Default, Serialize, Deserialize)]
pub struct HashDays(pub f64);

impl HashDays {
    pub fn to_total_work(self) -> TotalWork {
        TotalWork(self.0 * 86_400.0 / HASHES_PER_DIFF_1 as f64)
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
        Ok(Self(parse_si(s, &["Hd"])?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversion() {
        let work = TotalWork(86_400.0 / HASHES_PER_DIFF_1 as f64);
        let hd = work.to_hash_days();
        assert!((hd.0 - 1.0).abs() < 1e-9);
    }

    #[test]
    fn inverse_conversion() {
        let work = TotalWork(42.0);
        let hd = work.to_hash_days();
        let roundtrip = hd.to_total_work();
        assert!((roundtrip.as_f64() - 42.0).abs() < 1e-9);
    }

    #[test]
    fn display() {
        #[track_caller]
        fn case(value: f64, expected: &str) {
            assert_eq!(HashDays(value).to_string(), expected);
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
                hd.0
            } else {
                ((hd.0 - expected) / expected).abs()
            };
            assert!(
                rel_err < 1e-10,
                "parse({input}): got {}, want {expected}",
                hd.0,
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
    fn serde_roundtrip() {
        let hd = HashDays(1.5e12);
        let json = serde_json::to_string(&hd).unwrap();
        let parsed: HashDays = serde_json::from_str(&json).unwrap();
        assert_eq!(hd, parsed);
    }
}
