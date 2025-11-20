use super::*;
use serde_json::json;

pub type Result<T, E = InternalError> = std::result::Result<T, E>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum StratumError {
    InvalidNonce2Length = -9,
    WorkerMismatch = -8,
    NoNonce = -7,
    NoNtime = -6,
    NoNonce2 = -5,
    NoJobId = -4,
    NoUsername = -3,
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
            Self::InvalidNonce2Length => "Invalid nonce2 length",
            Self::WorkerMismatch => "Worker mismatch",
            Self::NoNonce => "No nonce",
            Self::NoNtime => "No ntime",
            Self::NoNonce2 => "No nonce2",
            Self::NoJobId => "No job_id",
            Self::NoUsername => "No username",
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
    /// Create a response with optional internal error context
    pub fn with_context(self, context: impl std::fmt::Display) -> StratumErrorResponse {
        StratumErrorResponse {
            error: self,
            context: Some(context.to_string()),
        }
    }
}

impl From<StratumError> for StratumErrorResponse {
    fn from(error: StratumError) -> Self {
        StratumErrorResponse {
            error,
            context: None,
        }
    }
}

/// Stratum error response sent to clients
/// Serializes as [code, message, traceback] for Stratum V1 compatibility
#[derive(Debug)]
pub struct StratumErrorResponse {
    pub error: StratumError,
    pub context: Option<String>, // Store error context as string instead of InternalError
}

impl Serialize for StratumErrorResponse {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let traceback = self.context.as_ref().map(|ctx| {
            json!({
                "error": ctx,
            })
        });

        (self.error as i32, self.error.to_string(), traceback).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for StratumErrorResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct ErrorArray(i32, String, Option<Value>);

        let ErrorArray(code, _message, _traceback) = ErrorArray::deserialize(deserializer)?;

        // Map code to StratumError enum
        let error = match code {
            -9 => StratumError::InvalidNonce2Length,
            -8 => StratumError::WorkerMismatch,
            -7 => StratumError::NoNonce,
            -6 => StratumError::NoNtime,
            -5 => StratumError::NoNonce2,
            -4 => StratumError::NoJobId,
            -3 => StratumError::NoUsername,
            -2 => StratumError::InvalidArraySize,
            -1 => StratumError::ParamsNotArray,
            1 => StratumError::InvalidJobId,
            2 => StratumError::Stale,
            3 => StratumError::NtimeOutOfRange,
            4 => StratumError::Duplicate,
            5 => StratumError::AboveTarget,
            6 => StratumError::InvalidVersionMask,
            _ => {
                return Err(de::Error::custom(format!(
                    "Unknown stratum error code: {}",
                    code
                )));
            }
        };

        // Note: We lose the context/traceback on deserialization since we can't
        // reconstruct InternalError from arbitrary JSON. This is fine for client usage.
        Ok(StratumErrorResponse {
            error,
            context: None,
        })
    }
}

impl PartialEq for StratumErrorResponse {
    fn eq(&self, other: &Self) -> bool {
        // Only compare the error code, not the context
        self.error == other.error
    }
}

impl fmt::Display for StratumErrorResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.error as i32, self.error)?;
        if let Some(ctx) = &self.context {
            write!(f, " ({})", ctx)?;
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

    #[snafu(display("{message}"))]
    Protocol { message: String },

    #[snafu(display("Connection timeout: {source}"))]
    Timeout { source: tokio::time::error::Elapsed },

    #[snafu(display("IO error: {source}"))]
    Io { source: std::io::Error },

    #[snafu(display("Channel receive error: {source}"))]
    ChannelRecv {
        source: tokio::sync::oneshot::error::RecvError,
    },
}

// Implement From for common errors to enable ? operator
impl From<serde_json::Error> for InternalError {
    fn from(source: serde_json::Error) -> Self {
        InternalError::Serialization {
            source: Box::new(source),
        }
    }
}

impl From<std::io::Error> for InternalError {
    fn from(source: std::io::Error) -> Self {
        InternalError::Io { source }
    }
}

impl From<tokio::sync::oneshot::error::RecvError> for InternalError {
    fn from(source: tokio::sync::oneshot::error::RecvError) -> Self {
        InternalError::ChannelRecv { source }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stratum_error_code_display() {
        assert_eq!(
            StratumError::InvalidNonce2Length.to_string(),
            "Invalid nonce2 length"
        );
        assert_eq!(StratumError::WorkerMismatch.to_string(), "Worker mismatch");
        assert_eq!(StratumError::NoNonce.to_string(), "No nonce");
        assert_eq!(StratumError::NoNtime.to_string(), "No ntime");
        assert_eq!(StratumError::NoNonce2.to_string(), "No nonce2");
        assert_eq!(StratumError::NoJobId.to_string(), "No job_id");
        assert_eq!(StratumError::NoUsername.to_string(), "No username");
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
    fn stratum_error_code_values() {
        assert_eq!(StratumError::InvalidNonce2Length as i32, -9);
        assert_eq!(StratumError::WorkerMismatch as i32, -8);
        assert_eq!(StratumError::NoNonce as i32, -7);
        assert_eq!(StratumError::NoNtime as i32, -6);
        assert_eq!(StratumError::NoNonce2 as i32, -5);
        assert_eq!(StratumError::NoJobId as i32, -4);
        assert_eq!(StratumError::NoUsername as i32, -3);
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
        let response: StratumErrorResponse = error.into();

        assert_eq!(response.error, StratumError::Stale);
        assert_eq!(response.context, None);
    }

    #[test]
    fn stratum_error_response_serialization_as_array() {
        let response = StratumErrorResponse {
            error: StratumError::Stale,
            context: None,
        };

        let serialized = serde_json::to_string(&response).unwrap();
        assert_eq!(serialized, "[2,\"Stale\",null]");

        let with_context = StratumErrorResponse {
            error: StratumError::InvalidJobId,
            context: Some("job_id: deadbeef".to_string()),
        };

        let serialized = serde_json::to_string(&with_context).unwrap();
        assert!(serialized.contains("[1,\"Invalid JobID\","));
        assert!(serialized.contains("job_id: deadbeef"));
    }

    #[test]
    fn stratum_error_response_deserialization_from_array() {
        // Test basic deserialization
        let json = "[2,\"Stale\",null]";
        let response: StratumErrorResponse = serde_json::from_str(json).unwrap();

        assert_eq!(response.error, StratumError::Stale);
        assert_eq!(response.context, None);
    }

    #[test]
    fn stratum_error_response_with_context() {
        let error = StratumError::InvalidJobId;
        let context = InternalError::InvalidValue {
            reason: "received: abc123, expected: deadbeef".to_string(),
        };
        let response = error.with_context(context);

        assert_eq!(response.error, StratumError::InvalidJobId);
        assert!(response.context.is_some());
    }

    #[test]
    fn stratum_error_response_display() {
        let response = StratumErrorResponse {
            error: StratumError::Stale,
            context: None,
        };

        assert_eq!(response.to_string(), "2: Stale");

        let with_context = StratumErrorResponse {
            error: StratumError::InvalidJobId,
            context: Some("additional details".to_string()),
        };

        assert_eq!(with_context.to_string(), "1: Invalid JobID (additional details)");
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
