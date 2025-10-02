use super::*;

use primitive_types::U256;

lazy_static! {
    pub static ref DIFFICULTY_1_TARGET: U256 = U256::from_big_endian(&Target::MAX.to_be_bytes());
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Difficulty(pub u64); // integer-only difficulty (>= 1)

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FromTargetError {
    ZeroTarget,                 // target == 0 (invalid)
    FractionalUnderOne,         // D1/target ∈ (0,1) → q=0
    FractionalNonInteger(U256), // remainder != 0 (not an integer difficulty)
    Overflow(U256),             // integer diff > u64::MAX
}

/// Try to interpret `target` as an *integer* difficulty:
/// diff := D1 / target. Succeeds iff diff is a whole number in [1 .. u64::MAX].
impl Difficulty {
    pub fn from_target_integer(target: U256) -> Result<Self, FromTargetError> {
        if target.is_zero() { return Err(FromTargetError::ZeroTarget); }

        let d1 = DIFFICULTY_1_TARGET;
        let q = d1 / target;
        let r = d1 % target;

        // If q == 0, then diff < 1 → not representable as integer difficulty.
        if q.is_zero() {
            return Err(FromTargetError::FractionalUnderOne);
        }
        // If remainder != 0, the ratio isn't an integer.
        if !r.is_zero() {
            return Err(FromTargetError::FractionalNonInteger(r));
        }
        // Must fit in u64.
        if (q >> 64) != U256::zero() {
            return Err(FromTargetError::Overflow(q));
        }
        Ok(Difficulty(q.low_u64()))
    }

    /// Back to target (floor exact here since `self` is integer):
    /// target = D1 / diff, clamped to protocol cap implicitly by integer division.
    pub fn to_target(self) -> U256 {
        D1() / U256::from(self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_one_ok() {
        let t = D1();
        let d = Difficulty::from_target_integer(t).unwrap();
        assert_eq!(d.0, 1);
        assert_eq!(d.to_target(), t);
    }

    #[test]
    fn fractional_under_one_rejected() {
        // target > D1 ⇒ diff ∈ (0,1)
        let t = D1() + U256::from(1u8);
        assert!(matches!(
            Difficulty::from_target_integer(t),
            Err(FromTargetError::FractionalUnderOne)
        ));
    }

    #[test]
    fn fractional_non_integer_rejected() {
        // Make target = D1 / 3 + 1 → non-integer quotient
        let t = (D1() / U256::from(3u8)) + U256::from(1u8);
        assert!(matches!(
            Difficulty::from_target_integer(t),
            Err(FromTargetError::FractionalNonInteger(_))
        ));
    }

    #[test]
    fn overflow_rejected() {
        // Make tiny target so diff > u64::MAX
        let tiny = U256::from(1u8);
        assert!(matches!(
            Difficulty::from_target_integer(tiny),
            Err(FromTargetError::Overflow(_))
        ));
    }
}
