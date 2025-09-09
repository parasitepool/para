use super::*;

#[derive(Debug, PartialEq)]
pub struct Subscribe {
    pub user_agent: String,
    pub extranonce1: Option<String>,
}

impl Serialize for Subscribe {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let len = if self.extranonce1.is_some() { 2 } else { 1 };
        let mut seq = serializer.serialize_seq(Some(len))?;
        seq.serialize_element(&self.user_agent)?;
        if let Some(x1) = &self.extranonce1 {
            seq.serialize_element(x1)?;
        }
        seq.end()
    }
}

impl<'de> Deserialize<'de> for Subscribe {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            One((String,)),
            Two((String, Option<String>)),
        }

        match Raw::deserialize(deserializer)? {
            Raw::One((ua,)) => Ok(Subscribe {
                user_agent: ua,
                extranonce1: None,
            }),
            Raw::Two((ua, x1)) => Ok(Subscribe {
                user_agent: ua,
                extranonce1: x1,
            }),
        }
    }
}

#[derive(Debug, PartialEq, Clone)]
pub struct SubscribeResult {
    pub subscriptions: Vec<(String, String)>,
    pub extranonce1: Extranonce,
    pub extranonce2_size: u32,
}

impl Serialize for SubscribeResult {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut seq = serializer.serialize_seq(Some(3))?;
        seq.serialize_element(&self.subscriptions)?;
        seq.serialize_element(&self.extranonce1)?;
        seq.serialize_element(&self.extranonce2_size)?;
        seq.end()
    }
}

impl<'de> Deserialize<'de> for SubscribeResult {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let (subscriptions, extranonce1, extranonce2_size) =
            <(Vec<(String, String)>, Extranonce, u32)>::deserialize(deserializer)?;

        Ok(SubscribeResult {
            subscriptions,
            extranonce1,
            extranonce2_size,
        })
    }
}

#[cfg(test)]
mod tests {
    use {super::*, serde::de::DeserializeOwned};

    #[track_caller]
    fn case<T>(json: &str, expected: T)
    where
        T: DeserializeOwned + Serialize + PartialEq + std::fmt::Debug,
    {
        let parsed: T = serde_json::from_str(json).unwrap();
        assert_eq!(parsed, expected, "deserialize equality");

        let ser = serde_json::to_string(&parsed).unwrap();
        let lhs: Value = serde_json::from_str(json).unwrap();
        let rhs: Value = serde_json::from_str(&ser).unwrap();
        assert_eq!(lhs, rhs, "semantic JSON equality");

        let back: T = serde_json::from_str(&ser).unwrap();
        assert_eq!(back, expected, "roundtrip equality");
    }

    #[test]
    fn subscribe_only_user_agent() {
        case::<Subscribe>(
            r#"["paraminer/0.0.1"]"#,
            Subscribe {
                user_agent: "paraminer/0.0.1".into(),
                extranonce1: None,
            },
        );
    }

    #[test]
    fn subscribe_user_agent_and_extranonce1() {
        case::<Subscribe>(
            r#"["para/BM1623/0.1","abcd12345"]"#,
            Subscribe {
                user_agent: "para/BM1623/0.1".into(),
                extranonce1: Some("abcd12345".into()),
            },
        );
    }

    #[test]
    fn subscribe_allows_null_and_normalizes() {
        let json = r#"["ua",null]"#;
        let parsed: Subscribe = serde_json::from_str(json).unwrap();
        assert_eq!(
            parsed,
            Subscribe {
                user_agent: "ua".into(),
                extranonce1: None
            }
        );

        let ser = serde_json::to_string(&parsed).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&ser).unwrap(),
            serde_json::json!(["ua"])
        );
    }

    #[test]
    fn subscribe_serialize_shapes() {
        let a = Subscribe {
            user_agent: "ua".into(),
            extranonce1: None,
        };
        assert_eq!(serde_json::to_value(&a).unwrap(), serde_json::json!(["ua"]));

        let b = Subscribe {
            user_agent: "ua".into(),
            extranonce1: Some("x1".into()),
        };
        assert_eq!(
            serde_json::to_value(&b).unwrap(),
            serde_json::json!(["ua", "x1"])
        );
    }

    #[test]
    fn subscribe_result_roundtrip() {
        let sr = SubscribeResult {
            subscriptions: vec![
                (
                    "mining.set_difficulty".into(),
                    "b4b6693b72a50c7116db18d6497cac52".into(),
                ),
                (
                    "mining.notify".into(),
                    "ae6812eb4cd7735a302a8a9dd95cf71f".into(),
                ),
            ],
            extranonce1: Extranonce::from_str("08000002").unwrap(),
            extranonce2_size: 4,
        };

        let json = r#"
            [
              [
                ["mining.set_difficulty","b4b6693b72a50c7116db18d6497cac52"],
                ["mining.notify","ae6812eb4cd7735a302a8a9dd95cf71f"]
              ],
              "08000002",
              4
            ]
        "#;

        case::<SubscribeResult>(json, sr);
    }

    #[test]
    fn subscribe_result_empty_subscriptions() {
        let sr = SubscribeResult {
            subscriptions: vec![],
            extranonce1: Extranonce::from_str("deadbeef").unwrap(),
            extranonce2_size: 8,
        };

        let json = r#"[[], "deadbeef", 8]"#;
        case::<SubscribeResult>(json, sr);
    }

    #[test]
    fn subscribe_result_serialize_shape() {
        let extranonce1 = Extranonce::generate(EXTRANONCE1_SIZE);
        let sr = SubscribeResult {
            subscriptions: vec![("mining.notify".into(), "tag".into())],
            extranonce1: extranonce1.clone(),
            extranonce2_size: 16,
        };

        let v = serde_json::to_value(&sr).unwrap();
        assert_eq!(
            v,
            serde_json::json!([[["mining.notify", "tag"]], extranonce1.to_string(), 16])
        );
    }
}
