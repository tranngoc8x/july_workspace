use super::{
    AgentId, CheckpointId, ConversationId, DecisionId, DomainError, HandoffId, MemoryId, MessageId,
    ProposalId, ProposalResponseId, PublishId, ResultId, RoomId, RoomMessageId, SessionBindingId,
    WorkItemId,
};
use serde_json::Value;
use std::fmt::{self, Display, Formatter};
use std::str::FromStr;

pub const TRUSTED_LOCAL_USER_ID: &str = "local-user";

macro_rules! string_enum {
    ($name:ident { $($variant:ident => $value:literal),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
        pub enum $name {
            $($variant),+
        }

        impl Display for $name {
            fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
                formatter.write_str(match self {
                    $(Self::$variant => $value),+
                })
            }
        }

        impl FromStr for $name {
            type Err = DomainError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                match value {
                    $($value => Ok(Self::$variant),)+
                    _ => Err(DomainError::InvalidEnum {
                        kind: stringify!($name),
                        value: value.into(),
                    }),
                }
            }
        }
    };
}

string_enum!(ConversationKind {
    Dm => "dm",
    Thread => "thread",
});
string_enum!(MemberType {
    User => "user",
    Agent => "agent",
});
string_enum!(WorkStatus {
    Open => "open",
    Working => "working",
    Blocked => "blocked",
    Ready => "ready",
    Done => "done",
    Failed => "failed",
    Cancelled => "cancelled",
});

impl WorkStatus {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }

    pub const fn can_transition_to(self, target: Self) -> bool {
        matches!(
            (self, target),
            (Self::Open, Self::Working | Self::Blocked | Self::Cancelled)
                | (
                    Self::Working,
                    Self::Blocked | Self::Ready | Self::Failed | Self::Cancelled
                )
                | (
                    Self::Blocked,
                    Self::Working | Self::Failed | Self::Cancelled
                )
                | (Self::Ready, Self::Done)
        )
    }
}
string_enum!(DependencyType {
    Requires => "requires",
});
string_enum!(DependencyStatus {
    Waiting => "waiting",
    Satisfied => "satisfied",
    Failed => "failed",
    Superseded => "superseded",
});
string_enum!(MemoryKind {
    Fact => "fact",
    Decision => "decision",
    Constraint => "constraint",
    Result => "result",
    Reference => "reference",
});
string_enum!(MemoryScopeType {
    Project => "project",
    Room => "room",
    Agent => "agent",
});
string_enum!(SessionBindingStatus {
    Active => "active",
    Disconnected => "disconnected",
    Lost => "lost",
    Closed => "closed",
});
string_enum!(HandoffStatus {
    Proposed => "proposed",
    Accepted => "accepted",
    Rejected => "rejected",
    Partial => "partial",
    Disputed => "disputed",
    Resolved => "resolved",
    Cancelled => "cancelled",
});

impl HandoffStatus {
    /// A negotiation still waiting for the source, the target, or a decision.
    pub const fn is_open(self) -> bool {
        matches!(
            self,
            Self::Proposed | Self::Rejected | Self::Partial | Self::Disputed
        )
    }
}

// The target agent's answer to a proposed handoff.
string_enum!(HandoffDecision {
    Accept => "accept",
    Reject => "reject",
    Partial => "partial",
});

string_enum!(ProposalStatus {
    Open => "open",
    Amended => "amended",
    Accepted => "accepted",
    Rejected => "rejected",
    Superseded => "superseded",
    Withdrawn => "withdrawn",
});

impl ProposalStatus {
    /// A proposal still open to responses and selection.
    pub const fn is_live(self) -> bool {
        matches!(self, Self::Open | Self::Amended)
    }
}

string_enum!(ProposalResponseType {
    Support => "support",
    Challenge => "challenge",
    Amend => "amend",
    Reject => "reject",
});

string_enum!(DecisionType {
    Ownership => "ownership",
    Technical => "technical",
    Scope => "scope",
});
string_enum!(DecisionStatus {
    Pending => "pending",
    NeedsDecision => "needs_decision",
    Decided => "decided",
    Superseded => "superseded",
    Cancelled => "cancelled",
});

/// Who settles a decision. The default is the user; a named agent may be
/// nominated instead. A facilitator only recommends and is never the owner.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DecisionOwner {
    User,
    Agent(AgentId),
}

impl Display for DecisionOwner {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::User => formatter.write_str("user"),
            Self::Agent(agent_id) => agent_id.fmt(formatter),
        }
    }
}

impl FromStr for DecisionOwner {
    type Err = DomainError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value == "user" {
            return Ok(Self::User);
        }
        value
            .parse()
            .map(Self::Agent)
            .map_err(|_| DomainError::InvalidEnum {
                kind: "DecisionOwner",
                value: value.into(),
            })
    }
}

string_enum!(DeliveryStatus {
    Pending => "pending",
    Delivered => "delivered",
    Failed => "failed",
});

fn require_text(value: &str, field: &'static str) -> Result<(), DomainError> {
    if value.trim().is_empty() {
        Err(DomainError::EmptyField(field))
    } else {
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Agent {
    pub id: AgentId,
    pub name: String,
    pub project_root: String,
    pub transport_type: String,
    pub transport_config: Value,
    pub status: String,
    pub metadata: Value,
    pub created_at: String,
    pub updated_at: String,
}

impl Agent {
    pub fn validate(&self) -> Result<(), DomainError> {
        require_text(&self.name, "agent.name")?;
        require_text(&self.project_root, "agent.project_root")?;
        require_text(&self.transport_type, "agent.transport_type")?;
        require_text(&self.status, "agent.status")?;
        require_text(&self.created_at, "agent.created_at")?;
        require_text(&self.updated_at, "agent.updated_at")
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Room {
    pub id: RoomId,
    pub name: String,
    pub description: Option<String>,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

impl Room {
    pub fn validate(&self) -> Result<(), DomainError> {
        require_text(&self.name, "room.name")?;
        require_text(&self.status, "room.status")?;
        require_text(&self.created_at, "room.created_at")?;
        require_text(&self.updated_at, "room.updated_at")
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RoomMember {
    pub room_id: RoomId,
    pub agent_id: AgentId,
    pub role: Option<String>,
    pub generation: u32,
    pub joined_at: String,
    pub left_at: Option<String>,
}

impl RoomMember {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.generation == 0 {
            return Err(DomainError::InvalidMembershipGeneration);
        }
        require_text(&self.joined_at, "room_member.joined_at")
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RoomMessage {
    pub id: RoomMessageId,
    pub room_id: RoomId,
    pub sender_type: MemberType,
    pub sender_id: String,
    pub body: String,
    pub mentions: Vec<AgentId>,
    pub reply_to: Option<RoomMessageId>,
    pub created_at: String,
}

impl RoomMessage {
    pub fn validate(&self) -> Result<(), DomainError> {
        require_text(&self.sender_id, "room_message.sender_id")?;
        require_text(&self.body, "room_message.body")?;
        require_text(&self.created_at, "room_message.created_at")?;
        if self.sender_type == MemberType::User && self.sender_id != TRUSTED_LOCAL_USER_ID {
            return Err(DomainError::UntrustedRoomUserSender(self.sender_id.clone()));
        }
        if self
            .mentions
            .iter()
            .enumerate()
            .any(|(index, mention)| self.mentions[..index].contains(mention))
        {
            return Err(DomainError::DuplicateRoomMessageMention);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Conversation {
    pub id: ConversationId,
    pub kind: ConversationKind,
    pub room_id: Option<RoomId>,
    pub title: Option<String>,
    pub goal: Option<String>,
    pub parent_conversation_id: Option<ConversationId>,
    pub origin_conversation_id: Option<ConversationId>,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

impl Conversation {
    pub fn validate(&self) -> Result<(), DomainError> {
        require_text(&self.status, "conversation.status")?;
        require_text(&self.created_at, "conversation.created_at")?;
        require_text(&self.updated_at, "conversation.updated_at")?;
        match self.kind {
            ConversationKind::Dm if self.room_id.is_some() => Err(DomainError::DmHasRoom),
            ConversationKind::Thread if self.room_id.is_none() => {
                Err(DomainError::ThreadMissingRoom)
            }
            ConversationKind::Thread
                if self
                    .title
                    .as_deref()
                    .is_none_or(|title| title.trim().is_empty()) =>
            {
                Err(DomainError::ThreadMissingTitle)
            }
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ConversationMember {
    pub conversation_id: ConversationId,
    pub member_type: MemberType,
    pub member_id: String,
    pub generation: u32,
    pub joined_at: String,
    pub left_at: Option<String>,
}

impl ConversationMember {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.generation == 0 {
            return Err(DomainError::InvalidMembershipGeneration);
        }
        require_text(&self.member_id, "conversation_member.member_id")?;
        require_text(&self.joined_at, "conversation_member.joined_at")
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub id: MessageId,
    pub conversation_id: ConversationId,
    pub sender_type: MemberType,
    pub sender_id: String,
    pub body: String,
    pub reply_to: Option<MessageId>,
    pub metadata: Value,
    pub created_at: String,
}

impl Message {
    pub fn validate(&self) -> Result<(), DomainError> {
        require_text(&self.sender_id, "message.sender_id")?;
        require_text(&self.body, "message.body")?;
        require_text(&self.created_at, "message.created_at")
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MessageDelivery {
    pub message_id: MessageId,
    pub target_agent_id: AgentId,
    pub status: DeliveryStatus,
    pub capsule: Option<String>,
    pub capsule_delivered_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub delivered_at: Option<String>,
}

impl MessageDelivery {
    pub fn validate(&self) -> Result<(), DomainError> {
        if let Some(capsule) = &self.capsule {
            require_text(capsule, "message_delivery.capsule")?;
        }
        if let Some(delivered_at) = &self.capsule_delivered_at {
            require_text(delivered_at, "message_delivery.capsule_delivered_at")?;
        }
        require_text(&self.created_at, "message_delivery.created_at")?;
        require_text(&self.updated_at, "message_delivery.updated_at")?;
        if let Some(delivered_at) = &self.delivered_at {
            require_text(delivered_at, "message_delivery.delivered_at")?;
        }
        if self.capsule_delivered_at.is_some() && self.capsule.is_none() {
            return Err(DomainError::CapsuleDeliveryWithoutCapsule);
        }
        if (self.status == DeliveryStatus::Delivered) != self.delivered_at.is_some() {
            return Err(DomainError::DeliveryTimestampStatusMismatch);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorkItem {
    pub id: WorkItemId,
    pub conversation_id: ConversationId,
    pub title: String,
    pub goal: Option<String>,
    pub status: WorkStatus,
    pub owner_agent_id: Option<AgentId>,
    pub is_primary: bool,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
}

impl WorkItem {
    pub fn validate(&self) -> Result<(), DomainError> {
        require_text(&self.title, "work_item.title")?;
        require_text(&self.created_at, "work_item.created_at")?;
        require_text(&self.updated_at, "work_item.updated_at")?;
        if let Some(completed_at) = &self.completed_at {
            require_text(completed_at, "work_item.completed_at")?;
        }
        if self.status.is_terminal() != self.completed_at.is_some() {
            return Err(DomainError::WorkCompletionTimestampMismatch);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorkDependency {
    pub upstream_work_id: WorkItemId,
    pub downstream_work_id: WorkItemId,
    pub dependency_type: DependencyType,
    pub status: DependencyStatus,
    pub result_id: Option<ResultId>,
    pub created_at: String,
}

impl WorkDependency {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.upstream_work_id == self.downstream_work_id {
            return Err(DomainError::SelfDependency);
        }
        require_text(&self.created_at, "work_dependency.created_at")?;
        let result_matches_status = match self.status {
            DependencyStatus::Waiting | DependencyStatus::Failed => self.result_id.is_none(),
            DependencyStatus::Satisfied | DependencyStatus::Superseded => self.result_id.is_some(),
        };
        if !result_matches_status {
            return Err(DomainError::DependencyResultStatusMismatch);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorkResult {
    pub id: ResultId,
    pub work_id: WorkItemId,
    pub status: String,
    pub summary: String,
    pub outputs: Vec<String>,
    pub evidence: Vec<String>,
    pub supersedes_result_id: Option<ResultId>,
    pub created_at: String,
}

impl WorkResult {
    pub fn validate(&self) -> Result<(), DomainError> {
        require_text(&self.status, "work_result.status")?;
        require_text(&self.summary, "work_result.summary")?;
        require_text(&self.created_at, "work_result.created_at")?;
        if self.supersedes_result_id == Some(self.id) {
            return Err(DomainError::ResultSupersedesItself);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Publish {
    pub id: PublishId,
    pub result_id: ResultId,
    pub source_conversation_id: ConversationId,
    pub target_conversation_id: ConversationId,
    pub created_at: String,
}

impl Publish {
    pub fn validate(&self) -> Result<(), DomainError> {
        require_text(&self.created_at, "publish.created_at")
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionBinding {
    pub id: SessionBindingId,
    pub conversation_id: ConversationId,
    pub agent_id: AgentId,
    pub transport_type: String,
    pub remote_session_id: Option<String>,
    pub generation: u64,
    pub status: SessionBindingStatus,
    pub created_at: String,
    pub last_used_at: String,
}

impl SessionBinding {
    pub fn validate(&self) -> Result<(), DomainError> {
        require_text(&self.transport_type, "session_binding.transport_type")?;
        require_text(&self.created_at, "session_binding.created_at")?;
        require_text(&self.last_used_at, "session_binding.last_used_at")?;
        if self.generation == 0 {
            return Err(DomainError::InvalidSessionGeneration);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionRecovery {
    pub session_binding_id: SessionBindingId,
    pub source_binding_id: SessionBindingId,
    pub capsule: String,
    pub capsule_delivered_at: Option<String>,
    pub created_at: String,
}

impl SessionRecovery {
    pub fn validate(&self) -> Result<(), DomainError> {
        require_text(&self.capsule, "session_recovery.capsule")?;
        require_text(&self.created_at, "session_recovery.created_at")?;
        if let Some(delivered_at) = self.capsule_delivered_at.as_deref() {
            require_text(delivered_at, "session_recovery.capsule_delivered_at")?;
        }
        if self.session_binding_id == self.source_binding_id {
            return Err(DomainError::SessionRecoverySelfReference);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PermissionOption {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PermissionOutcome {
    Selected(String),
    Cancelled,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PermissionDecision {
    pub id: String,
    pub session_binding_id: SessionBindingId,
    pub correlation_id: String,
    pub options: Vec<PermissionOption>,
    pub outcome: PermissionOutcome,
    pub decided_at: String,
}

impl PermissionDecision {
    pub fn validate(&self) -> Result<(), DomainError> {
        require_text(&self.id, "permission_decision.id")?;
        require_text(&self.correlation_id, "permission_decision.correlation_id")?;
        require_text(&self.decided_at, "permission_decision.decided_at")?;
        for option in &self.options {
            require_text(&option.id, "permission_option.id")?;
            require_text(&option.label, "permission_option.label")?;
        }
        if let PermissionOutcome::Selected(selected) = &self.outcome
            && !self.options.iter().any(|option| option.id == *selected)
        {
            return Err(DomainError::PermissionOptionNotAdvertised(selected.clone()));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Checkpoint {
    pub id: CheckpointId,
    pub conversation_id: ConversationId,
    pub agent_id: AgentId,
    pub goal: Option<String>,
    pub current_state: Option<String>,
    pub decisions: Vec<String>,
    pub open_items: Vec<String>,
    pub references: Vec<String>,
    pub last_message_id: Option<MessageId>,
    pub created_at: String,
}

impl Checkpoint {
    pub fn validate(&self) -> Result<(), DomainError> {
        require_text(&self.created_at, "checkpoint.created_at")
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Memory {
    pub id: MemoryId,
    pub scope_type: MemoryScopeType,
    pub scope_id: String,
    pub kind: MemoryKind,
    pub content: String,
    pub source_conversation_id: Option<ConversationId>,
    pub evidence: Vec<String>,
    pub supersedes_memory_id: Option<MemoryId>,
    pub created_at: String,
}

impl Memory {
    pub fn validate(&self) -> Result<(), DomainError> {
        require_text(&self.scope_id, "memory.scope_id")?;
        require_text(&self.content, "memory.content")?;
        require_text(&self.created_at, "memory.created_at")
    }
}

/// The structured answer a target agent gives to a proposed handoff.
#[derive(Clone, Debug, PartialEq)]
pub struct HandoffResponse {
    pub agent_id: AgentId,
    pub decision: HandoffDecision,
    pub reason: Option<String>,
    pub evidence: Vec<String>,
    pub owned_scope: Vec<String>,
    pub rejected_scope: Vec<String>,
    pub proposed_owner_id: Option<AgentId>,
}

impl HandoffResponse {
    pub fn accept(agent_id: AgentId) -> Self {
        Self {
            agent_id,
            decision: HandoffDecision::Accept,
            reason: None,
            evidence: Vec::new(),
            owned_scope: Vec::new(),
            rejected_scope: Vec::new(),
            proposed_owner_id: None,
        }
    }

    pub fn reject(agent_id: AgentId, reason: impl Into<String>, evidence: Vec<String>) -> Self {
        Self {
            reason: Some(reason.into()),
            evidence,
            decision: HandoffDecision::Reject,
            ..Self::accept(agent_id)
        }
    }

    pub fn partial(
        agent_id: AgentId,
        reason: impl Into<String>,
        owned_scope: Vec<String>,
        rejected_scope: Vec<String>,
    ) -> Self {
        Self {
            reason: Some(reason.into()),
            owned_scope,
            rejected_scope,
            decision: HandoffDecision::Partial,
            ..Self::accept(agent_id)
        }
    }

    #[must_use]
    pub fn with_proposed_owner(mut self, proposed_owner_id: AgentId) -> Self {
        self.proposed_owner_id = Some(proposed_owner_id);
        self
    }

    #[must_use]
    pub fn with_evidence(mut self, evidence: Vec<String>) -> Self {
        self.evidence = evidence;
        self
    }
}

/// The source agent's structured challenge to a rejection, carrying the
/// decision to open if the round budget is already spent.
#[derive(Clone, Debug, PartialEq)]
pub struct HandoffChallenge {
    pub agent_id: AgentId,
    pub evidence: Vec<String>,
    pub decision_id: DecisionId,
    pub decision_owner: DecisionOwner,
}

impl HandoffChallenge {
    pub fn new(agent_id: AgentId, evidence: Vec<String>) -> Self {
        Self {
            agent_id,
            evidence,
            decision_id: DecisionId::new(),
            decision_owner: DecisionOwner::User,
        }
    }

    #[must_use]
    pub fn decided_by(mut self, decision_owner: DecisionOwner) -> Self {
        self.decision_owner = decision_owner;
        self
    }
}

/// What the decision owner concluded.
#[derive(Clone, Debug, PartialEq)]
pub struct DecisionOutcome {
    pub decided_by: DecisionOwner,
    pub decision: String,
    pub reason: Option<String>,
    pub evidence: Vec<String>,
    pub selected_proposal_id: Option<ProposalId>,
    /// For an ownership dispute: the agent that ends up owning the work.
    pub assigned_owner_id: Option<AgentId>,
}

impl DecisionOutcome {
    pub fn new(decided_by: DecisionOwner, decision: impl Into<String>) -> Self {
        Self {
            decided_by,
            decision: decision.into(),
            reason: None,
            evidence: Vec::new(),
            selected_proposal_id: None,
            assigned_owner_id: None,
        }
    }

    #[must_use]
    pub fn assigning_owner(mut self, owner_agent_id: AgentId) -> Self {
        self.assigned_owner_id = Some(owner_agent_id);
        self
    }

    #[must_use]
    pub fn because(mut self, reason: impl Into<String>, evidence: Vec<String>) -> Self {
        self.reason = Some(reason.into());
        self.evidence = evidence;
        self
    }
}

/// One ownership negotiation: the source agent proposes that the target agent
/// owns a work item, and the target answers with evidence.
#[derive(Clone, Debug, PartialEq)]
pub struct Handoff {
    pub id: HandoffId,
    pub thread_id: ConversationId,
    pub work_id: WorkItemId,
    pub from_agent_id: AgentId,
    pub to_agent_id: AgentId,
    pub status: HandoffStatus,
    pub reason: Option<String>,
    pub evidence: Vec<String>,
    pub owned_scope: Vec<String>,
    pub rejected_scope: Vec<String>,
    pub proposed_owner_id: Option<AgentId>,
    pub round_count: u32,
    /// Set when the dispute exhausted its rounds and needs a decision.
    pub decision_id: Option<DecisionId>,
    pub created_at: String,
    pub updated_at: String,
}

impl Handoff {
    /// Structured challenge rounds allowed before a dispute must be decided
    /// instead of continuing: claim, response, and one challenged response.
    pub const MAX_DISPUTE_ROUNDS: u32 = 2;

    pub fn validate(&self) -> Result<(), DomainError> {
        if self.from_agent_id == self.to_agent_id {
            return Err(DomainError::HandoffSelfTarget);
        }
        require_text(&self.created_at, "handoff.created_at")?;
        require_text(&self.updated_at, "handoff.updated_at")?;
        let scoped = self.status == HandoffStatus::Partial;
        if !scoped && !(self.owned_scope.is_empty() && self.rejected_scope.is_empty()) {
            return Err(DomainError::HandoffScopeNotAllowed);
        }
        match self.status {
            HandoffStatus::Rejected | HandoffStatus::Disputed => {
                self.require_reason("rejected")?;
                if self.evidence.is_empty() {
                    return Err(DomainError::HandoffRejectionMissingEvidence);
                }
            }
            HandoffStatus::Partial => {
                self.require_reason("partial")?;
                if self.owned_scope.is_empty() || self.rejected_scope.is_empty() {
                    return Err(DomainError::HandoffPartialScopeMissing);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn require_reason(&self, kind: &'static str) -> Result<(), DomainError> {
        match &self.reason {
            Some(reason) if !reason.trim().is_empty() => Ok(()),
            _ => Err(DomainError::HandoffResponseMissingReason(kind)),
        }
    }
}

/// One candidate solution offered to a thread.
#[derive(Clone, Debug, PartialEq)]
pub struct Proposal {
    pub id: ProposalId,
    pub thread_id: ConversationId,
    pub author_agent_id: AgentId,
    pub title: String,
    pub problem_statement: Option<String>,
    pub approach: Option<String>,
    pub benefits: Vec<String>,
    pub costs: Vec<String>,
    pub risks: Vec<String>,
    pub assumptions: Vec<String>,
    pub evidence: Vec<String>,
    pub status: ProposalStatus,
    pub supersedes_proposal_id: Option<ProposalId>,
    pub created_at: String,
    pub updated_at: String,
}

impl Proposal {
    pub fn validate(&self) -> Result<(), DomainError> {
        require_text(&self.title, "proposal.title")?;
        require_text(&self.created_at, "proposal.created_at")?;
        require_text(&self.updated_at, "proposal.updated_at")?;
        if self.supersedes_proposal_id == Some(self.id) {
            return Err(DomainError::ProposalSupersedesItself);
        }
        Ok(())
    }
}

/// One agent's structured answer to a proposal. Disagreement must carry
/// actionable content, never bare rejection.
#[derive(Clone, Debug, PartialEq)]
pub struct ProposalResponse {
    pub id: ProposalResponseId,
    pub proposal_id: ProposalId,
    pub agent_id: AgentId,
    pub response_type: ProposalResponseType,
    pub reason: Option<String>,
    pub evidence: Vec<String>,
    pub created_at: String,
}

impl ProposalResponse {
    pub fn validate(&self) -> Result<(), DomainError> {
        require_text(&self.created_at, "proposal_response.created_at")?;
        let stated = self
            .reason
            .as_ref()
            .is_some_and(|reason| !reason.trim().is_empty());
        match self.response_type {
            ProposalResponseType::Support => Ok(()),
            ProposalResponseType::Amend if stated => Ok(()),
            ProposalResponseType::Challenge | ProposalResponseType::Reject
                if stated && !self.evidence.is_empty() =>
            {
                Ok(())
            }
            _ => Err(DomainError::ProposalResponseNotActionable(
                self.response_type,
            )),
        }
    }
}

/// One work item a decision asks for, with the upstream work it waits on.
#[derive(Clone, Debug, PartialEq)]
pub struct DecisionWork {
    pub work_id: WorkItemId,
    pub title: String,
    pub goal: Option<String>,
    pub owner_agent_id: Option<AgentId>,
    pub depends_on: Vec<WorkItemId>,
}

impl DecisionWork {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            work_id: WorkItemId::new(),
            title: title.into(),
            goal: None,
            owner_agent_id: None,
            depends_on: Vec::new(),
        }
    }

    #[must_use]
    pub fn owned_by(mut self, owner_agent_id: AgentId) -> Self {
        self.owner_agent_id = Some(owner_agent_id);
        self
    }

    #[must_use]
    pub fn after(mut self, upstream_work_id: WorkItemId) -> Self {
        self.depends_on.push(upstream_work_id);
        self
    }
}

/// A durable conclusion of a deliberation. It is workspace state, not a chat
/// message, and it stays distinct from the work it may generate.
#[derive(Clone, Debug, PartialEq)]
pub struct Decision {
    pub id: DecisionId,
    pub thread_id: ConversationId,
    pub decision_type: DecisionType,
    pub title: String,
    pub decision: Option<String>,
    pub reason: Option<String>,
    pub selected_proposal_id: Option<ProposalId>,
    pub alternatives: Vec<String>,
    pub evidence: Vec<String>,
    pub participants: Vec<AgentId>,
    pub decision_owner: DecisionOwner,
    pub status: DecisionStatus,
    pub supersedes_decision_id: Option<DecisionId>,
    pub created_at: String,
    pub updated_at: String,
}

impl Decision {
    pub fn validate(&self) -> Result<(), DomainError> {
        require_text(&self.title, "decision.title")?;
        require_text(&self.created_at, "decision.created_at")?;
        require_text(&self.updated_at, "decision.updated_at")?;
        if self.supersedes_decision_id == Some(self.id) {
            return Err(DomainError::DecisionSupersedesItself);
        }
        let stated = self
            .decision
            .as_ref()
            .is_some_and(|decision| !decision.trim().is_empty());
        let settled = matches!(
            self.status,
            DecisionStatus::Decided | DecisionStatus::Superseded
        );
        if stated != settled {
            return Err(DomainError::DecisionOutcomeStatusMismatch);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::*;
    use serde_json::json;
    use std::fmt::{Debug, Display};
    use std::str::FromStr;

    fn assert_enum_roundtrip<T>(cases: &[(T, &str)])
    where
        T: Clone + Debug + Display + FromStr + PartialEq,
        T::Err: Debug,
    {
        for (value, text) in cases {
            assert_eq!(value.to_string(), *text);
            assert_eq!(T::from_str(text).unwrap(), value.clone());
        }
        assert!(T::from_str("invalid").is_err());
    }

    #[test]
    fn enums_round_trip_as_exact_snake_case_and_reject_invalid_input() {
        assert_enum_roundtrip(&[
            (ConversationKind::Dm, "dm"),
            (ConversationKind::Thread, "thread"),
        ]);
        assert_enum_roundtrip(&[(MemberType::User, "user"), (MemberType::Agent, "agent")]);
        assert_enum_roundtrip(&[
            (WorkStatus::Open, "open"),
            (WorkStatus::Working, "working"),
            (WorkStatus::Blocked, "blocked"),
            (WorkStatus::Ready, "ready"),
            (WorkStatus::Done, "done"),
            (WorkStatus::Failed, "failed"),
            (WorkStatus::Cancelled, "cancelled"),
        ]);
        assert_enum_roundtrip(&[(DependencyType::Requires, "requires")]);
        assert_enum_roundtrip(&[
            (DependencyStatus::Waiting, "waiting"),
            (DependencyStatus::Satisfied, "satisfied"),
            (DependencyStatus::Failed, "failed"),
            (DependencyStatus::Superseded, "superseded"),
        ]);
        assert_enum_roundtrip(&[
            (MemoryKind::Fact, "fact"),
            (MemoryKind::Decision, "decision"),
            (MemoryKind::Constraint, "constraint"),
            (MemoryKind::Result, "result"),
            (MemoryKind::Reference, "reference"),
        ]);
        assert_enum_roundtrip(&[
            (MemoryScopeType::Project, "project"),
            (MemoryScopeType::Room, "room"),
            (MemoryScopeType::Agent, "agent"),
        ]);
        assert_enum_roundtrip(&[
            (SessionBindingStatus::Active, "active"),
            (SessionBindingStatus::Disconnected, "disconnected"),
            (SessionBindingStatus::Lost, "lost"),
            (SessionBindingStatus::Closed, "closed"),
        ]);
    }

    fn valid_agent() -> Agent {
        Agent {
            id: AgentId::new(),
            name: "cashpoint".into(),
            project_root: "/workspace/cashpoint".into(),
            transport_type: "acp".into(),
            transport_config: json!({"command": "codex"}),
            status: "active".into(),
            metadata: json!({"owner": "payments"}),
            created_at: "2026-08-09T00:00:00Z".into(),
            updated_at: "2026-08-09T00:00:00Z".into(),
        }
    }

    fn valid_room() -> Room {
        Room {
            id: RoomId::new(),
            name: "VNA".into(),
            description: None,
            status: "active".into(),
            created_at: "2026-08-09T00:00:00Z".into(),
            updated_at: "2026-08-09T00:00:00Z".into(),
        }
    }

    fn valid_conversation(kind: ConversationKind) -> Conversation {
        Conversation {
            id: ConversationId::new(),
            kind,
            room_id: None,
            title: None,
            goal: None,
            parent_conversation_id: None,
            origin_conversation_id: None,
            status: "open".into(),
            created_at: "2026-08-09T00:00:00Z".into(),
            updated_at: "2026-08-09T00:00:00Z".into(),
        }
    }

    fn valid_message() -> Message {
        Message {
            id: MessageId::new(),
            conversation_id: ConversationId::new(),
            sender_type: MemberType::Agent,
            sender_id: AgentId::new().to_string(),
            body: "Done".into(),
            reply_to: None,
            metadata: json!({}),
            created_at: "2026-08-09T00:00:00Z".into(),
        }
    }

    fn valid_work_item() -> WorkItem {
        WorkItem {
            id: WorkItemId::new(),
            conversation_id: ConversationId::new(),
            title: "Implement domain".into(),
            goal: None,
            status: WorkStatus::Open,
            owner_agent_id: None,
            is_primary: false,
            created_at: "2026-08-09T00:00:00Z".into(),
            updated_at: "2026-08-09T00:00:00Z".into(),
            completed_at: None,
        }
    }

    fn valid_result() -> WorkResult {
        WorkResult {
            id: ResultId::new(),
            work_id: WorkItemId::new(),
            status: "accepted".into(),
            summary: "Domain complete".into(),
            outputs: vec!["src/domain/model.rs".into()],
            evidence: vec!["cargo test".into()],
            supersedes_result_id: None,
            created_at: "2026-08-09T00:00:00Z".into(),
        }
    }

    fn valid_session_binding() -> SessionBinding {
        SessionBinding {
            id: SessionBindingId::new(),
            conversation_id: ConversationId::new(),
            agent_id: AgentId::new(),
            transport_type: "acp".into(),
            remote_session_id: None,
            generation: 1,
            status: SessionBindingStatus::Active,
            created_at: "2026-08-09T00:00:00Z".into(),
            last_used_at: "2026-08-09T00:00:00Z".into(),
        }
    }

    fn valid_memory() -> Memory {
        Memory {
            id: MemoryId::new(),
            scope_type: MemoryScopeType::Project,
            scope_id: "cashpoint".into(),
            kind: MemoryKind::Fact,
            content: "Callbacks are idempotent".into(),
            source_conversation_id: None,
            evidence: vec![],
            supersedes_memory_id: None,
            created_at: "2026-08-09T00:00:00Z".into(),
        }
    }

    #[test]
    fn required_text_fields_reject_blank_values() {
        macro_rules! rejects_blank {
            ($value:expr, $field:ident, $name:literal) => {{
                let mut value = $value;
                value.$field = " ".into();
                assert_eq!(value.validate(), Err(DomainError::EmptyField($name)));
            }};
        }

        rejects_blank!(valid_agent(), name, "agent.name");
        rejects_blank!(valid_agent(), project_root, "agent.project_root");
        rejects_blank!(valid_agent(), transport_type, "agent.transport_type");
        rejects_blank!(valid_agent(), status, "agent.status");
        rejects_blank!(valid_agent(), created_at, "agent.created_at");
        rejects_blank!(valid_agent(), updated_at, "agent.updated_at");

        rejects_blank!(valid_room(), name, "room.name");
        rejects_blank!(valid_room(), status, "room.status");
        rejects_blank!(valid_room(), created_at, "room.created_at");
        rejects_blank!(valid_room(), updated_at, "room.updated_at");

        let mut member = RoomMember {
            room_id: RoomId::new(),
            agent_id: AgentId::new(),
            role: None,
            generation: 1,
            joined_at: String::new(),
            left_at: None,
        };
        assert_eq!(
            member.validate(),
            Err(DomainError::EmptyField("room_member.joined_at"))
        );
        member.joined_at = "2026-08-09T00:00:00Z".into();
        assert!(member.validate().is_ok());

        let mut conversation_member = ConversationMember {
            conversation_id: ConversationId::new(),
            member_type: MemberType::User,
            member_id: String::new(),
            generation: 1,
            joined_at: "2026-08-09T00:00:00Z".into(),
            left_at: None,
        };
        assert_eq!(
            conversation_member.validate(),
            Err(DomainError::EmptyField("conversation_member.member_id"))
        );
        conversation_member.member_id = "tony".into();
        assert!(conversation_member.validate().is_ok());
        conversation_member.joined_at.clear();
        assert_eq!(
            conversation_member.validate(),
            Err(DomainError::EmptyField("conversation_member.joined_at"))
        );

        rejects_blank!(
            valid_conversation(ConversationKind::Dm),
            status,
            "conversation.status"
        );
        rejects_blank!(
            valid_conversation(ConversationKind::Dm),
            created_at,
            "conversation.created_at"
        );
        rejects_blank!(
            valid_conversation(ConversationKind::Dm),
            updated_at,
            "conversation.updated_at"
        );

        rejects_blank!(valid_message(), sender_id, "message.sender_id");
        rejects_blank!(valid_message(), created_at, "message.created_at");

        rejects_blank!(valid_work_item(), title, "work_item.title");
        rejects_blank!(valid_work_item(), created_at, "work_item.created_at");
        rejects_blank!(valid_work_item(), updated_at, "work_item.updated_at");

        let work_id = WorkItemId::new();
        let dependency = WorkDependency {
            upstream_work_id: work_id,
            downstream_work_id: WorkItemId::new(),
            dependency_type: DependencyType::Requires,
            status: DependencyStatus::Waiting,
            result_id: None,
            created_at: String::new(),
        };
        assert_eq!(
            dependency.validate(),
            Err(DomainError::EmptyField("work_dependency.created_at"))
        );

        rejects_blank!(valid_result(), created_at, "work_result.created_at");

        let mut publish = Publish {
            id: PublishId::new(),
            result_id: ResultId::new(),
            source_conversation_id: ConversationId::new(),
            target_conversation_id: ConversationId::new(),
            created_at: String::new(),
        };
        assert_eq!(
            publish.validate(),
            Err(DomainError::EmptyField("publish.created_at"))
        );
        publish.created_at = "2026-08-09T00:00:00Z".into();
        assert!(publish.validate().is_ok());

        rejects_blank!(
            valid_session_binding(),
            transport_type,
            "session_binding.transport_type"
        );
        rejects_blank!(
            valid_session_binding(),
            created_at,
            "session_binding.created_at"
        );
        rejects_blank!(
            valid_session_binding(),
            last_used_at,
            "session_binding.last_used_at"
        );

        let mut checkpoint = Checkpoint {
            id: CheckpointId::new(),
            conversation_id: ConversationId::new(),
            agent_id: AgentId::new(),
            goal: None,
            current_state: None,
            decisions: vec![],
            open_items: vec![],
            references: vec![],
            last_message_id: None,
            created_at: String::new(),
        };
        assert_eq!(
            checkpoint.validate(),
            Err(DomainError::EmptyField("checkpoint.created_at"))
        );
        checkpoint.created_at = "2026-08-09T00:00:00Z".into();
        assert!(checkpoint.validate().is_ok());

        rejects_blank!(valid_memory(), created_at, "memory.created_at");
    }

    #[test]
    fn membership_generations_must_be_positive() {
        let room_member = RoomMember {
            room_id: RoomId::new(),
            agent_id: AgentId::new(),
            role: None,
            generation: 0,
            joined_at: "2026-08-09T00:00:00Z".into(),
            left_at: None,
        };
        assert_eq!(
            room_member.validate(),
            Err(DomainError::InvalidMembershipGeneration)
        );

        let conversation_member = ConversationMember {
            conversation_id: ConversationId::new(),
            member_type: MemberType::Agent,
            member_id: AgentId::new().to_string(),
            generation: 0,
            joined_at: "2026-08-09T00:00:00Z".into(),
            left_at: None,
        };
        assert_eq!(
            conversation_member.validate(),
            Err(DomainError::InvalidMembershipGeneration)
        );
    }

    #[test]
    fn conversation_kind_enforces_room_and_title_shape() {
        let mut dm = valid_conversation(ConversationKind::Dm);
        dm.room_id = Some(RoomId::new());
        assert_eq!(dm.validate(), Err(DomainError::DmHasRoom));

        let thread = valid_conversation(ConversationKind::Thread);
        assert_eq!(thread.validate(), Err(DomainError::ThreadMissingRoom));

        let mut thread = valid_conversation(ConversationKind::Thread);
        thread.room_id = Some(RoomId::new());
        thread.title = Some(" ".into());
        assert_eq!(thread.validate(), Err(DomainError::ThreadMissingTitle));

        thread.title = Some("Payment callback".into());
        assert!(thread.validate().is_ok());
    }

    #[test]
    fn message_body_must_not_be_blank() {
        let mut message = valid_message();
        message.body = "\t".into();
        assert_eq!(
            message.validate(),
            Err(DomainError::EmptyField("message.body"))
        );
    }

    #[test]
    fn dependency_rejects_a_self_edge() {
        let work_id = WorkItemId::new();
        let dependency = WorkDependency {
            upstream_work_id: work_id,
            downstream_work_id: work_id,
            dependency_type: DependencyType::Requires,
            status: DependencyStatus::Waiting,
            result_id: None,
            created_at: "2026-08-09T00:00:00Z".into(),
        };
        assert_eq!(dependency.validate(), Err(DomainError::SelfDependency));
    }

    #[test]
    fn dependency_result_reference_must_match_status() {
        let mut dependency = WorkDependency {
            upstream_work_id: WorkItemId::new(),
            downstream_work_id: WorkItemId::new(),
            dependency_type: DependencyType::Requires,
            status: DependencyStatus::Waiting,
            result_id: None,
            created_at: "2026-08-09T00:00:00Z".into(),
        };

        for (status, result_id) in [
            (DependencyStatus::Waiting, Some(ResultId::new())),
            (DependencyStatus::Failed, Some(ResultId::new())),
            (DependencyStatus::Satisfied, None),
            (DependencyStatus::Superseded, None),
        ] {
            dependency.status = status;
            dependency.result_id = result_id;
            assert_eq!(
                dependency.validate(),
                Err(DomainError::DependencyResultStatusMismatch)
            );
        }

        for (status, result_id) in [
            (DependencyStatus::Waiting, None),
            (DependencyStatus::Failed, None),
            (DependencyStatus::Satisfied, Some(ResultId::new())),
            (DependencyStatus::Superseded, Some(ResultId::new())),
        ] {
            dependency.status = status;
            dependency.result_id = result_id;
            assert!(dependency.validate().is_ok());
        }
    }

    #[test]
    fn session_generation_must_be_positive() {
        let mut binding = valid_session_binding();
        binding.generation = 0;
        assert_eq!(
            binding.validate(),
            Err(DomainError::InvalidSessionGeneration)
        );
    }

    #[test]
    fn session_recovery_validates_identity_and_progress_text() {
        let replacement_id = SessionBindingId::new();
        let source_id = SessionBindingId::new();
        let valid = SessionRecovery {
            session_binding_id: replacement_id,
            source_binding_id: source_id,
            capsule: "capsule".into(),
            capsule_delivered_at: None,
            created_at: "2026-08-22T00:00:00Z".into(),
        };
        assert!(valid.validate().is_ok());

        for (recovery, field) in [
            (
                SessionRecovery {
                    capsule: " ".into(),
                    ..valid.clone()
                },
                "session_recovery.capsule",
            ),
            (
                SessionRecovery {
                    created_at: " ".into(),
                    ..valid.clone()
                },
                "session_recovery.created_at",
            ),
            (
                SessionRecovery {
                    capsule_delivered_at: Some(" ".into()),
                    ..valid.clone()
                },
                "session_recovery.capsule_delivered_at",
            ),
        ] {
            assert_eq!(
                recovery.validate(),
                Err(DomainError::EmptyField(field)),
                "accepted blank {field}"
            );
        }

        assert_eq!(
            SessionRecovery {
                source_binding_id: replacement_id,
                ..valid
            }
            .validate(),
            Err(DomainError::SessionRecoverySelfReference)
        );
    }

    #[test]
    fn permission_selection_must_have_been_advertised() {
        let decision = PermissionDecision {
            id: "decision-1".into(),
            session_binding_id: SessionBindingId::new(),
            correlation_id: "request-1".into(),
            options: vec![PermissionOption {
                id: "allow-once".into(),
                label: "Allow once".into(),
            }],
            outcome: PermissionOutcome::Selected("allow-always".into()),
            decided_at: "2026-08-09T00:00:00Z".into(),
        };

        assert_eq!(
            decision.validate(),
            Err(DomainError::PermissionOptionNotAdvertised(
                "allow-always".into()
            ))
        );
    }

    #[test]
    fn result_status_and_summary_must_not_be_blank() {
        let mut result = valid_result();
        result.status.clear();
        assert_eq!(
            result.validate(),
            Err(DomainError::EmptyField("work_result.status"))
        );

        let mut result = valid_result();
        result.summary.clear();
        assert_eq!(
            result.validate(),
            Err(DomainError::EmptyField("work_result.summary"))
        );
    }

    #[test]
    fn memory_scope_and_content_must_not_be_blank() {
        let mut memory = valid_memory();
        memory.scope_id.clear();
        assert_eq!(
            memory.validate(),
            Err(DomainError::EmptyField("memory.scope_id"))
        );

        memory.scope_id = "cashpoint".into();
        memory.content = " ".into();
        assert_eq!(
            memory.validate(),
            Err(DomainError::EmptyField("memory.content"))
        );
    }

    #[test]
    fn valid_records_pass_immediate_invariants() {
        assert!(valid_agent().validate().is_ok());
        assert!(valid_room().validate().is_ok());
        assert!(valid_conversation(ConversationKind::Dm).validate().is_ok());
        assert!(valid_message().validate().is_ok());
        assert!(valid_work_item().validate().is_ok());
        assert!(valid_result().validate().is_ok());
        assert!(valid_session_binding().validate().is_ok());
        assert!(valid_memory().validate().is_ok());
    }
}
