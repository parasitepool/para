use {super::*, primitive_types::U256};

lazy_static! {
    pub static ref DIFFICULTY_1_TARGET: U256 = U256::from_big_endian(&Target::MAX.to_be_bytes());
}

/// The difficulty is kinda fraught because it's only really meant for human readability and
/// understanding but used in protocol messages anyways. It's usually just a u64 but can be a f64
/// sometimes. If it is an f64 conversion becomes lossy for values > 2^53. In the stratum protocol
/// all mining.set_difficulty messages are supposed to be only u64 so that's why I decided to go
/// with this. There is also the compact representation called nbits.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Serialize)]
pub struct Difficulty(pub u64);

impl Difficulty {
    pub fn to_target(&self) -> Target {
        let target_be_bytes = DIFFICULTY_1_TARGET
            .checked_div(U256::from(self.0))
            .expect("difficulty must not be 0")
            .to_big_endian();

        Target::from_be_bytes(target_be_bytes)
    }
}

impl Default for Difficulty {
    fn default() -> Self {
        Self(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_target_to_difficulty_1() {
        assert_eq!(Difficulty(1).to_target(), Target::MAX);
    }
}
