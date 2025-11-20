use super::*;
use snafu::Snafu;

/// JSON-RPC error response for Stratum V1 protocol.
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

/// Stratum protocol error codes matching ckpool's implementation.
/// These are sent to miners as part of share validation responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum StratumErrorCode {
    /// Invalid nonce2 length
    InvalidNonce2Length = -9,
    /// Worker name mismatch
    WorkerMismatch = -8,
    /// Missing or invalid nonce
    NoNonce = -7,
    /// Missing or invalid ntime
    NoNtime = -6,
    /// Missing or invalid nonce2
    NoNonce2 = -5,
    /// Missing or invalid job_id
    NoJobId = -4,
    /// Missing or invalid username
    NoUsername = -3,
    /// Invalid array size in params
    InvalidArraySize = -2,
    /// Params is not an array
    ParamsNotArray = -1,
    /// Valid share (no error)
    Valid = 0,
    /// Job ID not found or invalid
    InvalidJobId = 1,
    /// Share is stale (job expired)
    Stale = 2,
    /// ntime value out of acceptable range
    NtimeOutOfRange = 3,
    /// Duplicate share submission
    Duplicate = 4,
    /// Share difficulty above target
    AboveTarget = 5,
    /// Invalid version mask bits
    InvalidVersionMask = 6,
}

impl StratumErrorCode {
    /// Returns the human-readable error message for this error code.
    pub fn message(self) -> &'static str {
        match self {
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
        }
    }

    /// Converts this error code to a JSON-RPC error for wire transmission.
    pub fn to_json_rpc_error(self) -> JsonRpcError {
        JsonRpcError {
            error_code: self as i32,
            message: self.message().to_string(),
            traceback: None,
        }
    }

    /// Creates a JSON-RPC error with additional traceback data.
    pub fn to_json_rpc_error_with_traceback(self, traceback: Value) -> JsonRpcError {
        JsonRpcError {
            error_code: self as i32,
            message: self.message().to_string(),
            traceback: Some(traceback),
        }
    }
}

impl From<StratumErrorCode> for JsonRpcError {
    fn from(code: StratumErrorCode) -> Self {
        code.to_json_rpc_error()
    }
}

/// Internal errors for the stratum module using snafu.
/// These represent errors that occur during processing, not protocol errors.
#[derive(Debug, Snafu)]
pub enum InternalError {
    #[snafu(display("Failed to serialize JSON: {}", source))]
    Serialization { source: serde_json::Error },

    #[snafu(display("Failed to deserialize JSON: {}", source))]
    Deserialization { source: serde_json::Error },

    #[snafu(display("Failed to parse hex string: {}", source))]
    HexParse { source: hex::FromHexError },

    #[snafu(display("Invalid hex string: {}", reason))]
    InvalidHex { reason: String },

    #[snafu(display("Invalid length: expected {}, got {}", expected, actual))]
    InvalidLength { expected: usize, actual: usize },

    #[snafu(display("Invalid value: {}", reason))]
    InvalidValue { reason: String },

    #[snafu(display("Invalid merkle tree structure"))]
    InvalidMerkle,

    #[snafu(display("Merkle computation failed: {}", reason))]
    MerkleComputation { reason: String },

    #[snafu(display("Invalid version bits"))]
    InvalidVersionBits,

    #[snafu(display("Invalid target/difficulty"))]
    InvalidTarget,

    #[snafu(display("Parse error: {}", message))]
    Parse { message: String },
}

// Helper type alias for Results in the stratum module
pub type Result<T, E = InternalError> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stratum_error_code_messages() {
        assert_eq!(
            StratumErrorCode::InvalidNonce2Length.message(),
            "Invalid nonce2 length"
        );
        assert_eq!(
            StratumErrorCode::WorkerMismatch.message(),
            "Worker mismatch"
        );
        assert_eq!(StratumErrorCode::NoNonce.message(), "No nonce");
        assert_eq!(StratumErrorCode::NoNtime.message(), "No ntime");
        assert_eq!(StratumErrorCode::NoNonce2.message(), "No nonce2");
        assert_eq!(StratumErrorCode::NoJobId.message(), "No job_id");
        assert_eq!(StratumErrorCode::NoUsername.message(), "No username");
        assert_eq!(
            StratumErrorCode::InvalidArraySize.message(),
            "Invalid array size"
        );
        assert_eq!(
            StratumErrorCode::ParamsNotArray.message(),
            "Params not array"
        );
        assert_eq!(StratumErrorCode::Valid.message(), "Valid");
        assert_eq!(StratumErrorCode::InvalidJobId.message(), "Invalid JobID");
        assert_eq!(StratumErrorCode::Stale.message(), "Stale");
        assert_eq!(
            StratumErrorCode::NtimeOutOfRange.message(),
            "Ntime out of range"
        );
        assert_eq!(StratumErrorCode::Duplicate.message(), "Duplicate");
        assert_eq!(StratumErrorCode::AboveTarget.message(), "Above target");
        assert_eq!(
            StratumErrorCode::InvalidVersionMask.message(),
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
