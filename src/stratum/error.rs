use super::*;

pub type Result<T, E = InternalError> = std::result::Result<T, E>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum StratumError {
    UnsupportedExtension = -11,
    MethodNotAllowed = -10,
    InvalidNonce2Length = -9,
    WorkerMismatch = -8,
    NoNonce = -7,
    NoNtime = -6,
    NoNonce2 = -5,
    NoJobId = -4,
    Unauthorized = -3,
    InvalidArraySize = -2,
    ParamsNotArray = -1,
    InvalidJobId = 1,
    Stale = 2,
    NtimeOutOfRange = 3,
    Duplicate = 4,
    AboveTarget = 5,
    InvalidVersionMask = 6,
}

impl fmt::Display for StratumError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::UnsupportedExtension => "Unsupported extension",
            Self::MethodNotAllowed => "Method not allowed",
            Self::InvalidNonce2Length => "Invalid nonce2 length",
            Self::WorkerMismatch => "Worker mismatch",
            Self::NoNonce => "No nonce",
            Self::NoNtime => "No ntime",
            Self::NoNonce2 => "No nonce2",
            Self::NoJobId => "No job_id",
            Self::Unauthorized => "Unauthorized",
            Self::InvalidArraySize => "Invalid array size",
            Self::ParamsNotArray => "Params not array",
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

impl StratumError {
    pub fn into_response(self, traceback: Option<Value>) -> StratumErrorResponse {
        StratumErrorResponse {
            error_code: self as i32,
            message: self.to_string(),
            traceback,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StratumErrorResponse {
    pub error_code: i32,
    pub message: String,
    pub traceback: Option<Value>,
}

impl Serialize for StratumErrorResponse {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        (&self.error_code, &self.message, &self.traceback).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for StratumErrorResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct ErrorArray(i32, String, Option<Value>);

        let ErrorArray(error_code, message, traceback) = ErrorArray::deserialize(deserializer)?;

        Ok(StratumErrorResponse {
            error_code,
            message,
            traceback,
        })
    }
}

impl fmt::Display for StratumErrorResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.error_code, self.message)?;
        if let Some(traceback) = &self.traceback {
            write!(
                f,
                " (traceback: {})",
                serde_json::to_string(traceback).unwrap_or_else(|_| "<invalid>".into())
            )?;
        }
        Ok(())
    }
}

#[derive(Debug, Snafu)]
#[snafu(visibility(pub))]
pub enum InternalError {
    #[snafu(display("Failed to serialize JSON: {source}"))]
    Serialization {
        #[snafu(source(from(serde_json::Error, Box::new)))]
        source: Box<serde_json::Error>,
    },

    #[snafu(display("Failed to deserialize JSON: {source}"))]
    Deserialization {
        #[snafu(source(from(serde_json::Error, Box::new)))]
        source: Box<serde_json::Error>,
    },

    #[snafu(display("Failed to parse hex string: {source}"))]
    HexParse { source: hex::FromHexError },

    #[snafu(display("Invalid hex string: {reason}"))]
    InvalidHex { reason: String },

    #[snafu(display("Invalid length: expected {expected}, got {actual}"))]
    InvalidLength { expected: usize, actual: usize },

    #[snafu(display("Invalid value: {reason}"))]
    InvalidValue { reason: String },

    #[snafu(display("Invalid block hash: {source}"))]
    InvalidBlockHash {
        source: bitcoin::hashes::FromSliceError,
    },

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

    #[snafu(display("Invalid hex integer '{input}': {source}"))]
    ParseHexInt {
        input: String,
        source: std::num::ParseIntError,
    },

    #[snafu(display("Invalid nbits hex '{input}': {source}"))]
    ParseNbits {
        input: String,
        source: bitcoin::error::UnprefixedHexError,
    },

    #[snafu(display("Username is empty"))]
    EmptyUsername,

    #[snafu(display("Invalid bitcoin address: {source}"))]
    InvalidAddress {
        source: bitcoin::address::ParseError,
    },

    #[snafu(display("Address {address} is not valid for {expected} network"))]
    NetworkMismatch { expected: Network, address: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stratum_error_code_display() {
        assert_eq!(
            StratumError::UnsupportedExtension.to_string(),
            "Unsupported extension"
        );
        assert_eq!(
            StratumError::MethodNotAllowed.to_string(),
            "Method not allowed"
        );
        assert_eq!(
            StratumError::InvalidNonce2Length.to_string(),
            "Invalid nonce2 length"
        );
        assert_eq!(StratumError::WorkerMismatch.to_string(), "Worker mismatch");
        assert_eq!(StratumError::NoNonce.to_string(), "No nonce");
        assert_eq!(StratumError::NoNtime.to_string(), "No ntime");
        assert_eq!(StratumError::NoNonce2.to_string(), "No nonce2");
        assert_eq!(StratumError::NoJobId.to_string(), "No job_id");
        assert_eq!(StratumError::Unauthorized.to_string(), "Unauthorized");
        assert_eq!(
            StratumError::InvalidArraySize.to_string(),
            "Invalid array size"
        );
        assert_eq!(StratumError::ParamsNotArray.to_string(), "Params not array");
        assert_eq!(StratumError::InvalidJobId.to_string(), "Invalid JobID");
        assert_eq!(StratumError::Stale.to_string(), "Stale");
        assert_eq!(
            StratumError::NtimeOutOfRange.to_string(),
            "Ntime out of range"
        );
        assert_eq!(StratumError::Duplicate.to_string(), "Duplicate");
        assert_eq!(StratumError::AboveTarget.to_string(), "Above target");
        assert_eq!(
            StratumError::InvalidVersionMask.to_string(),
            "Invalid version mask"
        );
    }

    #[test]
    fn method_not_allowed_error() {
        let error = StratumError::MethodNotAllowed;
        let response = error.into_response(Some(serde_json::json!({
            "method": "mining.configure",
            "current_state": "working"
        })));

        assert_eq!(response.error_code, -10);
        assert_eq!(response.message, "Method not allowed");
        assert!(response.traceback.is_some());

        let serialized = serde_json::to_string(&response).unwrap();
        assert!(serialized.contains("[-10,\"Method not allowed\","));
        assert!(serialized.contains("mining.configure"));
    }

    #[test]
    fn unsupported_extension_error() {
        let error = StratumError::UnsupportedExtension;
        let response = error.into_response(Some(serde_json::json!({
            "extensions": ["minimum-difficulty"],
            "reason": "Only version-rolling is supported"
        })));

        assert_eq!(response.error_code, -11);
        assert_eq!(response.message, "Unsupported extension");
        assert!(response.traceback.is_some());

        let serialized = serde_json::to_string(&response).unwrap();
        assert!(serialized.contains("[-11,\"Unsupported extension\","));
        assert!(serialized.contains("minimum-difficulty"));
    }

    #[test]
    fn stratum_error_code_values() {
        assert_eq!(StratumError::UnsupportedExtension as i32, -11);
        assert_eq!(StratumError::MethodNotAllowed as i32, -10);
        assert_eq!(StratumError::InvalidNonce2Length as i32, -9);
        assert_eq!(StratumError::WorkerMismatch as i32, -8);
        assert_eq!(StratumError::NoNonce as i32, -7);
        assert_eq!(StratumError::NoNtime as i32, -6);
        assert_eq!(StratumError::NoNonce2 as i32, -5);
        assert_eq!(StratumError::NoJobId as i32, -4);
        assert_eq!(StratumError::Unauthorized as i32, -3);
        assert_eq!(StratumError::InvalidArraySize as i32, -2);
        assert_eq!(StratumError::ParamsNotArray as i32, -1);
        assert_eq!(StratumError::InvalidJobId as i32, 1);
        assert_eq!(StratumError::Stale as i32, 2);
        assert_eq!(StratumError::NtimeOutOfRange as i32, 3);
        assert_eq!(StratumError::Duplicate as i32, 4);
        assert_eq!(StratumError::AboveTarget as i32, 5);
        assert_eq!(StratumError::InvalidVersionMask as i32, 6);
    }

    #[test]
    fn stratum_error_response_from_error() {
        let error = StratumError::Stale;
        let response = error.into_response(None);

        assert_eq!(response.error_code, 2);
        assert_eq!(response.message, "Stale");
        assert_eq!(response.traceback, None);
    }

    #[test]
    fn stratum_error_response_serialization_as_array() {
        let response = StratumError::Stale.into_response(None);

        let serialized = serde_json::to_string(&response).unwrap();
        assert_eq!(serialized, "[2,\"Stale\",null]");

        let with_traceback = StratumError::InvalidJobId
            .into_response(Some(serde_json::json!({"job_id": "deadbeef"})));

        let serialized = serde_json::to_string(&with_traceback).unwrap();
        assert!(serialized.contains("[1,\"Invalid JobID\","));
        assert!(serialized.contains("job_id"));
        assert!(serialized.contains("deadbeef"));
    }

    #[test]
    fn stratum_error_response_deserialization_from_array() {
        let json = "[2,\"Stale\",null]";
        let response: StratumErrorResponse = serde_json::from_str(json).unwrap();

        assert_eq!(response.error_code, 2);
        assert_eq!(response.message, "Stale");
        assert_eq!(response.traceback, None);
    }

    #[test]
    fn stratum_error_response_with_traceback() {
        let error = StratumError::InvalidJobId;
        let response = error.into_response(Some(serde_json::json!({
            "received": "abc123",
            "expected": "deadbeef"
        })));

        assert_eq!(response.error_code, 1);
        assert_eq!(response.message, "Invalid JobID");
        assert!(response.traceback.is_some());
    }

    #[test]
    fn stratum_error_response_display() {
        let response = StratumError::Stale.into_response(None);

        assert_eq!(response.to_string(), "2: Stale");

        let with_traceback = StratumError::InvalidJobId
            .into_response(Some(serde_json::json!({"detail": "additional details"})));

        let display = with_traceback.to_string();
        assert!(display.contains("1: Invalid JobID"));
        assert!(display.contains("traceback"));
        assert!(display.contains("additional details"));
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
