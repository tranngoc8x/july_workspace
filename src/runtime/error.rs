use crate::domain::{AgentId, SessionBindingId};
use crate::storage::StoreError;
use crate::transport::TransportError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("agent {0} already has a runtime owner")]
    AgentAlreadyRegistered(AgentId),
    #[error("agent {0} has no runtime owner")]
    AgentNotRegistered(AgentId),
    #[error(transparent)]
    Storage(#[from] StoreError),
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("runtime owner channel closed")]
    ChannelClosed,
    #[error("no Tokio runtime is currently entered")]
    TokioRuntimeUnavailable,
    #[error("SQLite owner thread panicked")]
    StorageWorkerPanicked,
    #[error("agent owner task panicked")]
    OwnerTaskPanicked,
    #[error("session binding does not belong to the connected agent")]
    BindingAgentMismatch,
    #[error("session binding has no remote session id")]
    MissingRemoteSession,
    #[error("permission request {0} was not observed by the session manager")]
    PermissionRequestNotFound(String),
    #[error("session binding {0} does not exist in durable state")]
    SessionBindingNotFound(SessionBindingId),
    #[error("session binding {0} is already attached to this runtime owner")]
    SessionBindingAlreadyAttached(SessionBindingId),
    #[error("workspace runtime is stopped")]
    WorkspaceStopped,
}
