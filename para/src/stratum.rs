use super::*;

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum Message {
    Request {
        id: u64,
        method: String,
        params: Value,
    },
    Response {
        id: u64,
        result: Option<Value>,
        error: Option<JsonRpcError>,
    },
    Notification {
        method: String,
        params: Value,
    },
}

// Stratum does id: null, which is technically wrong according to the JSON-RPC spec, which
// states that no id field should be present. This is a work around to allow both cases. If
// a server sends a notification with an id field other than null it will be classified as
// a request and should just be ignored by any client.
impl<'de> Deserialize<'de> for Message {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;

        let is_request = value.get("method").is_some() && value.get("id").is_some();

        let is_notification_null_id =
            value.get("method").is_some() && value.get("id") == Some(&Value::Null);

        let is_response = value.get("result").is_some() || value.get("error").is_some();

        if is_response {
            #[derive(Deserialize)]
            struct Resp {
                id: u64,
                result: Option<Value>,
                error: Option<JsonRpcError>,
            }

            let r: Resp = serde_json::from_value(value).map_err(de::Error::custom)?;

            Ok(Message::Response {
                id: r.id,
                result: r.result,
                error: r.error,
            })
        } else if is_notification_null_id {
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
                id: u64,
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

#[derive(Debug, Deserialize, Serialize)]
pub struct JsonRpcError(
    pub i32,           // error code
    pub String,        // human-readable message
    pub Option<Value>, // optional traceback or debugging info
);

impl Display for JsonRpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.2 {
            Some(traceback) => write!(
                f,
                "Stratum error {}: {} (traceback: {})",
                self.0,
                self.1,
                serde_json::to_string(traceback).unwrap_or_else(|_| "<invalid traceback>".into())
            ),
            None => write!(f, "Stratum error {}: {}", self.0, self.1),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SubscribeResult(
    pub Vec<(String, String)>, // subscriptions
    pub String,                // extranonce1
    pub u32,                   // extranonce2_size
);

impl Display for SubscribeResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let subs: Vec<String> = self
            .0
            .iter()
            .map(|(method, id)| format!("(\"{method}\", \"{id}\")"))
            .collect();

        write!(
            f,
            "subscriptions=[{}], extranonce1={}, extranonce2_size={}",
            subs.join(", "),
            self.1,
            self.2
        )
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Notify {
    pub job_id: String,
    pub prevhash: String,
    pub coinb1: String,
    pub coinb2: String,
    pub merkle_branch: Vec<String>,
    pub version: String,
    pub nbits: String,
    pub ntime: String,
    pub clean_jobs: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SetDifficulty(pub Vec<u64>);
