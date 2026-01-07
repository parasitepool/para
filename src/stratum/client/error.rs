use super::*;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub))]
pub enum ClientError {
    #[snafu(display("Connection timeout: {source}"))]
    Timeout { source: tokio::time::error::Elapsed },

    #[snafu(display("IO error: {source}"))]
    Io { source: std::io::Error },

    #[snafu(display("Channel receive error: {source}"))]
    ChannelRecv {
        source: tokio::sync::oneshot::error::RecvError,
    },

    #[snafu(display("Serialization error: {source}"))]
    Serialization { source: serde_json::Error },

    #[snafu(display("{response}"))]
    Stratum { response: StratumErrorResponse },

    #[snafu(display("Client not connected"))]
    NotConnected,

    #[snafu(display("{method} rejected: {reason}"))]
    Rejected { method: String, reason: String },

    #[snafu(display("Unhandled response for {method}"))]
    UnhandledResponse { method: String },

    #[snafu(display("Server returned false for submit"))]
    SubmitFalse,
}
