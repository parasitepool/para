use {super::*, std::fmt};

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

    #[snafu(display("Channel send error"))]
    ChannelSend,

    #[snafu(display("Serialization error: {source}"))]
    Serialization { source: serde_json::Error },

    #[snafu(display("{message}"))]
    Protocol { message: String },

    #[snafu(display("Disconnected: {reason}"))]
    Disconnected { reason: DisconnectReason },
}

#[derive(Debug, Clone)]
pub enum DisconnectReason {
    ServerClosed,
    ReadError(String),
    UserRequested,
}

impl fmt::Display for DisconnectReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ServerClosed => write!(f, "server closed connection"),
            Self::ReadError(e) => write!(f, "read error: {}", e),
            Self::UserRequested => write!(f, "user requested disconnect"),
        }
    }
}
