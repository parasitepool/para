use {super::*, primitive_types::U256};

lazy_static! {
    pub static ref DIFFICULTY_1_TARGET: U256 = U256::from_big_endian(&Target::MAX.to_be_bytes());
}

// #[derive(Debug, Clone, Copy, PartialEq, Deserialize, Serialize, Display)]
// #[serde(transparent)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Difficulty(CompactTarget);

impl Difficulty {
    pub const DIFF1_COMPACT: u32 = 0x1d00_ffff; // TODO: is this correct?

    pub fn target(self) -> Target {
        self.0.into()
    }

    pub fn as_f64(self) -> f64 {
        Target::from_compact(self.0).difficulty_float()
    }
}

impl Serialize for Difficulty {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        let d = self.as_f64();
        // difficulty is always > 0.0 by construction.
        if d < 1.0 {
            ser.serialize_f64(d)
        } else {
            // Serialize as an integer u64 for >= 1.0 (explicit floor).
            ser.serialize_u64(d.floor() as u64)
        }
    }
}

impl<'de> Deserialize<'de> for Difficulty {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        // Accept either an integer or a float on the wire.
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Int(u64),
            Float(f64),
        }

        match Wire::deserialize(de)? {
            Wire::Int(u) => {
                if u == 0 {
                    return Err(de::Error::custom("difficulty must be > 0"));
                }
                Ok(Difficulty::from(u))
            }
            Wire::Float(x) => {
                if !x.is_finite() || x <= 0.0 {
                    return Err(de::Error::custom("difficulty must be finite and > 0"));
                }
                Ok(Difficulty::from(x))
            }
        }
    }
}

impl From<Nbits> for Difficulty {
    fn from(nbits: Nbits) -> Self {
        Difficulty(nbits.into())
    }
}

impl From<CompactTarget> for Difficulty {
    fn from(compact_target: CompactTarget) -> Self {
        Difficulty(compact_target)
    }
}

impl From<Difficulty> for CompactTarget {
    fn from(difficulty: Difficulty) -> Self {
        difficulty.0
    }
}

impl From<Target> for Difficulty {
    fn from(target: Target) -> Self {
        Difficulty(target.to_compact_lossy())
    }
}

impl From<u64> for Difficulty {
    fn from(difficulty: u64) -> Self {
        assert!(difficulty > 0, "difficulty must be > 0");
        Self::from(difficulty as f64)
    }
}

impl From<f64> for Difficulty {
    fn from(diff: f64) -> Self {
        assert!(
            diff.is_finite() && diff > 0.0,
            "difficulty must be finite and > 0"
        );

        let difficulty_1_target = &DIFFICULTY_1_TARGET;
        // Target::from_compact(CompactTarget::from_consensus(Difficulty::DIFF1_COMPACT));

        const SCALE: u64 = 1_000_000_000;

        let num = difficulty_1_target.saturating_mul(U256::from(SCALE));
        let den = (diff * SCALE as f64).round() as u64;

        let target = if den == 0 {
            U256::MAX
        } else {
            num / U256::from(den)
        };

        Difficulty(Target::from_be_bytes(target.to_big_endian()).to_compact_lossy())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{from_str, to_string};

    #[test]
    fn max_target_to_difficulty_1() {
        assert_eq!(Difficulty::from(1.0).target(), Target::MAX);
        assert_eq!(Difficulty::from(1).target(), Target::MAX);
    }

    #[test]
    fn ser_lt_one_as_float() {
        let d = Difficulty::from(0.5_f64);
        let json = to_string(&d).unwrap();
        assert!(json.contains('.'), "should serialize as float: {json}");
    }

    #[test]
    fn ser_ge_one_as_int() {
        let d = Difficulty::from(42_u64);
        let json = to_string(&d).unwrap();
        assert_eq!(json, "42");
    }

    #[test]
    fn de_from_int_or_float() {
        let a: Difficulty = from_str("2").unwrap();
        let b: Difficulty = from_str("2.0").unwrap();
        let c: Difficulty = from_str("0.125").unwrap();

        assert!(a.as_f64() >= 1.0);
        assert!(b.as_f64() >= 1.0);
        assert!(c.as_f64() < 1.0);
    }
}
