use super::*;

#[derive(Debug, PartialEq)]
pub struct Subscribe {
    pub user_agent: String,
    pub enonce1: Option<Extranonce>,
}

impl Serialize for Subscribe {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let len = if self.enonce1.is_some() { 2 } else { 1 };
        let mut seq = serializer.serialize_seq(Some(len))?;
        seq.serialize_element(&self.user_agent)?;
        if let Some(x1) = &self.enonce1 {
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
            Other(Vec<Value>),
        }

        match Raw::deserialize(deserializer)? {
            Raw::One((user_agent,)) => Ok(Subscribe {
                user_agent,
                enonce1: None,
            }),
            Raw::Two((user_agent, enonce1_str)) => {
                let enonce1 = enonce1_str.and_then(|s| s.parse::<Extranonce>().ok());
                Ok(Subscribe {
                    user_agent,
                    enonce1,
                })
            }
            Raw::Other(v) if v.is_empty() => Ok(Subscribe {
                user_agent: String::new(),
                enonce1: None,
            }),
            Raw::Other(_) => Err(de::Error::custom("unexpected subscribe params")),
        }
    }
}

#[derive(Debug, PartialEq, Clone)]
pub struct SubscribeResult {
    pub subscriptions: Vec<(String, String)>,
    pub enonce1: Extranonce,
    pub enonce2_size: usize,
}

impl Serialize for SubscribeResult {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut seq = serializer.serialize_seq(Some(3))?;
        seq.serialize_element(&self.subscriptions)?;
        seq.serialize_element(&self.enonce1)?;
        seq.serialize_element(&self.enonce2_size)?;
        seq.end()
    }
}

impl<'de> Deserialize<'de> for SubscribeResult {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let (subscriptions, enonce1, enonce2_size) =
            <(Vec<(String, String)>, Extranonce, usize)>::deserialize(deserializer)?;

        Ok(SubscribeResult {
            subscriptions,
            enonce1,
            enonce2_size,
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
                enonce1: None,
            },
        );
    }

    #[test]
    fn subscribe_user_agent_and_enonce1() {
        case::<Subscribe>(
            r#"["para/BM1623/0.1","abcd1234"]"#,
            Subscribe {
                user_agent: "para/BM1623/0.1".into(),
                enonce1: Some("abcd1234".parse().unwrap()),
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
                enonce1: None
            }
        );

        let ser = serde_json::to_string(&parsed).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&ser).unwrap(),
            serde_json::json!(["ua"])
        );
    }

    #[test]
    fn subscribe_ignores_invalid_hex() {
        let json = r#"["whatsminer/v1.0","b08cf00d1"]"#;
        let parsed: Subscribe = serde_json::from_str(json).unwrap();
        assert_eq!(
            parsed,
            Subscribe {
                user_agent: "whatsminer/v1.0".into(),
                enonce1: None
            }
        );

        let ser = serde_json::to_string(&parsed).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&ser).unwrap(),
            serde_json::json!(["whatsminer/v1.0"])
        );
    }

    #[test]
    fn subscribe_serialize_shapes() {
        let a = Subscribe {
            user_agent: "my_miner".into(),
            enonce1: None,
        };
        assert_eq!(
            serde_json::to_value(&a).unwrap(),
            serde_json::json!(["my_miner"])
        );

        let b = Subscribe {
            user_agent: "my_miner".into(),
            enonce1: Some("cafedade".parse().unwrap()),
        };
        assert_eq!(
            serde_json::to_value(&b).unwrap(),
            serde_json::json!(["my_miner", "cafedade"])
        );
    }

    #[test]
    fn subscribe_empty_params() {
        let parsed: Subscribe = serde_json::from_str("[]").unwrap();
        assert_eq!(
            parsed,
            Subscribe {
                user_agent: String::new(),
                enonce1: None,
            }
        );
    }

    #[test]
    fn subscribe_unexpected_params() {
        assert!(serde_json::from_str::<Subscribe>("[123]").is_err());
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
            enonce1: "08000002".parse().unwrap(),
            enonce2_size: 4,
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
            enonce1: "deadbeef".parse().unwrap(),
            enonce2_size: 8,
        };

        let json = r#"[[], "deadbeef", 8]"#;
        case::<SubscribeResult>(json, sr);
    }

    #[test]
    fn subscribe_result_serialize_shape() {
        let enonce1 = Extranonce::random(8);
        let sr = SubscribeResult {
            subscriptions: vec![("mining.notify".into(), "tag".into())],
            enonce1: enonce1.clone(),
            enonce2_size: 16,
        };

        let v = serde_json::to_value(&sr).unwrap();
        assert_eq!(
            v,
            serde_json::json!([[["mining.notify", "tag"]], enonce1.to_string(), 16])
        );
    }
}
