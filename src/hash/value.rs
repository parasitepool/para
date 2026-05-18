use super::*;

#[derive(
    Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct HashValue(Amount);

impl HashValue {
    pub fn compute(coinbase_value: Amount, nbits: Nbits) -> Self {
        let difficulty = Difficulty::from(nbits).as_f64();

        let sats = coinbase_value.to_sat() as f64 * SECONDS_PER_DAY * PETA
            / (difficulty * HASHES_PER_DIFF_1 as f64);

        Self::from_sats(saturating_rounded_sats(sats))
    }

    pub fn from_sats(sats: u64) -> Self {
        Self(Amount::from_sat(sats))
    }

    pub fn to_sats(self) -> u64 {
        self.0.to_sat()
    }

    pub fn total(self, hash_days: HashDays) -> Option<Amount> {
        let sats = (self.to_sats() as f64 * hash_days.as_f64() / PETA).ceil();

        if !sats.is_finite() || sats < 0.0 || sats > u64::MAX as f64 {
            return None;
        }

        Some(Amount::from_sat(sats as u64))
    }
}

fn saturating_rounded_sats(sats: f64) -> u64 {
    let sats = sats.round();

    if sats <= 0.0 {
        0
    } else if !sats.is_finite() || sats >= u64::MAX as f64 {
        u64::MAX
    } else {
        sats as u64
    }
}

impl Display for HashValue {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{} sats/PHd", self.to_sats())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute() {
        let hash_value = HashValue::compute(
            Amount::from_btc(50.0).unwrap(),
            CompactTarget::from(Difficulty::from(1_000_000)).into(),
        );

        assert_eq!(hash_value, HashValue::from_sats(100_582_760_248_431));
    }

    #[test]
    fn compute_saturates_extreme_values() {
        let hash_value =
            HashValue::compute(Amount::from_sat(u64::MAX), "207fffff".parse().unwrap());

        assert_eq!(hash_value, HashValue::from_sats(u64::MAX));
    }

    #[test]
    fn total_charges_sats_per_phd() {
        assert_eq!(
            HashValue::from_sats(1234)
                .total(HashDays::new(PETA).unwrap())
                .unwrap(),
            Amount::from_sat(1234),
        );
    }

    #[test]
    fn total_overflow_returns_none() {
        assert_eq!(
            HashValue::from_sats(u64::MAX).total(HashDays::new(1e18).unwrap()),
            None,
        );
    }

    #[test]
    fn display() {
        assert_eq!(HashValue::from_sats(1000).to_string(), "1000 sats/PHd");
    }
}
