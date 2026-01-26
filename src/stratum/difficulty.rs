use {super::*, core::cmp::Ordering, primitive_types::U256, si::format_si};

pub static DIFFICULTY_1_TARGET: LazyLock<U256> =
    LazyLock::new(|| U256::from_big_endian(&Target::MAX.to_be_bytes()));

/// Difficulty is a fraught metric. It is derived from the network target, where the
/// difficulty equals the current network target divided by the network target defined in the genesis block.
/// The target principally represents a 256-bit number but the block header
/// contains a compact representation called nbits (or CompactTarget). This is inherently lossy.
/// Furthermore difficulty, which used to be only for human consumption has made itself into the
/// stratum protocol to define the target (inverse of difficulty) a miner's share has to meet. It
/// is used to tune the frequency of a miner's share submission and for accounting how much work
/// has been completed. This struct aims to define it's edges and make it easier to work with but
/// it is inherently lossy and imprecise. If someone stumbles accross this comment and sees a
/// better way to reconcile these different types, please open an issue or PR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Difficulty(CompactTarget);

impl Difficulty {
    pub fn to_target(self) -> Target {
        self.0.into()
    }

    pub fn as_f64(self) -> f64 {
        Target::from_compact(self.0).difficulty_float()
    }
}

impl From<BlockHash> for Difficulty {
    fn from(hash: BlockHash) -> Self {
        let target = Target::from_le_bytes(hash.to_byte_array());
        Difficulty(target.to_compact_lossy())
    }
}

impl Ord for Difficulty {
    fn cmp(&self, other: &Self) -> Ordering {
        let target_self = self.to_target();
        let target_other = other.to_target();

        // Reverse the target order: lower target = higher difficulty
        target_other.cmp(&target_self)
    }
}

impl PartialOrd for Difficulty {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Default for Difficulty {
    fn default() -> Self {
        Difficulty::from(1)
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

        let target = *DIFFICULTY_1_TARGET / U256::from(difficulty);

        Self::from(Target::from_be_bytes(target.to_big_endian()))
    }
}

impl From<f64> for Difficulty {
    fn from(difficulty: f64) -> Self {
        assert!(
            difficulty.is_finite() && difficulty > 0.0,
            "difficulty must be finite and > 0"
        );

        // 2^32 - 1 is safe: DIFFICULTY_1_TARGET (2^224) * scale fits in 256 bits.
        const MAX_SCALE_NUM: u64 = 0xFFFF_FFFF;

        let max_by_den = (u64::MAX as f64 / difficulty).floor();
        let scale = max_by_den.min(MAX_SCALE_NUM as f64).max(1.0) as u64;

        let numerator = (*DIFFICULTY_1_TARGET).saturating_mul(U256::from(scale));
        let denominator = (difficulty * scale as f64).round() as u64;

        let target = if denominator == 0 {
            U256::MAX
        } else {
            numerator / U256::from(denominator)
        };

        Difficulty(Target::from_be_bytes(target.to_big_endian()).to_compact_lossy())
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

impl fmt::Display for Difficulty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let d = self.as_f64();

        if d >= 1.0 {
            format_si(d.floor(), "", f)
        } else if let Some(p) = f.precision() {
            write!(f, "{:.*}", p, d)
        } else {
            let s = format!("{:.8}", d);
            let s = s.trim_end_matches('0').trim_end_matches('.');
            f.write_str(s)
        }
    }
}

impl FromStr for Difficulty {
    type Err = InternalError;

    fn from_str(difficulty: &str) -> Result<Self, Self::Err> {
        let difficulty = difficulty.trim();

        if difficulty.is_empty() {
            return Err(InternalError::InvalidValue {
                reason: "difficulty string is empty".to_string(),
            });
        }

        if let Ok(u) = difficulty.parse::<u64>() {
            if u == 0 {
                return Err(InternalError::InvalidValue {
                    reason: "difficulty must be > 0".to_string(),
                });
            }
            return Ok(Difficulty::from(u));
        }

        if let Ok(x) = difficulty.parse::<f64>() {
            if !x.is_finite() || x <= 0.0 {
                return Err(InternalError::InvalidValue {
                    reason: "difficulty must be > 0".to_string(),
                });
            }
            return Ok(Difficulty::from(x));
        }

        Err(InternalError::Parse {
            message: "difficulty must be an integer or float".to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn relative_error(a: f64, b: f64) -> f64 {
        let denom = a.abs().max(b.abs()).max(1.0);
        ((a - b) / denom).abs()
    }

    #[test]
    fn max_target_to_difficulty_1() {
        assert_eq!(Difficulty::from(1.0).to_target(), Target::MAX);
        assert_eq!(Difficulty::from(1).to_target(), Target::MAX);
    }

    #[test]
    fn real_network_diff() {
        let difficulty = Difficulty::from(150839487445890.5);
        assert_eq!(difficulty.as_f64(), 150839487445890.5)
    }

    #[test]
    fn signet_network_diff() {
        let want = 0.0012;
        let got = Difficulty::from(want).as_f64();
        assert!(relative_error(got, want) < 1e-6);
    }

    #[test]
    fn ordering() {
        let a = Difficulty::from(0.5);
        let b = Difficulty::from(1.0);
        let c = Difficulty::from(2.0);
        assert!(a < b && b < c);
    }

    #[test]
    fn equality() {
        let x = Difficulty::from(Nbits::from_str("1d00ffff").unwrap());
        let y = Difficulty::from(Target::MAX);
        assert_eq!(x, y);
        assert_eq!(x.cmp(&y), Ordering::Equal);
    }

    #[test]
    fn difficulty_1_equivalences() {
        let difficulty_1_float = Difficulty::from(1.0);
        let difficulty_1_integer = Difficulty::from(1);
        assert_eq!(difficulty_1_float.to_target(), Target::MAX);
        assert_eq!(difficulty_1_integer.to_target(), Target::MAX);
        assert!(relative_error(difficulty_1_float.as_f64(), 1.0) <= 1e-12);
        assert!(relative_error(difficulty_1_integer.as_f64(), 1.0) <= 1e-12);
    }

    #[test]
    fn extremely_small_and_large_difficulties_dont_panic() {
        for difficulty in &[1e-18, 1e-12, 1e20, 1e24] {
            let x = Difficulty::from(*difficulty);
            x.to_target();
            let y = x.as_f64();
            assert!(y.is_finite() && y > 0.0, "bad as_f64 for {difficulty}: {y}");
        }
    }

    #[test]
    fn roundtrip_target_and_nbits() {
        let nbits = [
            Nbits::from_str("1d00ffff").unwrap(), // Bitcoin genesis / diff=1
            Nbits::from_str("1b0404cb").unwrap(), // historical mainnet example
            Nbits::from_str("1a0ffff0").unwrap(), // arbitrary
            Nbits::from_str("207fffff").unwrap(), // near-min difficulty in compact form
        ];

        for nbit in nbits {
            let diff = Difficulty::from(nbit);
            let diff_from_nbits = diff.as_f64();

            let target = diff.to_target();
            let diff_from_target = Difficulty::from(target).as_f64();

            assert!(
                relative_error(diff_from_nbits, diff_from_target) <= 1e-12,
                "nbits->diff vs target->diff mismatch: nbits={:#x} {} {}",
                nbit.to_compact(),
                diff_from_nbits,
                diff_from_target
            );

            let d = Difficulty::from(nbit);
            let t2 = d.to_target();
            let d2 = Difficulty::from(t2).as_f64();
            assert!(
                relative_error(d.as_f64(), d2) <= 1e-12,
                "diff->target->diff drift: start={} end={}",
                d.as_f64(),
                d2
            );
        }
    }

    #[test]
    fn serialize_less_than_1_as_float() {
        let json = serde_json::to_string(&Difficulty::from(0.5)).unwrap();
        assert!(json.contains('.'), "should serialize as float: {json}");
    }

    #[test]
    fn serialize_greater_than_1_as_int() {
        let json = serde_json::to_string(&Difficulty::from(42)).unwrap();
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

    #[test]
    fn serde_rejects_bad_inputs() {
        for diff in ["0", "0.0", "-1", "-0.001", "NaN", "Infinity", "-Infinity"] {
            assert!(
                serde_json::from_str::<Difficulty>(diff).is_err(),
                "should reject {diff}"
            );
        }
    }

    #[test]
    fn display_si_when_greater_than_1() {
        assert_eq!(format!("{}", Difficulty::from(1)), "1 ");
        assert_eq!(format!("{}", Difficulty::from(42)), "42 ");
        assert_eq!(format!("{}", Difficulty::from(2.9)), "2 ");
        assert_eq!(format!("{}", Difficulty::from(1000)), "1 K");
        assert_eq!(format!("{}", Difficulty::from(1_000_000)), "1 M");

        let large = Difficulty::from(150_000_000_000_000u64);
        let s = large.to_string();
        assert!(s.ends_with(" T"), "expected T suffix, got: {s}");
    }

    #[test]
    fn display_sub_1() {
        let s = format!("{}", Difficulty::from(0.5));
        assert!(s.starts_with("0.5"), "expected 0.5..., got: {s}");
        assert!(!s.ends_with('0'), "should trim trailing zeros: {s}");

        let s = format!("{}", Difficulty::from(0.001));
        assert!(s.starts_with("0.00"), "expected 0.00..., got: {s}");
        assert!(!s.ends_with('0'), "should trim trailing zeros: {s}");
    }

    #[test]
    fn display_respects_precision_flag() {
        assert_eq!(format!("{:.5}", Difficulty::from(0.5)), "0.50000");
        assert_eq!(format!("{:.2}", Difficulty::from(0.125)), "0.13");
    }

    #[track_caller]
    fn case_from_str(s: &str, want: f64) {
        let got = Difficulty::from_str(s).unwrap();
        assert!(
            relative_error(got.as_f64(), want) < 1e-6,
            "parse {s}, got {got}, want {want}"
        );
    }

    #[test]
    fn from_str_int_float_scientific() {
        case_from_str("1", 1.0);
        case_from_str("0.125", 0.125);
        case_from_str("1e6", 1_000_000.0);
        case_from_str("1000", 1000.0);
    }

    #[test]
    fn display_pairs_with_parsed() {
        for s in ["0.5", "0.125"] {
            let d1 = Difficulty::from_str(s).unwrap();
            let s2 = d1.to_string();
            let d2 = Difficulty::from_str(&s2).unwrap();
            assert!(
                relative_error(d1.as_f64(), d2.as_f64()) < 1e-6,
                "roundtrip {s}"
            );
        }
    }
    #[test]
    fn from_str_rejects_bad() {
        for s in [
            "",
            "0",
            "0.0",
            "-1",
            "-0.01",
            "NaN",
            "Infinity",
            "-Infinity",
            "nope",
        ] {
            assert!(Difficulty::from_str(s).is_err(), "should reject {s}");
        }
    }

    #[test]
    fn from_blockhash_max_target_is_difficulty_one() {
        let target_max = Target::MAX;
        let hash = BlockHash::from_byte_array(target_max.to_le_bytes());
        let diff = Difficulty::from(hash);

        assert!(
            relative_error(diff.as_f64(), 1.0) < 1e-6,
            "max target hash should be difficulty 1, got {diff}"
        );
    }

    #[test]
    fn from_blockhash_lower_hash_means_higher_difficulty() {
        let easy_target = Target::MAX;
        let easy_u256 = U256::from_big_endian(&easy_target.to_be_bytes());
        let hard_u256 = easy_u256 >> 2;
        let hard_target = Target::from_be_bytes(hard_u256.to_big_endian());

        let easy_hash = BlockHash::from_byte_array(easy_target.to_le_bytes());
        let hard_hash = BlockHash::from_byte_array(hard_target.to_le_bytes());

        let easy_diff = Difficulty::from(easy_hash);
        let hard_diff = Difficulty::from(hard_hash);

        assert!(
            hard_diff > easy_diff,
            "lower hash should mean higher difficulty: hard={hard_diff} easy={easy_diff}"
        );
    }

    #[test]
    fn from_blockhash_difficulty_scales_correctly() {
        let target_1 = Target::MAX;

        let target_bytes = target_1.to_be_bytes();
        let target_u256 = U256::from_big_endian(&target_bytes);
        let half_target_u256 = target_u256 >> 1;
        let half_target = Target::from_be_bytes(half_target_u256.to_big_endian());

        let hash_1 = BlockHash::from_byte_array(target_1.to_le_bytes());
        let hash_2 = BlockHash::from_byte_array(half_target.to_le_bytes());

        let diff_1 = Difficulty::from(hash_1);
        let diff_2 = Difficulty::from(hash_2);

        assert!(
            relative_error(diff_2.as_f64() / diff_1.as_f64(), 2.0) < 1e-6,
            "halving target should double difficulty: diff_1={diff_1} diff_2={diff_2} ratio={}",
            diff_2.as_f64() / diff_1.as_f64()
        );
    }
}
