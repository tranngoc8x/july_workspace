use crate::domain::SessionBindingId;
use crate::storage::StoreError;
use crate::transport::TransportError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error(transparent)]
    Storage(#[from] StoreError),
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("runtime owner channel closed")]
    ChannelClosed,
    #[error("SQLite owner thread panicked")]
    StorageWorkerPanicked,
    #[error("session binding does not belong to the connected agent")]
    BindingAgentMismatch,
    #[error("session binding has no remote session id")]
    MissingRemoteSession,
    #[error("permission request {0} was not observed by the session manager")]
    PermissionRequestNotFound(String),
    #[error("session binding {0} does not exist in durable state")]
    SessionBindingNotFound(SessionBindingId),
}
