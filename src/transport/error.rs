use thiserror::Error;

/// Failures visible at July's agent transport boundary.
#[derive(Debug, Error)]
pub enum TransportError {
    #[error("invalid agent transport configuration: {0}")]
    InvalidConfiguration(&'static str),
    #[error("agent provider authentication is required")]
    AuthenticationRequired,
    #[error("agent adapter identity mismatch: expected {expected}, received {actual}")]
    UnexpectedAgentIdentity { expected: String, actual: String },
    #[error("unsupported ACP protocol version {actual}; expected {expected}")]
    UnsupportedProtocol { expected: u16, actual: u16 },
    #[error("agent adapter does not support {0}")]
    UnsupportedCapability(&'static str),
    #[error("agent transport is not connected")]
    NotConnected,
    #[error("agent transport events were already subscribed")]
    AlreadySubscribed,
    #[error("a turn is already active for session {0}")]
    TurnAlreadyActive(String),
    #[error("remote session {0} was not found")]
    SessionLost(String),
    #[error("session reference does not match the admitted binding for remote session {0}")]
    SessionReferenceMismatch(String),
    #[error("permission request {0} is not pending")]
    PermissionRequestNotFound(String),
    #[error("permission option {0} was not advertised")]
    PermissionOptionNotAdvertised(String),
    #[error("agent transport disconnected: {0}")]
    Disconnected(String),
    #[error("agent transport protocol error: {0}")]
    Protocol(String),
    #[error("agent transport channel closed")]
    ChannelClosed,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
