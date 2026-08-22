//! Runtime boundary for process, session, and cancellation lifecycle.

use chrono::{SecondsFormat, Utc};

mod direct_message;
mod error;
mod session_manager;
mod storage_worker;
mod thread;
mod workspace;

pub use direct_message::{
    AgentDirectMessageRuntime, DirectMessageBootstrapError, open_acp_direct_message,
};
pub use error::RuntimeError;
pub(crate) use session_manager::SessionManager;
pub(crate) use storage_worker::StorageHandle;
pub use storage_worker::StorageWorker;
pub use thread::AgentThreadRuntime;
pub use workspace::WorkspaceRuntime;
pub(crate) use workspace::{RuntimeSession, WorkspaceHandle};

pub(crate) fn timestamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}
