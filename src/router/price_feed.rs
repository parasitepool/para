use {
    super::*,
    crate::{generator::get_block_template, hash::price::difficulty_multiplier},
};

pub(crate) struct PriceFeed {
    hash_value: AtomicU64,
    difficulty_multiplier: AtomicU64,
}

impl PriceFeed {
    pub(crate) fn new(initial_hash_value: HashValue) -> Self {
        Self {
            hash_value: AtomicU64::new(initial_hash_value.to_sats()),
            difficulty_multiplier: AtomicU64::new(1.0f64.to_bits()),
        }
    }

    pub(crate) fn hash_value(&self) -> HashValue {
        HashValue::from_sats(self.hash_value.load(Ordering::Relaxed))
    }

    pub(crate) fn hash_price(&self, premium_percent: f64) -> HashPrice {
        HashPrice::from_hash_value(
            self.hash_value(),
            premium_percent,
            f64::from_bits(self.difficulty_multiplier.load(Ordering::Relaxed)),
        )
    }

    pub(crate) fn set_difficulty_multiplier(&self, multiplier: f64) {
        self.difficulty_multiplier
            .store(multiplier.to_bits(), Ordering::Relaxed);
    }

    pub(crate) fn difficulty_multiplier(&self) -> f64 {
        f64::from_bits(self.difficulty_multiplier.load(Ordering::Relaxed))
    }

    pub(crate) fn set_hash_value(&self, hash_value: HashValue) {
        self.hash_value
            .store(hash_value.to_sats(), Ordering::Relaxed);
    }

    pub(crate) async fn update(&self, bitcoin_client: &Arc<BitcoindClient>, settings: &Settings) {
        match get_block_template(bitcoin_client, settings).await {
            Ok(template) => {
                self.set_hash_value(HashValue::compute(template.coinbase_value, template.bits));

                let multiplier = difficulty_multiplier(bitcoin_client, template.height).await;

                self.set_difficulty_multiplier(multiplier);
            }
            Err(err) => warn!("Failed to update hash value: {err}"),
        }
    }
}
