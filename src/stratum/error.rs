use super::*;

#[derive(Debug, Deserialize, PartialEq)]
pub struct JsonRpcError {
    pub error_code: i32,
    pub message: String,
    pub traceback: Option<Value>,
}

impl Serialize for JsonRpcError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        (&self.error_code, &self.message, &self.traceback).serialize(serializer)
    }
}

impl fmt::Display for JsonRpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.traceback {
            Some(traceback) => write!(
                f,
                "Stratum error {}: {} (traceback: {})",
                self.error_code,
                self.message,
                serde_json::to_string(traceback).unwrap_or_else(|_| "<invalid traceback>".into())
            ),
            None => write!(f, "Stratum error {}: {}", self.error_code, self.message),
        }
    }
}
