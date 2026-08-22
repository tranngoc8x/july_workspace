use crate::domain::{
    AgentId, ConversationId, DomainError, MessageId, PublishId, ResultId, RoomId, WorkItemId,
    WorkStatus,
};
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
    #[error("work {0} does not exist")]
    WorkItemNotFound(WorkItemId),
    #[error("work dependency cannot reference itself: {0}")]
    WorkDependencySelf(WorkItemId),
    #[error("work dependency {upstream_work_id} -> {downstream_work_id} would create a cycle")]
    WorkDependencyCycle {
        upstream_work_id: WorkItemId,
        downstream_work_id: WorkItemId,
    },
    #[error(
        "work dependency {upstream_work_id} -> {downstream_work_id} already exists with different content"
    )]
    WorkDependencyConflict {
        upstream_work_id: WorkItemId,
        downstream_work_id: WorkItemId,
    },
    #[error("agent {owner_agent_id} is not an active member of work {work_id}'s conversation")]
    WorkOwnerScopeRequired {
        work_id: WorkItemId,
        owner_agent_id: AgentId,
    },
    #[error("terminal work {0} cannot change owner")]
    TerminalWorkOwnerImmutable(WorkItemId),
    #[error("work {work_id} cannot transition from {from} to {to}")]
    InvalidWorkTransition {
        work_id: WorkItemId,
        from: WorkStatus,
        to: WorkStatus,
    },
    #[error("work mutation timestamp must not be blank")]
    InvalidWorkTimestamp,
    #[error("result {0} already exists with different content")]
    WorkResultConflict(ResultId),
    #[error("superseded result {0} does not exist")]
    SupersededWorkResultNotFound(ResultId),
    #[error("result {result_id} cannot supersede result {supersedes_result_id} from another work")]
    CrossWorkResultSupersede {
        result_id: ResultId,
        supersedes_result_id: ResultId,
    },
    #[error("result {0} does not exist")]
    PublishResultNotFound(ResultId),
    #[error("source conversation {0} does not exist")]
    PublishSourceNotFound(ConversationId),
    #[error("target conversation {0} does not exist")]
    PublishTargetNotFound(ConversationId),
    #[error("publish id {0} already maps a different result or target")]
    PublishIdConflict(PublishId),
    #[error("publish timestamp must not be blank")]
    InvalidPublishTimestamp,
    #[error("message sender must be agent {0}")]
    MessageSenderMismatch(AgentId),
    #[error("message {id} already exists with different content")]
    MessageConflict { id: MessageId },
    #[error("delivery for message {message_id} and target {target_agent_id} conflicts")]
    DeliveryConflict {
        message_id: MessageId,
        target_agent_id: AgentId,
    },
    #[error("database schema version {found} is newer than supported version {supported}")]
    DatabaseTooNew { found: i64, supported: i64 },
}
