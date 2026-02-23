use super::*;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SuggestDifficulty(pub Difficulty);

impl SuggestDifficulty {
    pub fn difficulty(self) -> Difficulty {
        self.0
    }
}

impl From<Difficulty> for SuggestDifficulty {
    fn from(d: Difficulty) -> Self {
        SuggestDifficulty(d)
    }
}
impl From<SuggestDifficulty> for Difficulty {
    fn from(s: SuggestDifficulty) -> Self {
        s.0
    }
}

impl Serialize for SuggestDifficulty {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut seq = serializer.serialize_seq(Some(1))?;
        seq.serialize_element(&self.0)?;
        seq.end()
    }
}

impl<'de> Deserialize<'de> for SuggestDifficulty {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let (d,): (Difficulty,) = Deserialize::deserialize(deserializer)?;
        Ok(SuggestDifficulty(d))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggest_difficulty_roundtrip() {
        let expected = SuggestDifficulty(Difficulty::from(1000));
        let parsed: SuggestDifficulty = serde_json::from_str("[1000]").unwrap();
        assert_eq!(parsed, expected);

        let ser = serde_json::to_string(&parsed).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&ser).unwrap(),
            serde_json::json!([1000])
        );

        let back: SuggestDifficulty = serde_json::from_str(&ser).unwrap();
        assert_eq!(back, expected);
    }

    #[test]
    fn suggest_difficulty_serialize_shape() {
        let v = serde_json::to_value(SuggestDifficulty(Difficulty::from(2))).unwrap();
        assert_eq!(v, serde_json::json!([2]));
    }

    #[test]
    fn suggest_difficulty_reject_bad_arity() {
        assert!(serde_json::from_str::<SuggestDifficulty>("[]").is_err());
        assert!(serde_json::from_str::<SuggestDifficulty>("[2,3]").is_err());
    }
}
