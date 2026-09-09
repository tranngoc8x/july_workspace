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
    pub work: Option<RoomWorkIntent>,
}

/// Only explicit delegation creates durable Work; chat text is never classified.
#[derive(Clone, Debug, PartialEq)]
pub enum RoomWorkIntent {
    Create { title: String, goal: Option<String> },
    Bind { work_id: WorkItemId },
}

#[derive(Clone, Debug, PartialEq)]
pub struct RoomA2aTaskBinding {
    pub work_id: WorkItemId,
    pub task_id: String,
    pub room_id: RoomId,
    pub requester_agent_id: AgentId,
    pub owner_agent_id: AgentId,
}

/// A snapshot of canonical July state, never an independently mutable A2A task.
#[derive(Clone, Debug, PartialEq)]
pub struct RoomWork {
    pub binding: RoomA2aTaskBinding,
    pub work: WorkItem,
    pub results: Vec<WorkResult>,
}
