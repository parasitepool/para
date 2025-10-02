use super::*;

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord, Display, Clone)]
#[serde(untagged)]
pub enum Id {
    Null,
    Number(u64),
    String(String),
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(untagged)]
pub enum Message {
    Request {
        id: Id,
        method: String,
        params: Value,
    },
    Response {
        id: Id,
        result: Option<Value>,
        error: Option<JsonRpcError>,
        #[serde(skip_serializing_if = "Option::is_none", rename = "reject-reason")]
        reject_reason: Option<String>,
    },
    Notification {
        method: String,
        params: Value,
    },
}

/// Stratum does id: null, which is technically wrong according to the JSON-RPC spec, which
/// states that no id field should be present. This is a work around to allow both cases. If
/// a server sends a notification with an id field other than null it will be classified as
/// a request and should just be ignored by any client.
impl<'de> Deserialize<'de> for Message {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;

        let is_request = value.get("method").is_some() && value.get("id").is_some();

        let is_notification_optional_null_id = value.get("method").is_some()
            && (value.get("id") == Some(&Value::Null) || value.get("id").is_none());

        let is_response = value.get("result").is_some()
            || value.get("error").is_some()
            || value.get("reject-reason").is_some();

        if is_response {
            #[derive(Deserialize)]
            struct Resp {
                id: Id,
                result: Option<Value>,
                error: Option<JsonRpcError>,
                #[serde(rename = "reject-reason")]
                reject_reason: Option<String>,
            }

            let r: Resp = serde_json::from_value(value).map_err(de::Error::custom)?;

            Ok(Message::Response {
                id: r.id,
                result: r.result,
                error: r.error,
                reject_reason: r.reject_reason,
            })
        } else if is_notification_optional_null_id {
            let method = value
                .get("method")
                .and_then(Value::as_str)
                .ok_or_else(|| de::Error::missing_field("method"))?
                .to_string();

            let params = value
                .get("params")
                .cloned()
                .ok_or_else(|| de::Error::missing_field("params"))?;

            Ok(Message::Notification { method, params })
        } else if is_request {
            #[derive(Deserialize)]
            struct Req {
                id: Id,
                method: String,
                params: Value,
            }

            let r: Req = serde_json::from_value(value).map_err(de::Error::custom)?;

            Ok(Message::Request {
                id: r.id,
                method: r.method,
                params: r.params,
            })
        } else {
            Err(de::Error::custom("unknown message format"))
        }
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
            r#"{"id":1,"method":"mining.subscribe","params":[]}"#,
            Message::Request {
                id: Id::Number(1),
                method: "mining.subscribe".into(),
                params: json!([]),
            },
        );
    }

    #[test]
    fn notification() {
        case(
            r#"{"method":"mining.notify","params":[]}"#,
            Message::Notification {
                method: "mining.notify".into(),
                params: json!([]),
            },
        );

        let with_id_null = r#"{"method":"mining.notify","params":[],"id":null}"#;

        assert_eq!(
            serde_json::from_str::<Message>(with_id_null).unwrap(),
            Message::Notification {
                method: "mining.notify".into(),
                params: json!([]),
            }
        );
    }

    #[test]
    fn response() {
        case(
            r#"{"id":8,"result":[[["mining.set_difficulty","b4b6693b72a50c7116db18d6497cac52"],["mining.notify","ae6812eb4cd7735a302a8a9dd95cf71f"]],"08000002",4],"error":null}"#,
            Message::Response {
                id: Id::Number(8),
                result: Some(json!([
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
                result: Some(json!(false)),
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
            r#"{"id":10,"result":null,"error":[21,"Job not found",null]}"#,
            Message::Response {
                id: Id::Number(10),
                result: None,
                reject_reason: None,
                error: Some(JsonRpcError {
                    error_code: 21,
                    message: "Job not found".into(),
                    traceback: None,
                }),
            },
        );
    }

    #[test]
    fn notify() {
        let notify = Notify {
            job_id: "bf".into(),
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
                method: "mining.notify".into(),
                params: serde_json::to_value(&notify).unwrap(),
            },
        );

        let notify_string = r#"{"params": ["bf", "4d16b6f85af6e2198f44ae2a6de67f78487ae5611b77c6c0440b921e00000000",
"01000000010000000000000000000000000000000000000000000000000000000000000000ffffffff20020862062f503253482f04b8864e5008",
"072f736c7573682f000000000100f2052a010000001976a914d23fcdf86f7e756a64a7a9688ef9903327048ed988ac00000000", [],
"00000002", "1c2ac4af", "504e86b9", false], "id": null, "method": "mining.notify"}"#;

        assert_eq!(
            serde_json::from_str::<Message>(notify_string).unwrap(),
            Message::Notification {
                method: "mining.notify".into(),
                params: serde_json::to_value(notify).unwrap(),
            },
        );
    }

    #[test]
    fn submit() {
        case(
            r#"{"id":4,"method":"mining.submit","params":["slush.miner1","bf","00000001","504e86ed","b2957c02"]}"#,
            Message::Request {
                id: Id::Number(4),
                method: "mining.submit".into(),
                params: serde_json::to_value(&Submit {
                    username: "slush.miner1".into(),
                    job_id: "bf".into(),
                    extranonce2: "00000001".parse().unwrap(),
                    ntime: "504e86ed".parse().unwrap(),
                    nonce: "b2957c02".parse().unwrap(),
                    version_bits: None,
                })
                .unwrap(),
            },
        );

        case(
            r#"{"id":4,"result":true,"error":null}"#,
            Message::Response {
                reject_reason: None,
                id: Id::Number(4),
                result: Some(json!(true)),
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
                method: "mining.submit".into(),
                params: serde_json::to_value(&Submit {
                    username: "slush.miner1".into(),
                    job_id: "bf".into(),
                    extranonce2: "00000001".parse().unwrap(),
                    ntime: "504e86ed".parse().unwrap(),
                    nonce: "b2957c02".parse().unwrap(),
                    version_bits: Some("04d46000".parse().unwrap()),
                })
                .unwrap(),
            },
        );

        case(
            r#"{"id":4,"result":true,"error":null}"#,
            Message::Response {
                reject_reason: None,
                id: Id::Number(4),
                result: Some(json!(true)),
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
                method: "mining.set_difficulty".into(),
                params: serde_json::to_value(SetDifficulty(Difficulty(2))).unwrap(),
            },
        );
    }

    #[test]
    fn authorize() {
        case(
            r#"{"id":2,"method":"mining.authorize","params":["slush.miner1","password"]}"#,
            Message::Request {
                id: Id::Number(2),
                method: "mining.authorize".into(),
                params: serde_json::to_value(Authorize {
                    username: "slush.miner1".into(),
                    password: Some("password".into()),
                })
                .unwrap(),
            },
        );

        case(
            r#"{"id":2,"result":true,"error":null}"#,
            Message::Response {
                id: Id::Number(2),
                result: Some(json!(true)),
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
                method: "mining.authorize".into(),
                params: serde_json::to_value(Authorize {
                    username: "slush.miner1".into(),
                    password: None,
                })
                .unwrap(),
            },
        );
    }

    #[test]
    fn subscribe() {
        case(
            r#"{"id":1,"method":"mining.subscribe","params":["para/0.5.2"]}"#,
            Message::Request {
                id: Id::Number(1),
                method: "mining.subscribe".into(),
                params: serde_json::to_value(Subscribe {
                    user_agent: "para/0.5.2".into(),
                    extranonce1: None,
                })
                .unwrap(),
            },
        );

        case(
            r#"{"id":2,"method":"mining.subscribe","params":["para/0.1","abcd"]}"#,
            Message::Request {
                id: Id::Number(2),
                method: "mining.subscribe".into(),
                params: serde_json::to_value(Subscribe {
                    user_agent: "para/0.1".into(),
                    extranonce1: Some("abcd".parse().unwrap()),
                })
                .unwrap(),
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
                    serde_json::to_value(SubscribeResult {
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
                        extranonce1: "08000002".parse().unwrap(),
                        extranonce2_size: 4,
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
                method: "mining.configure".into(),
                params: serde_json::to_value(Configure {
                    extensions: vec!["version-rolling".into()],
                    minimum_difficulty_value: None,
                    version_rolling_mask: Some("ffffffff".parse().unwrap()),
                    version_rolling_min_bit_count: None,
                })
                .unwrap(),
            },
        );
    }

    #[test]
    fn configure_with_options() {
        case(
            r#"{"id":5,"method":"mining.configure","params":[["minimum-difficulty","version-rolling"],{"minimum-difficulty.value":2048,"version-rolling.mask":"00fff000","version-rolling.min-bit-count":2}]}"#,
            Message::Request {
                id: Id::Number(5),
                method: "mining.configure".into(),
                params: serde_json::to_value(Configure {
                    extensions: vec!["minimum-difficulty".into(), "version-rolling".into()],
                    minimum_difficulty_value: Some(Difficulty(2048)),
                    version_rolling_mask: Some("00fff000".parse().unwrap()),
                    version_rolling_min_bit_count: Some(2),
                })
                .unwrap(),
            },
        );
    }
}
