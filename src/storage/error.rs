use crate::domain::{AgentId, ConversationId, DomainError, MessageId, RoomId, WorkItemId};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Domain(#[from] DomainError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Id(#[from] ulid::DecodeError),
    #[error("integer value {value} for {field} is out of range")]
    IntegerOutOfRange { field: &'static str, value: i128 },
    #[error("invalid stored value for {0}")]
    InvalidStoredValue(&'static str),
    #[error("room member parent {found} does not match batch room {expected}")]
    RoomMemberParentMismatch { expected: RoomId, found: RoomId },
    #[error("conversation member parent {found} does not match batch conversation {expected}")]
    ConversationMemberParentMismatch {
        expected: ConversationId,
        found: ConversationId,
    },
    #[error("room {0} does not exist")]
    RoomNotFound(RoomId),
    #[error("room {0} is not active")]
    RoomInactive(RoomId),
    #[error("room id {0} already exists")]
    RoomIdConflict(RoomId),
    #[error("room name {0} already exists")]
    RoomNameConflict(String),
    #[error("agent {0} does not exist")]
    AgentNotFound(AgentId),
    #[error("agent {0} is not active")]
    AgentInactive(AgentId),
    #[error("thread {0} does not exist")]
    ThreadNotFound(ConversationId),
    #[error("conversation {0} is not a thread")]
    NotThread(ConversationId),
    #[error("thread {0} is not open")]
    ThreadNotOpen(ConversationId),
    #[error("thread {0} must be created through the Thread/primary Work aggregate")]
    ThreadAggregateRequired(ConversationId),
    #[error("{0} must be changed through the membership transition API")]
    MembershipTransitionRequired(&'static str),
    #[error("agent {agent_id} must be an active member of room {room_id}")]
    RoomMembershipRequired { room_id: RoomId, agent_id: AgentId },
    #[error("agent {agent_id} must be an active member of thread {thread_id}")]
    ThreadMembershipRequired {
        thread_id: ConversationId,
        agent_id: AgentId,
    },
    #[error("agent {agent_id} still has an active thread membership in room {room_id}")]
    RoomRemovalBlocked { room_id: RoomId, agent_id: AgentId },
    #[error("thread id {0} already exists")]
    ThreadIdConflict(ConversationId),
    #[error("primary work id {0} already exists")]
    PrimaryWorkIdConflict(WorkItemId),
    #[error("message sender must be agent {0}")]
    MessageSenderMismatch(AgentId),
    #[error("message {id} already exists with different content")]
    MessageConflict { id: MessageId },
    #[error("database schema version {found} is newer than supported version {supported}")]
    DatabaseTooNew { found: i64, supported: i64 },
}
