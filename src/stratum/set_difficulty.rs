use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetDifficulty(pub Difficulty);

impl SetDifficulty {
    pub fn difficulty(self) -> Difficulty {
        self.0
    }
}

impl From<Difficulty> for SetDifficulty {
    fn from(d: Difficulty) -> Self {
        SetDifficulty(d)
    }
}
impl From<SetDifficulty> for Difficulty {
    fn from(s: SetDifficulty) -> Self {
        s.0
    }
}

impl Serialize for SetDifficulty {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut seq = serializer.serialize_seq(Some(1))?;
        seq.serialize_element(&self.0)?;
        seq.end()
    }
}

impl<'de> Deserialize<'de> for SetDifficulty {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let (d,): (Difficulty,) = Deserialize::deserialize(deserializer)?;
        Ok(SetDifficulty(d))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_difficulty_roundtrip() {
        let expected = SetDifficulty(Difficulty(9999));
        let parsed: SetDifficulty = serde_json::from_str("[9999]").unwrap();
        assert_eq!(parsed, expected);

        let ser = serde_json::to_string(&parsed).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&ser).unwrap(),
            serde_json::json!([9999])
        );

        let back: SetDifficulty = serde_json::from_str(&ser).unwrap();
        assert_eq!(back, expected);
    }

    #[test]
    fn set_difficulty_serialize_shape() {
        let v = serde_json::to_value(SetDifficulty(Difficulty(3))).unwrap();
        assert_eq!(v, serde_json::json!([3]));
    }

    #[test]
    fn set_difficulty_reject_bad_arity() {
        assert!(serde_json::from_str::<SetDifficulty>("[]").is_err());
        assert!(serde_json::from_str::<SetDifficulty>("[5,11]").is_err());
    }
}
