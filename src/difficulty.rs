use {super::*, primitive_types::U256};

lazy_static! {
    pub static ref DIFFICULTY_1_TARGET: U256 = U256::from_big_endian(&Target::MAX.to_be_bytes());
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Serialize, Display, Eq)]
#[serde(transparent)]
pub struct Difficulty(pub u64);

impl Difficulty {
    pub fn to_target(self) -> Target {
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
