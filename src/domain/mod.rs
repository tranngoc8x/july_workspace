//! Domain boundary for pure July Workspace concepts and invariants.

mod error;
mod id;
mod model;

pub use error::DomainError;
pub use id::*;
pub use model::*;

/// Agent-facing arguments. Sender and Room come from the host activation.
#[derive(Clone, Debug)]
pub struct SendRoomMessage {
    /// Names of agents to activate; empty publishes to the Room without activation.
    pub targets: Vec<String>,
    pub body: String,
    pub reply_to: Option<RoomMessageId>,
    pub request_id: Option<String>,
}
