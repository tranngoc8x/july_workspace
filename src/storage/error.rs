use crate::domain::{
    AgentId, ConversationId, DecisionId, DecisionOwner, DecisionStatus, DomainError, HandoffId,
    HandoffStatus, MessageId, ProposalId, ProposalResponseId, ProposalStatus, PublishId, ResultId,
    RoomId, RoomMessageId, SessionBindingId, SessionBindingStatus, WorkItemId, WorkStatus,
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
    #[error("room message id {0} already exists with different content")]
    RoomMessageIdConflict(RoomMessageId),
    #[error("room message reply {0} does not exist")]
    RoomMessageReplyNotFound(RoomMessageId),
    #[error("room message reply {reply_to} does not belong to room {room_id}")]
    RoomMessageReplyNotInRoom {
        room_id: RoomId,
        reply_to: RoomMessageId,
    },
    #[error("room message {0} does not exist")]
    RoomMessageNotFound(RoomMessageId),
    #[error("agent {agent_id} is not a target of room message {message_id}")]
    RoomMessageTargetRequired {
        message_id: RoomMessageId,
        agent_id: AgentId,
    },
    #[error("room session {0} requires explicit recovery")]
    RoomSessionUnavailable(SessionBindingId),
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
    #[error("handoff {0} does not exist")]
    HandoffNotFound(HandoffId),
    #[error("handoff id {0} already exists with different content")]
    HandoffIdConflict(HandoffId),
    #[error("work {work_id} does not belong to thread {thread_id}")]
    HandoffWorkOutOfThread {
        work_id: WorkItemId,
        thread_id: ConversationId,
    },
    #[error("work {0} already has an open handoff")]
    HandoffAlreadyOpen(WorkItemId),
    #[error("only target agent {expected} may respond to handoff {handoff_id}")]
    HandoffRespondentMismatch {
        handoff_id: HandoffId,
        expected: AgentId,
    },
    #[error("only source agent {expected} may act on handoff {handoff_id}")]
    HandoffSourceMismatch {
        handoff_id: HandoffId,
        expected: AgentId,
    },
    #[error("handoff {handoff_id} cannot transition from {from} to {to}")]
    InvalidHandoffTransition {
        handoff_id: HandoffId,
        from: HandoffStatus,
        to: HandoffStatus,
    },
    #[error("handoff timestamp must not be blank")]
    InvalidHandoffTimestamp,
    #[error("challenging handoff {0} requires new evidence")]
    HandoffChallengeMissingEvidence(HandoffId),
    #[error("proposal {0} does not exist")]
    ProposalNotFound(ProposalId),
    #[error("proposal id {0} already exists with different content")]
    ProposalIdConflict(ProposalId),
    #[error("proposal response id {0} already exists with different content")]
    ProposalResponseIdConflict(ProposalResponseId),
    #[error("proposal {proposal_id} is {status} and takes no further responses")]
    ProposalNotLive {
        proposal_id: ProposalId,
        status: ProposalStatus,
    },
    #[error("proposal {proposal_id} cannot transition from {from} to {to}")]
    InvalidProposalTransition {
        proposal_id: ProposalId,
        from: ProposalStatus,
        to: ProposalStatus,
    },
    #[error("proposal {proposal_id} belongs to author {expected}")]
    ProposalAuthorMismatch {
        proposal_id: ProposalId,
        expected: AgentId,
    },
    #[error("proposal {proposal_id} cannot supersede proposal {superseded_id} from another thread")]
    ProposalSupersedeOutOfThread {
        proposal_id: ProposalId,
        superseded_id: ProposalId,
    },
    #[error("proposal {proposal_id} does not belong to thread {thread_id}")]
    ProposalOutOfThread {
        proposal_id: ProposalId,
        thread_id: ConversationId,
    },
    #[error("proposal timestamp must not be blank")]
    InvalidProposalTimestamp,
    #[error("decision {0} does not exist")]
    DecisionNotFound(DecisionId),
    #[error("decision id {0} already exists with different content")]
    DecisionIdConflict(DecisionId),
    #[error("decision {decision_id} cannot supersede decision {superseded_id} from another thread")]
    DecisionSupersedeOutOfThread {
        decision_id: DecisionId,
        superseded_id: DecisionId,
    },
    #[error("decision {decision_id} cannot transition from {from} to {to}")]
    InvalidDecisionTransition {
        decision_id: DecisionId,
        from: DecisionStatus,
        to: DecisionStatus,
    },
    #[error("decision {decision_id} belongs to owner {expected}")]
    DecisionOwnerMismatch {
        decision_id: DecisionId,
        expected: DecisionOwner,
    },
    #[error("decision {decision_id} is {status}, so it generates no work")]
    DecisionNotDecided {
        decision_id: DecisionId,
        status: DecisionStatus,
    },
    #[error("work {work_id} conflicts with an earlier conversion of decision {decision_id}")]
    DecisionWorkConflict {
        decision_id: DecisionId,
        work_id: WorkItemId,
    },
    #[error("decision {0} settles no handoff, so it cannot assign an owner")]
    DecisionHasNoHandoff(DecisionId),
    #[error("decision timestamp must not be blank")]
    InvalidDecisionTimestamp,
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
    #[error("session replacement source {0} does not exist")]
    SessionReplacementSourceNotFound(SessionBindingId),
    #[error(
        "session replacement source {source_binding_id} is stale; latest binding is {latest_binding_id}"
    )]
    SessionReplacementSourceStale {
        source_binding_id: SessionBindingId,
        latest_binding_id: SessionBindingId,
    },
    #[error("session replacement source {source_binding_id} is unavailable in status {status}")]
    SessionReplacementSourceUnavailable {
        source_binding_id: SessionBindingId,
        status: SessionBindingStatus,
    },
    #[error("session replacement source {0} exhausted its generation range")]
    SessionReplacementGenerationExhausted(SessionBindingId),
    #[error(
        "session replacement retry for source {source_binding_id} conflicts with replacement {replacement_binding_id}"
    )]
    SessionReplacementConflict {
        source_binding_id: SessionBindingId,
        replacement_binding_id: SessionBindingId,
    },
    #[error("session recovery for replacement {0} does not exist")]
    SessionRecoveryNotFound(SessionBindingId),
    #[error("session recovery replacement {0} has no attached remote session")]
    SessionRecoveryNotAttached(SessionBindingId),
    #[error("session recovery replacement {0} is already attached differently")]
    SessionRecoveryRemoteAttachmentConflict(SessionBindingId),
    #[error("database schema version {found} is newer than supported version {supported}")]
    DatabaseTooNew { found: i64, supported: i64 },
}
