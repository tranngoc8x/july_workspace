use super::{
    AgentId, CheckpointId, ConversationId, DomainError, MemoryId, MessageId, PublishId, ResultId,
    RoomId, SessionBindingId, WorkItemId,
};
use serde_json::Value;
use std::fmt::{self, Display, Formatter};
use std::str::FromStr;

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
string_enum!(DependencyType {
    Requires => "requires",
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
        require_text(&self.updated_at, "work_item.updated_at")
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorkDependency {
    pub upstream_work_id: WorkItemId,
    pub downstream_work_id: WorkItemId,
    pub dependency_type: DependencyType,
    pub created_at: String,
}

impl WorkDependency {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.upstream_work_id == self.downstream_work_id {
            return Err(DomainError::SelfDependency);
        }
        require_text(&self.created_at, "work_dependency.created_at")
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
        require_text(&self.created_at, "work_result.created_at")
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
            created_at: "2026-08-09T00:00:00Z".into(),
        };
        assert_eq!(dependency.validate(), Err(DomainError::SelfDependency));
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
