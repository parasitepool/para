use super::*;

pub type Result<T, E = InternalError> = std::result::Result<T, E>;

/// JSON-RPC error response for Stratum V1 protocol (not compliant with JSON-RPC spec).
/// Serializes as an array [code, message, traceback] for compatibility.
#[derive(Debug, Deserialize, PartialEq, Clone)]
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

/// Stratum protocol error codes matching ckpool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum StratumErrorCode {
    InvalidNonce2Length = -9,
    WorkerMismatch = -8,
    NoNonce = -7,
    NoNtime = -6,
    NoNonce2 = -5,
    NoJobId = -4,
    NoUsername = -3,
    InvalidArraySize = -2,
    ParamsNotArray = -1,
    Valid = 0,
    InvalidJobId = 1,
    Stale = 2,
    NtimeOutOfRange = 3,
    Duplicate = 4,
    AboveTarget = 5,
    InvalidVersionMask = 6,
}

impl fmt::Display for StratumErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidNonce2Length => "Invalid nonce2 length",
            Self::WorkerMismatch => "Worker mismatch",
            Self::NoNonce => "No nonce",
            Self::NoNtime => "No ntime",
            Self::NoNonce2 => "No nonce2",
            Self::NoJobId => "No job_id",
            Self::NoUsername => "No username",
            Self::InvalidArraySize => "Invalid array size",
            Self::ParamsNotArray => "Params not array",
            Self::Valid => "Valid",
            Self::InvalidJobId => "Invalid JobID",
            Self::Stale => "Stale",
            Self::NtimeOutOfRange => "Ntime out of range",
            Self::Duplicate => "Duplicate",
            Self::AboveTarget => "Above target",
            Self::InvalidVersionMask => "Invalid version mask",
        };
        write!(f, "{}", message)
    }
}

impl StratumErrorCode {
    pub fn to_json_rpc_error(self) -> JsonRpcError {
        JsonRpcError {
            error_code: self as i32,
            message: self.to_string(),
            traceback: None,
        }
    }

    pub fn to_json_rpc_error_with_traceback(self, traceback: Value) -> JsonRpcError {
        JsonRpcError {
            error_code: self as i32,
            message: self.to_string(),
            traceback: Some(traceback),
        }
    }
}

impl From<StratumErrorCode> for JsonRpcError {
    fn from(code: StratumErrorCode) -> Self {
        code.to_json_rpc_error()
    }
}

#[derive(Debug, Snafu)]
#[snafu(visibility(pub))]
pub enum InternalError {
    #[snafu(display("Failed to serialize JSON: {source}"))]
    Serialization { source: serde_json::Error },

    #[snafu(display("Failed to deserialize JSON: {source}"))]
    Deserialization { source: serde_json::Error },

    #[snafu(display("Failed to parse hex string: {source}"))]
    HexParse { source: hex::FromHexError },

    #[snafu(display("Invalid hex string: {reason}"))]
    InvalidHex { reason: String },

    #[snafu(display("Invalid length: expected {expected}, got {actual}"))]
    InvalidLength { expected: usize, actual: usize },

    #[snafu(display("Invalid value: {reason}"))]
    InvalidValue { reason: String },

    #[snafu(display("Invalid merkle tree structure"))]
    InvalidMerkle,

    #[snafu(display("Merkle computation failed: {reason}"))]
    MerkleComputation { reason: String },

    #[snafu(display("Invalid version bits"))]
    InvalidVersionBits,

    #[snafu(display("Invalid target/difficulty"))]
    InvalidTarget,

    #[snafu(display("Parse error: {message}"))]
    Parse { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stratum_error_code_display() {
        assert_eq!(
            StratumErrorCode::InvalidNonce2Length.to_string(),
            "Invalid nonce2 length"
        );
        assert_eq!(
            StratumErrorCode::WorkerMismatch.to_string(),
            "Worker mismatch"
        );
        assert_eq!(StratumErrorCode::NoNonce.to_string(), "No nonce");
        assert_eq!(StratumErrorCode::NoNtime.to_string(), "No ntime");
        assert_eq!(StratumErrorCode::NoNonce2.to_string(), "No nonce2");
        assert_eq!(StratumErrorCode::NoJobId.to_string(), "No job_id");
        assert_eq!(StratumErrorCode::NoUsername.to_string(), "No username");
        assert_eq!(
            StratumErrorCode::InvalidArraySize.to_string(),
            "Invalid array size"
        );
        assert_eq!(
            StratumErrorCode::ParamsNotArray.to_string(),
            "Params not array"
        );
        assert_eq!(StratumErrorCode::Valid.to_string(), "Valid");
        assert_eq!(StratumErrorCode::InvalidJobId.to_string(), "Invalid JobID");
        assert_eq!(StratumErrorCode::Stale.to_string(), "Stale");
        assert_eq!(
            StratumErrorCode::NtimeOutOfRange.to_string(),
            "Ntime out of range"
        );
        assert_eq!(StratumErrorCode::Duplicate.to_string(), "Duplicate");
        assert_eq!(StratumErrorCode::AboveTarget.to_string(), "Above target");
        assert_eq!(
            StratumErrorCode::InvalidVersionMask.to_string(),
            "Invalid version mask"
        );
    }

    #[test]
    fn stratum_error_code_values() {
        assert_eq!(StratumErrorCode::InvalidNonce2Length as i32, -9);
        assert_eq!(StratumErrorCode::WorkerMismatch as i32, -8);
        assert_eq!(StratumErrorCode::NoNonce as i32, -7);
        assert_eq!(StratumErrorCode::NoNtime as i32, -6);
        assert_eq!(StratumErrorCode::NoNonce2 as i32, -5);
        assert_eq!(StratumErrorCode::NoJobId as i32, -4);
        assert_eq!(StratumErrorCode::NoUsername as i32, -3);
        assert_eq!(StratumErrorCode::InvalidArraySize as i32, -2);
        assert_eq!(StratumErrorCode::ParamsNotArray as i32, -1);
        assert_eq!(StratumErrorCode::Valid as i32, 0);
        assert_eq!(StratumErrorCode::InvalidJobId as i32, 1);
        assert_eq!(StratumErrorCode::Stale as i32, 2);
        assert_eq!(StratumErrorCode::NtimeOutOfRange as i32, 3);
        assert_eq!(StratumErrorCode::Duplicate as i32, 4);
        assert_eq!(StratumErrorCode::AboveTarget as i32, 5);
        assert_eq!(StratumErrorCode::InvalidVersionMask as i32, 6);
    }

    #[test]
    fn json_rpc_error_to_from_stratum_error_code() {
        let error_code = StratumErrorCode::Stale;
        let json_rpc_error = error_code.to_json_rpc_error();

        assert_eq!(json_rpc_error.error_code, 2);
        assert_eq!(json_rpc_error.message, "Stale");
        assert_eq!(json_rpc_error.traceback, None);
    }

    #[test]
    fn json_rpc_error_serialization_as_array() {
        let error = JsonRpcError {
            error_code: 2,
            message: "Stale".to_string(),
            traceback: None,
        };

        let serialized = serde_json::to_string(&error).unwrap();
        assert_eq!(serialized, "[2,\"Stale\",null]");

        let with_traceback = JsonRpcError {
            error_code: 1,
            message: "Invalid JobID".to_string(),
            traceback: Some(serde_json::json!({"job_id": "deadbeef"})),
        };

        let serialized = serde_json::to_string(&with_traceback).unwrap();
        assert_eq!(
            serialized,
            "[1,\"Invalid JobID\",{\"job_id\":\"deadbeef\"}]"
        );
    }

    #[test]
    fn json_rpc_error_deserialization_from_array() {
        let json = "[21,\"Job not found\",null]";
        let error: JsonRpcError = serde_json::from_str(json).unwrap();

        assert_eq!(error.error_code, 21);
        assert_eq!(error.message, "Job not found");
        assert_eq!(error.traceback, None);
    }

    #[test]
    fn json_rpc_error_with_traceback() {
        let error_code = StratumErrorCode::InvalidJobId;
        let traceback = serde_json::json!({"received": "abc123", "expected": "deadbeef"});
        let error = error_code.to_json_rpc_error_with_traceback(traceback.clone());

        assert_eq!(error.error_code, 1);
        assert_eq!(error.message, "Invalid JobID");
        assert_eq!(error.traceback, Some(traceback));
    }

    #[test]
    fn json_rpc_error_display() {
        let error = JsonRpcError {
            error_code: 2,
            message: "Stale".to_string(),
            traceback: None,
        };

        assert_eq!(error.to_string(), "Stratum error 2: Stale");

        let with_traceback = JsonRpcError {
            error_code: 1,
            message: "Invalid JobID".to_string(),
            traceback: Some(serde_json::json!({"detail": "info"})),
        };

        let display = with_traceback.to_string();
        assert!(display.contains("Stratum error 1: Invalid JobID"));
        assert!(display.contains("traceback"));
    }

    #[test]
    fn internal_error_display() {
        let err = InternalError::InvalidLength {
            expected: 64,
            actual: 32,
        };
        assert_eq!(err.to_string(), "Invalid length: expected 64, got 32");

        let err = InternalError::InvalidValue {
            reason: "bad value".to_string(),
        };
        assert_eq!(err.to_string(), "Invalid value: bad value");

        let err = InternalError::Parse {
            message: "failed to parse".to_string(),
        };
        assert_eq!(err.to_string(), "Parse error: failed to parse");
    }
}
