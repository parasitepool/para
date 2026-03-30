use super::*;

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord, Display, Clone)]
#[serde(untagged)]
pub enum Id {
    Null,
    Number(u64),
    String(String),
}

/// Stratum does id: null, which is technically wrong according to the JSON-RPC spec, which
/// states that no id field should be present. This is a work around to allow both cases. If
/// a server sends a notification with an id field other than null it will be classified as
/// a request and should just be ignored by any client.
#[derive(Debug, PartialEq)]
pub enum Message {
    Request {
        id: Id,
        method: Method,
    },
    Response {
        id: Id,
        result: Option<Value>,
        error: Option<StratumErrorResponse>,
        reject_reason: Option<String>,
    },
    Notification {
        method: Method,
    },
}

struct Params<'a>(&'a Method);

impl Serialize for Params<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize_params(serializer)
    }
}

impl Serialize for Message {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Message::Request { id, method } => {
                let mut map = serializer.serialize_map(Some(3))?;
                map.serialize_entry("id", id)?;
                map.serialize_entry("method", method.method_name())?;
                map.serialize_entry("params", &Params(method))?;
                map.end()
            }
            Message::Response {
                id,
                result,
                error,
                reject_reason,
            } => {
                let len = 3 + reject_reason.is_some() as usize;
                let mut map = serializer.serialize_map(Some(len))?;
                map.serialize_entry("id", id)?;
                map.serialize_entry("result", result)?;
                map.serialize_entry("error", error)?;
                if let Some(reason) = reject_reason {
                    map.serialize_entry("reject-reason", reason)?;
                }
                map.end()
            }
            Message::Notification { method } => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("method", method.method_name())?;
                map.serialize_entry("params", &Params(method))?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for Message {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct MessageVisitor;

        impl<'de> de::Visitor<'de> for MessageVisitor {
            type Value = Message;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a stratum message")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Message, A::Error>
            where
                A: de::MapAccess<'de>,
            {
                let mut id: Option<Id> = None;
                let mut method: Option<&'de str> = None;
                let mut params: Option<&'de serde_json::value::RawValue> = None;
                let mut result: Option<&'de serde_json::value::RawValue> = None;
                let mut error: Option<&'de serde_json::value::RawValue> = None;
                let mut reject_reason: Option<String> = None;

                while let Some(key) = map.next_key::<&str>()? {
                    match key {
                        "id" => id = Some(map.next_value()?),
                        "method" => method = Some(map.next_value()?),
                        "params" => params = Some(map.next_value()?),
                        "result" => result = Some(map.next_value()?),
                        "error" => error = Some(map.next_value()?),
                        "reject-reason" => reject_reason = Some(map.next_value()?),
                        _ => {
                            map.next_value::<de::IgnoredAny>()?;
                        }
                    }
                }

                let is_response = result.is_some() || error.is_some() || reject_reason.is_some();
                let is_notification =
                    method.is_some() && !matches!(id, Some(Id::Number(_) | Id::String(_)));

                if is_response {
                    let id = id.ok_or_else(|| de::Error::missing_field("id"))?;

                    let result = match result {
                        Some(raw) => serde_json::from_str(raw.get()).map_err(de::Error::custom)?,
                        None => None,
                    };

                    let error = match error {
                        Some(raw) => serde_json::from_str(raw.get()).map_err(de::Error::custom)?,
                        None => None,
                    };

                    Ok(Message::Response {
                        id,
                        result,
                        error,
                        reject_reason,
                    })
                } else if is_notification {
                    let method = method.ok_or_else(|| de::Error::missing_field("method"))?;
                    let params = params.ok_or_else(|| de::Error::missing_field("params"))?;
                    let method =
                        Method::from_parts(method, params.get()).map_err(de::Error::custom)?;

                    Ok(Message::Notification { method })
                } else if let Some(method) = method {
                    let id = id.ok_or_else(|| de::Error::missing_field("id"))?;
                    let params = params.ok_or_else(|| de::Error::missing_field("params"))?;
                    let method =
                        Method::from_parts(method, params.get()).map_err(de::Error::custom)?;

                    Ok(Message::Request { id, method })
                } else {
                    Err(de::Error::custom("unknown message format"))
                }
            }
        }

        deserializer.deserialize_map(MessageVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[track_caller]
    fn case(s: &str, expected: Message) {
        let actual = serde_json::from_str::<Message>(s).unwrap();
        assert_eq!(actual, expected, "deserialize Message from str");

        let serialized = serde_json::to_string(&actual).unwrap();
        let lhs: serde_json::Value = serde_json::from_str(s).unwrap();
        let rhs: serde_json::Value = serde_json::from_str(&serialized).unwrap();
        assert_eq!(lhs, rhs, "JSON semantic equality");

        let round_trip = serde_json::from_str::<Message>(&serialized).unwrap();
        assert_eq!(round_trip, expected, "roundtrip");
    }

    #[test]
    fn request() {
        case(
            r#"{"id":1,"method":"mining.subscribe","params":["foo"]}"#,
            Message::Request {
                id: Id::Number(1),
                method: Method::Subscribe(Subscribe {
                    user_agent: "foo".into(),
                    enonce1: None,
                }),
            },
        );
    }

    #[test]
    fn notification() {
        case(
            r#"{"method":"mining.set_difficulty","params":[2]}"#,
            Message::Notification {
                method: Method::SetDifficulty(SetDifficulty(Difficulty::from(2))),
            },
        );

        let with_id_null = r#"{"method":"mining.set_difficulty","params":[2],"id":null}"#;

        assert_eq!(
            serde_json::from_str::<Message>(with_id_null).unwrap(),
            Message::Notification {
                method: Method::SetDifficulty(SetDifficulty(Difficulty::from(2))),
            }
        );
    }

    #[test]
    fn response() {
        case(
            r#"{"id":8,"result":[[["mining.set_difficulty","b4b6693b72a50c7116db18d6497cac52"],["mining.notify","ae6812eb4cd7735a302a8a9dd95cf71f"]],"08000002",4],"error":null}"#,
            Message::Response {
                id: Id::Number(8),
                result: Some(serde_json::json!([
                    [
                        ["mining.set_difficulty", "b4b6693b72a50c7116db18d6497cac52"],
                        ["mining.notify", "ae6812eb4cd7735a302a8a9dd95cf71f"]
                    ],
                    "08000002",
                    4
                ])),
                error: None,
                reject_reason: None,
            },
        );
    }

    #[test]
    fn share_rejected_response() {
        assert_eq!(
            serde_json::from_str::<Message>(
                r#"{"reject-reason":"Above target","result":false,"error":null,"id":5}"#
            )
            .unwrap(),
            Message::Response {
                id: Id::Number(5),
                result: Some(serde_json::json!(false)),
                error: None,
                reject_reason: Some("Above target".into()),
            },
        );
    }

    #[test]
    fn error_response() {
        case(
            r#"{"id":10,"result":null,"error":null}"#,
            Message::Response {
                reject_reason: None,
                id: Id::Number(10),
                result: None,
                error: None,
            },
        );

        case(
            r#"{"id":10,"result":null,"error":[2,"Stale",null]}"#,
            Message::Response {
                id: Id::Number(10),
                result: None,
                reject_reason: None,
                error: Some(StratumError::Stale.into_response(None)),
            },
        );
    }

    #[test]
    fn notify() {
        let notify = Notify {
            job_id: "bf".parse().unwrap(),
            prevhash: "4d16b6f85af6e2198f44ae2a6de67f78487ae5611b77c6c0440b921e00000000".parse().unwrap(),
            coinb1: "01000000010000000000000000000000000000000000000000000000000000000000000000ffffffff20020862062f503253482f04b8864e5008".into(),
            coinb2: "072f736c7573682f000000000100f2052a010000001976a914d23fcdf86f7e756a64a7a9688ef9903327048ed988ac00000000".into(),
            merkle_branches: Vec::new(),
            version: Version(block::Version::TWO),
            nbits: "1c2ac4af".parse().unwrap(),
            ntime: "504e86b9".parse().unwrap(),
            clean_jobs: false,
        };

        case(
            r#"{"method":"mining.notify","params":["bf","4d16b6f85af6e2198f44ae2a6de67f78487ae5611b77c6c0440b921e00000000","01000000010000000000000000000000000000000000000000000000000000000000000000ffffffff20020862062f503253482f04b8864e5008","072f736c7573682f000000000100f2052a010000001976a914d23fcdf86f7e756a64a7a9688ef9903327048ed988ac00000000",[],"00000002","1c2ac4af","504e86b9",false]}"#,
            Message::Notification {
                method: Method::Notify(notify.clone()),
            },
        );

        let notify_string = r#"{"params": ["bf", "4d16b6f85af6e2198f44ae2a6de67f78487ae5611b77c6c0440b921e00000000",
"01000000010000000000000000000000000000000000000000000000000000000000000000ffffffff20020862062f503253482f04b8864e5008",
"072f736c7573682f000000000100f2052a010000001976a914d23fcdf86f7e756a64a7a9688ef9903327048ed988ac00000000", [],
"00000002", "1c2ac4af", "504e86b9", false], "id": null, "method": "mining.notify"}"#;

        assert_eq!(
            serde_json::from_str::<Message>(notify_string).unwrap(),
            Message::Notification {
                method: Method::Notify(notify),
            },
        );
    }

    #[test]
    fn submit() {
        case(
            r#"{"id":4,"method":"mining.submit","params":["slush.miner1","bf","00000001","504e86ed","b2957c02"]}"#,
            Message::Request {
                id: Id::Number(4),
                method: Method::Submit(Submit {
                    username: "slush.miner1".into(),
                    job_id: "bf".parse().unwrap(),
                    enonce2: "00000001".parse().unwrap(),
                    ntime: "504e86ed".parse().unwrap(),
                    nonce: "b2957c02".parse().unwrap(),
                    version_bits: None,
                }),
            },
        );

        case(
            r#"{"id":4,"result":true,"error":null}"#,
            Message::Response {
                reject_reason: None,
                id: Id::Number(4),
                result: Some(serde_json::json!(true)),
                error: None,
            },
        );
    }

    #[test]
    fn submit_with_version_bits() {
        case(
            r#"{"id":4,"method":"mining.submit","params":["slush.miner1","bf","00000001","504e86ed","b2957c02","04d46000"]}"#,
            Message::Request {
                id: Id::Number(4),
                method: Method::Submit(Submit {
                    username: "slush.miner1".into(),
                    job_id: "bf".parse().unwrap(),
                    enonce2: "00000001".parse().unwrap(),
                    ntime: "504e86ed".parse().unwrap(),
                    nonce: "b2957c02".parse().unwrap(),
                    version_bits: Some("04d46000".parse().unwrap()),
                }),
            },
        );

        case(
            r#"{"id":4,"result":true,"error":null}"#,
            Message::Response {
                reject_reason: None,
                id: Id::Number(4),
                result: Some(serde_json::json!(true)),
                error: None,
            },
        );
    }

    #[test]
    fn set_difficulty() {
        let set_difficulty_str = r#"{"id":null,"method":"mining.set_difficulty","params":[2]}"#;

        assert_eq!(
            serde_json::from_str::<Message>(set_difficulty_str).unwrap(),
            Message::Notification {
                method: Method::SetDifficulty(SetDifficulty(Difficulty::from(2))),
            },
        );
    }

    #[test]
    fn authorize() {
        case(
            r#"{"id":2,"method":"mining.authorize","params":["slush.miner1","password"]}"#,
            Message::Request {
                id: Id::Number(2),
                method: Method::Authorize(Authorize {
                    username: "slush.miner1".into(),
                    password: Some("password".into()),
                }),
            },
        );

        case(
            r#"{"id":2,"result":true,"error":null}"#,
            Message::Response {
                id: Id::Number(2),
                result: Some(serde_json::json!(true)),
                error: None,
                reject_reason: None,
            },
        );
    }

    #[test]
    fn authorize_optional_password() {
        case(
            r#"{"id":2,"method":"mining.authorize","params":["slush.miner1"]}"#,
            Message::Request {
                id: Id::Number(2),
                method: Method::Authorize(Authorize {
                    username: "slush.miner1".into(),
                    password: None,
                }),
            },
        );
    }

    #[test]
    fn subscribe() {
        case(
            r#"{"id":1,"method":"mining.subscribe","params":["para/0.5.2"]}"#,
            Message::Request {
                id: Id::Number(1),
                method: Method::Subscribe(Subscribe {
                    user_agent: "para/0.5.2".into(),
                    enonce1: None,
                }),
            },
        );

        case(
            r#"{"id":2,"method":"mining.subscribe","params":["para/0.1","abcd"]}"#,
            Message::Request {
                id: Id::Number(2),
                method: Method::Subscribe(Subscribe {
                    user_agent: "para/0.1".into(),
                    enonce1: Some("abcd".parse().unwrap()),
                }),
            },
        );
    }

    #[test]
    fn subscribe_result() {
        case(
            r#"{"id":1,"result":[[["mining.set_difficulty","b4b6693b72a50c7116db18d6497cac52"],["mining.notify","ae6812eb4cd7735a302a8a9dd95cf71f"]],"08000002",4],"error":null}"#,
            Message::Response {
                id: Id::Number(1),
                result: Some(
                    serde_json::to_value(SubscribeResponse {
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
                    })
                    .unwrap(),
                ),
                error: None,
                reject_reason: None,
            },
        );
    }

    #[test]
    fn configure_minimal() {
        case(
            r#"{"id":3,"method":"mining.configure","params":[["version-rolling"],{"version-rolling.mask":"ffffffff"}]}"#,
            Message::Request {
                id: Id::Number(3),
                method: Method::Configure(Configure {
                    extensions: vec!["version-rolling".into()],
                    minimum_difficulty_value: None,
                    version_rolling_mask: Some("ffffffff".parse().unwrap()),
                    version_rolling_min_bit_count: None,
                }),
            },
        );
    }

    #[test]
    fn extra_fields_ignored() {
        assert_eq!(
            serde_json::from_str::<Message>(
                r#"{"id":1,"method":"mining.subscribe","params":["foo"],"extra":"bar"}"#
            )
            .unwrap(),
            Message::Request {
                id: Id::Number(1),
                method: Method::Subscribe(Subscribe {
                    user_agent: "foo".into(),
                    enonce1: None,
                }),
            },
        );
    }

    #[test]
    fn unknown_message_format() {
        assert!(serde_json::from_str::<Message>(r#"{"foo":"bar"}"#).is_err());
    }

    #[test]
    fn request_missing_params() {
        assert!(
            serde_json::from_str::<Message>(r#"{"id":1,"method":"mining.subscribe"}"#).is_err()
        );
    }

    #[test]
    fn request_missing_id() {
        assert!(
            serde_json::from_str::<Message>(r#"{"method":"mining.subscribe","params":["foo"]}"#)
                .is_ok(),
            "missing id is classified as notification"
        );
    }

    #[test]
    fn request_with_string_id() {
        case(
            r#"{"id":"abc","method":"mining.subscribe","params":["foo"]}"#,
            Message::Request {
                id: Id::String("abc".into()),
                method: Method::Subscribe(Subscribe {
                    user_agent: "foo".into(),
                    enonce1: None,
                }),
            },
        );
    }

    #[test]
    fn configure_with_options() {
        case(
            r#"{"id":5,"method":"mining.configure","params":[["minimum-difficulty","version-rolling"],{"minimum-difficulty.value":2048,"version-rolling.mask":"00fff000","version-rolling.min-bit-count":2}]}"#,
            Message::Request {
                id: Id::Number(5),
                method: Method::Configure(Configure {
                    extensions: vec!["minimum-difficulty".into(), "version-rolling".into()],
                    minimum_difficulty_value: Some(Difficulty::from(2048)),
                    version_rolling_mask: Some("00fff000".parse().unwrap()),
                    version_rolling_min_bit_count: Some(2),
                }),
            },
        );
    }
}
