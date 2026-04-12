use super::*;

#[derive(
    Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct HashPrice(Amount);

impl HashPrice {
    pub fn from_sats(sats: u64) -> Self {
        Self(Amount::from_sat(sats))
    }

    pub fn to_sats(self) -> u64 {
        self.0.to_sat()
    }

    pub fn total(self, hashdays: HashDays) -> Option<Amount> {
        let sats = (self.to_sats() as f64 * hashdays.as_f64() / 1e15).ceil();

        if !sats.is_finite() || sats < 0.0 || sats > u64::MAX as f64 {
            return None;
        }

        Some(Amount::from_sat(sats as u64))
    }
}

impl Display for HashPrice {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{} sats/PHd", self.to_sats())
    }
}

impl FromStr for HashPrice {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let sats: u64 = s
            .parse()
            .with_context(|| format!("invalid hash price {s:?}"))?;

        Ok(Self::from_sats(sats))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn total() {
        #[track_caller]
        fn case(price: u64, hashdays: f64, expected: u64) {
            assert_eq!(
                HashPrice::from_sats(price)
                    .total(HashDays::new(hashdays).unwrap())
                    .unwrap(),
                Amount::from_sat(expected),
            );
        }

        case(50000, 1e15, 50000);
        case(50000, 2e15, 100000);
        case(50000, 500e12, 25000);
        case(1000, 1e15, 1000);
        case(1000, 1e12, 1);
    }

    #[test]
    fn total_overflow_returns_none() {
        assert_eq!(
            HashPrice::from_sats(u64::MAX).total(HashDays::new(1e18).unwrap()),
            None,
        );
    }

    #[test]
    fn display() {
        assert_eq!(HashPrice::from_sats(1000).to_string(), "1000 sats/PHd");
    }

    #[test]
    fn parse() {
        assert_eq!(
            "1000".parse::<HashPrice>().unwrap(),
            HashPrice::from_sats(1000),
        );
        assert!("".parse::<HashPrice>().is_err());
        assert!("-1".parse::<HashPrice>().is_err());
        assert!("abc".parse::<HashPrice>().is_err());
    }

    #[test]
    fn serde_roundtrip() {
        let price = HashPrice::from_sats(50000);
        let json = serde_json::to_string(&price).unwrap();
        assert_eq!(json, "50000");
        assert_eq!(serde_json::from_str::<HashPrice>(&json).unwrap(), price);
    }

    #[test]
    fn ordering() {
        assert!(HashPrice::from_sats(1000) < HashPrice::from_sats(2000));
        assert!(HashPrice::from_sats(2000) >= HashPrice::from_sats(1000));
    }
}
