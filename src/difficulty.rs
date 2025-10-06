use {super::*, primitive_types::U256};

lazy_static! {
    pub static ref DIFFICULTY_1_TARGET: U256 = U256::from_big_endian(&Target::MAX.to_be_bytes());
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Difficulty(CompactTarget);

impl Difficulty {
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
        if d < 1.0 {
            ser.serialize_f64(d)
        } else {
            ser.serialize_u64(d.floor() as u64)
        }
    }
}

impl<'de> Deserialize<'de> for Difficulty {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
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

        const SCALE: u64 = 1_000_000_000;

        let numerator = DIFFICULTY_1_TARGET.saturating_mul(U256::from(SCALE));
        let denominator = (diff * SCALE as f64).round() as u64;

        let target = if denominator == 0 {
            U256::MAX
        } else {
            numerator / U256::from(denominator)
        };

        Difficulty(Target::from_be_bytes(target.to_big_endian()).to_compact_lossy())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_target_to_difficulty_1() {
        assert_eq!(Difficulty::from(1.0).target(), Target::MAX);
        assert_eq!(Difficulty::from(1).target(), Target::MAX);
    }

    #[test]
    fn serialize_less_than_1_as_float() {
        let json = serde_json::to_string(&Difficulty::from(0.5_f64)).unwrap();
        assert!(json.contains('.'), "should serialize as float: {json}");
    }

    #[test]
    fn serialize_greater_than_1_as_int() {
        let json = serde_json::to_string(&Difficulty::from(42_u64)).unwrap();
        assert_eq!(json, "42");
    }

    #[test]
    fn deserialize_from_int_or_float() {
        let a: Difficulty = serde_json::from_str("2").unwrap();
        let b: Difficulty = serde_json::from_str("2.0").unwrap();
        let c: Difficulty = serde_json::from_str("0.125").unwrap();

        assert!(a.as_f64() >= 1.0);
        assert!(b.as_f64() >= 1.0);
        assert!(c.as_f64() < 1.0);
    }
}
