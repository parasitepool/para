use super::*;

const DELIVERY_WINDOW_BLOCKS: u32 = 144;

#[derive(
    Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct HashPrice(Amount);

impl HashPrice {
    pub fn from_hash_value(hash_value: HashValue, premium_percent: f64, multiplier: f64) -> Self {
        let sats =
            (hash_value.to_sats() as f64 * (100.0 + premium_percent) / 100.0 * multiplier).ceil();

        if sats >= u64::MAX as f64 {
            return Self::from_sats(u64::MAX);
        }

        Self::from_sats(sats as u64)
    }

    pub fn from_sats(sats: u64) -> Self {
        Self(Amount::from_sat(sats))
    }

    pub fn to_sats(self) -> u64 {
        self.0.to_sat()
    }

    pub fn from_total(amount: Amount, hash_days: HashDays) -> Self {
        Self::from_sats((amount.to_sat() as f64 * PETA / hash_days.as_f64()).floor() as u64)
    }

    pub fn total(self, hash_days: HashDays) -> Option<Amount> {
        let sats = (self.to_sats() as f64 * hash_days.as_f64() / PETA).ceil();

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

fn multiplier(height: u64, start_time: u32, now: u64) -> f64 {
    let tip = height.saturating_sub(1);
    let blocks_into_epoch = tip % u64::from(DIFFCHANGE_INTERVAL);
    let blocks_until_adjustment = DIFFCHANGE_INTERVAL - blocks_into_epoch as u32;

    if blocks_until_adjustment > DELIVERY_WINDOW_BLOCKS {
        return 1.0;
    }

    let elapsed = now.saturating_sub(u64::from(start_time));

    if elapsed == 0 {
        return 1.0;
    }

    let change = (f64::from(TARGET_BLOCK_SPACING) * (blocks_into_epoch + 1) as f64
        / elapsed as f64)
        .clamp(0.25, 4.0);

    if change >= 1.0 {
        return 1.0;
    }

    let before = f64::from(blocks_until_adjustment - 1) / f64::from(DELIVERY_WINDOW_BLOCKS);

    before + (1.0 - before) / change
}

pub(crate) async fn difficulty_multiplier(client: &BitcoindClient, height: u64) -> f64 {
    let tip = height.saturating_sub(1);
    let blocks_into_epoch = tip % u64::from(DIFFCHANGE_INTERVAL);

    if DIFFCHANGE_INTERVAL - blocks_into_epoch as u32 > DELIVERY_WINDOW_BLOCKS {
        return 1.0;
    }

    let start_time = match client.get_block_header_at(tip - blocks_into_epoch).await {
        Ok(header) => header.time,
        Err(err) => {
            warn!("Failed to fetch epoch start header: {err}");
            return 1.0;
        }
    };

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    multiplier(height, start_time, now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiplier() {
        #[track_caller]
        fn case(height: u64, start_time: u32, now: u64, expected: f64) {
            let multiplier = super::multiplier(height, start_time, now);
            assert!(
                (multiplier - expected).abs() < 1e-9,
                "expected multiplier {expected}, got {multiplier}",
            );
        }

        case(102, 0, 1_000_000, 1.0);
        case(1872, 0, 1_000_000, 1.0);
        case(1873, 0, 2_247_600, 143.0 / 144.0 + 1.0 / 144.0 / 0.5);
        case(1947, 0, 2_336_400, 69.0 / 144.0 + 75.0 / 144.0 / 0.5);
        case(2016, 0, 2_419_200, 2.0);
        case(1947, 0, 584_100, 1.0);
        case(1947, 0, 11_682_000, 69.0 / 144.0 + 75.0 / 144.0 / 0.25);
        case(1947, 0, 100, 1.0);
        case(2016, 100, 0, 1.0);
    }

    #[test]
    fn from_hash_value() {
        #[track_caller]
        fn case(hash_value: u64, premium_percent: f64, multiplier: f64, expected: u64) {
            assert_eq!(
                HashPrice::from_hash_value(
                    HashValue::from_sats(hash_value),
                    premium_percent,
                    multiplier,
                ),
                HashPrice::from_sats(expected),
            );
        }

        case(100, 5.0, 1.0, 105);
        case(100, 0.0, 1.0, 100);
        case(100, 10.0, 1.0, 110);
        case(100, 5.0, 2.0, 210);
    }

    #[test]
    fn total() {
        #[track_caller]
        fn case(price: u64, hash_days: f64, expected: u64) {
            assert_eq!(
                HashPrice::from_sats(price)
                    .total(HashDays::new(hash_days).unwrap())
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
    fn total_charges_sats_per_phd() {
        assert_eq!(
            HashPrice::from_sats(1234)
                .total(HashDays::new(PETA).unwrap())
                .unwrap(),
            Amount::from_sat(1234),
        );
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
    fn from_total_round_trips_with_total() {
        #[track_caller]
        fn case(price: u64, hash_days: f64) {
            let hd = HashDays::new(hash_days).unwrap();
            let amount = HashPrice::from_sats(price).total(hd).unwrap();
            assert_eq!(
                HashPrice::from_total(amount, hd),
                HashPrice::from_sats(price)
            );
        }

        case(50000, 1e15);
        case(50000, 2e15);
        case(50000, 500e12);
        case(1000, 1e15);
        case(1234, PETA);
    }

    #[test]
    fn ordering() {
        assert!(HashPrice::from_sats(1000) < HashPrice::from_sats(2000));
        assert!(HashPrice::from_sats(2000) >= HashPrice::from_sats(1000));
    }
}
