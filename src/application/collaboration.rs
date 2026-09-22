use crate::domain::{
    Agent, AgentId, Conversation, ConversationId, ConversationKind, ConversationMember, Message,
    MessageDelivery, MessageId, Room, RoomId, RoomMember, RoomMessage, RoomMessageId,
    SessionBindingId, SessionBindingStatus, WorkItem, WorkItemId, WorkResult,
};
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RoomRef {
    Id(RoomId),
    Name(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AgentRef {
    Id(AgentId),
    Name(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MembershipState {
    Active,
    Left,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MembershipChange {
    pub state: MembershipState,
    pub changed: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FailedMessageDelivery {
    pub message: Message,
    pub delivery: MessageDelivery,
    pub conversation_kind: ConversationKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateRoom {
    pub room_id: RoomId,
    pub name: String,
    pub description: Option<String>,
    pub created_at: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AppendRoomMessage {
    pub message: RoomMessage,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AddRoomMember {
    pub room: RoomRef,
    pub agent: AgentRef,
    pub role: Option<String>,
    pub changed_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoveRoomMember {
    pub room: RoomRef,
    pub agent: AgentRef,
    pub changed_at: String,
}

/// Agent onboarding: identity plus its project binding. Runtime preference is
/// configuration, not identity, so it is stored as metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AddAgent {
    pub agent_id: AgentId,
    pub name: String,
    pub project_root: String,
    pub transport_type: String,
    pub transport_config: serde_json::Value,
    pub runtime: Option<String>,
    /// What this agent is for, in the operator's own words. Routing reads it,
    /// so an agent that declares nothing can only be matched on its name.
    pub description: Option<String>,
    pub created_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateThread {
    pub thread_id: ConversationId,
    pub primary_work_id: WorkItemId,
    pub room: RoomRef,
    pub title: String,
    pub goal: Option<String>,
    pub user_id: String,
    pub initial_agents: Vec<AgentRef>,
    pub created_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreatedThread {
    pub thread_id: ConversationId,
    pub primary_work_id: WorkItemId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AddThreadMember {
    pub thread_id: ConversationId,
    pub agent: AgentRef,
    pub changed_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoveThreadMember {
    pub thread_id: ConversationId,
    pub agent: AgentRef,
    pub changed_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenThreadForAgent {
    pub thread_id: ConversationId,
    pub agent_id: AgentId,
    pub opened_at: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OpenedThread {
    pub thread_id: ConversationId,
    pub room_id: RoomId,
    pub agent_id: AgentId,
    pub session_binding_id: SessionBindingId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MentionThreadAgent {
    pub thread_id: ConversationId,
    pub source_agent_id: AgentId,
    pub target_agent_id: AgentId,
    pub message_id: MessageId,
    pub body: String,
    pub capsule: String,
    pub mentioned_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetryThreadMention {
    pub message_id: MessageId,
    pub target_agent_id: AgentId,
    pub retried_at: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MentionedThreadAgent {
    pub opened: OpenedThread,
    pub membership_changed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ThreadMentionOutcome {
    Delivered(MentionedThreadAgent),
    PersistedFailed(CollaborationError),
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum CollaborationError {
    #[error("room {0} does not exist")]
    RoomNotFound(String),
    #[error("agent {0} does not exist")]
    AgentNotFound(String),
    #[error("thread {0} does not exist")]
    ThreadNotFound(ConversationId),
    #[error("room {0} is not active")]
    RoomInactive(RoomId),
    #[error("agent {0} is not active")]
    AgentInactive(AgentId),
    #[error("thread {0} is not open")]
    ThreadNotOpen(ConversationId),
    #[error("room id {0} already exists")]
    RoomIdConflict(RoomId),
    #[error("room name {0} already exists")]
    RoomNameConflict(String),
    #[error("agent name {0} already exists")]
    AgentNameConflict(String),
    #[error("thread id {0} already exists")]
    ThreadIdConflict(ConversationId),
    #[error("primary work id {0} already exists")]
    PrimaryWorkIdConflict(WorkItemId),
    #[error("room message id {0} already exists with different content")]
    RoomMessageIdConflict(RoomMessageId),
    #[error("room message reply {0} does not exist")]
    RoomMessageReplyNotFound(RoomMessageId),
    #[error("room message reply {reply_to} does not belong to room {room_id}")]
    RoomMessageReplyNotInRoom {
        room_id: RoomId,
        reply_to: RoomMessageId,
    },
    #[error("room user sender must be the trusted local user: {0}")]
    UntrustedRoomUserSender(String),
    #[error("agent {agent_id} must be an active member of room {room_id}")]
    RoomMembershipRequired { room_id: RoomId, agent_id: AgentId },
    #[error("agent {agent_id} must be an active member of thread {thread_id}")]
    ThreadMembershipRequired {
        thread_id: ConversationId,
        agent_id: AgentId,
    },
    #[error("agent {agent_id} still has an active thread membership in room {room_id}")]
    RoomRemovalBlocked { room_id: RoomId, agent_id: AgentId },
    #[error("invalid collaboration command: {0}")]
    InvalidCommand(String),
    #[error("thread mentions require a runtime bound to the target agent")]
    AgentTargetNotBound,
    #[error("a Thread is already open in this runtime")]
    ThreadAlreadyOpen,
    #[error("thread chat is not open")]
    ChatNotOpen,
    #[error("thread message content cannot be blank")]
    EmptyMessage,
    #[error("agent completed an empty message")]
    EmptyAgentMessage,
    #[error("transport event does not belong to the open thread")]
    SessionMismatch,
    #[error("permission request {0} is not pending")]
    PermissionRequestNotFound(String),
    #[error("Thread context is stopped")]
    ContextStopped,
    #[error("the durable Agent session was lost")]
    SessionLost,
    #[error("the durable Agent session is unavailable with status {0}")]
    SessionUnavailable(SessionBindingStatus),
    #[error("session binding {0} is already attached to this runtime owner")]
    SessionAlreadyAttached(SessionBindingId),
    #[error("delivery state recording failed: {primary}; FAILED recovery also failed: {recovery}")]
    DeliveryStateRecoveryFailed {
        primary: Box<CollaborationError>,
        recovery: Box<CollaborationError>,
    },
    #[error("collaboration runtime failed: {0}")]
    Runtime(String),
}

#[allow(async_fn_in_trait)]
pub trait ThreadRuntime {
    async fn open_thread_for_agent(
        &mut self,
        command: OpenThreadForAgent,
    ) -> Result<OpenedThread, CollaborationError>;

    async fn mention_thread_agent(
        &mut self,
        command: MentionThreadAgent,
    ) -> Result<Option<ThreadMentionOutcome>, CollaborationError>;

    async fn retry_thread_mention(
        &mut self,
        command: RetryThreadMention,
    ) -> Result<Option<ThreadMentionOutcome>, CollaborationError>;

    async fn shutdown(&mut self, stopped_at: String) -> Result<(), CollaborationError>;
}

#[allow(async_fn_in_trait)]
pub trait CollaborationRuntime {
    async fn create_room(&mut self, room: Room) -> Result<(), CollaborationError>;
    async fn get_room(&mut self, room_id: RoomId) -> Result<Option<Room>, CollaborationError>;
    async fn get_room_by_name(&mut self, name: String) -> Result<Option<Room>, CollaborationError>;
    async fn list_rooms(&mut self) -> Result<Vec<Room>, CollaborationError>;
    async fn append_room_message(
        &mut self,
        message: RoomMessage,
    ) -> Result<RoomMessage, CollaborationError>;
    async fn list_recent_room_messages(
        &mut self,
        room_id: RoomId,
        limit: usize,
    ) -> Result<(Vec<RoomMessage>, bool), CollaborationError>;
    async fn get_agent(&mut self, agent_id: AgentId) -> Result<Option<Agent>, CollaborationError>;
    async fn list_agents(&mut self) -> Result<Vec<Agent>, CollaborationError>;
    async fn create_agent(&mut self, agent: Agent) -> Result<(), CollaborationError>;
    async fn update_agent(&mut self, agent: Agent) -> Result<bool, CollaborationError>;
    async fn list_work_items(
        &mut self,
        conversation_id: ConversationId,
    ) -> Result<Vec<WorkItem>, CollaborationError>;
    async fn list_work_results(
        &mut self,
        conversation_id: ConversationId,
    ) -> Result<Vec<WorkResult>, CollaborationError>;
    async fn get_agent_by_name(
        &mut self,
        name: String,
    ) -> Result<Option<Agent>, CollaborationError>;
    async fn list_room_members(
        &mut self,
        room_id: RoomId,
    ) -> Result<Vec<RoomMember>, CollaborationError>;
    async fn add_room_member(
        &mut self,
        room_id: RoomId,
        agent_id: AgentId,
        role: Option<String>,
        changed_at: String,
    ) -> Result<MembershipChange, CollaborationError>;
    async fn remove_room_member(
        &mut self,
        room_id: RoomId,
        agent_id: AgentId,
        changed_at: String,
    ) -> Result<MembershipChange, CollaborationError>;
    async fn create_thread(
        &mut self,
        thread: Conversation,
        primary_work_id: WorkItemId,
        user_id: String,
        initial_agents: Vec<AgentId>,
    ) -> Result<WorkItem, CollaborationError>;
    async fn list_threads(
        &mut self,
        room_id: RoomId,
    ) -> Result<Vec<Conversation>, CollaborationError>;
    async fn list_thread_members(
        &mut self,
        thread_id: ConversationId,
    ) -> Result<Vec<ConversationMember>, CollaborationError>;
    async fn add_thread_member(
        &mut self,
        thread_id: ConversationId,
        agent_id: AgentId,
        changed_at: String,
    ) -> Result<MembershipChange, CollaborationError>;
    async fn remove_thread_member(
        &mut self,
        thread_id: ConversationId,
        agent_id: AgentId,
        changed_at: String,
    ) -> Result<MembershipChange, CollaborationError>;
}

pub struct CollaborationService<R> {
    runtime: R,
}

impl<R: CollaborationRuntime> CollaborationService<R> {
    pub fn new(runtime: R) -> Self {
        Self { runtime }
    }

    pub fn into_runtime(self) -> R {
        self.runtime
    }

    pub async fn create_room(&mut self, command: CreateRoom) -> Result<RoomId, CollaborationError> {
        let room = Room {
            id: command.room_id,
            name: command.name,
            description: command.description,
            status: "active".into(),
            created_at: command.created_at.clone(),
            updated_at: command.created_at,
        };
        let id = room.id;
        self.runtime.create_room(room).await?;
        Ok(id)
    }

    pub async fn list_rooms(&mut self) -> Result<Vec<Room>, CollaborationError> {
        self.runtime.list_rooms().await
    }

    pub async fn append_room_message(
        &mut self,
        command: AppendRoomMessage,
    ) -> Result<RoomMessage, CollaborationError> {
        self.runtime.append_room_message(command.message).await
    }

    /// Resolve parsed mention names without changing the shared message body.
    /// Storage validates all targets and the sender in the append transaction.
    /// This only persists intent; replaying it does not authorize runtime delivery.
    pub async fn append_room_message_with_mentions(
        &mut self,
        mut command: AppendRoomMessage,
        names: &[String],
    ) -> Result<RoomMessage, CollaborationError> {
        let mut targets = Vec::with_capacity(names.len());
        for name in names {
            let agent = self.resolve_agent(AgentRef::Name(name.clone())).await?;
            if !targets.contains(&agent.id) {
                targets.push(agent.id);
            }
        }
        command.message.mentions = targets;
        self.append_room_message(command).await
    }

    pub async fn list_recent_room_messages(
        &mut self,
        room_id: RoomId,
        limit: usize,
    ) -> Result<(Vec<RoomMessage>, bool), CollaborationError> {
        self.runtime.list_recent_room_messages(room_id, limit).await
    }

    pub async fn list_agents(&mut self) -> Result<Vec<Agent>, CollaborationError> {
        self.runtime.list_agents().await
    }

    pub async fn list_work_items(
        &mut self,
        conversation_id: ConversationId,
    ) -> Result<Vec<WorkItem>, CollaborationError> {
        self.runtime.list_work_items(conversation_id).await
    }

    pub async fn list_work_results(
        &mut self,
        conversation_id: ConversationId,
    ) -> Result<Vec<WorkResult>, CollaborationError> {
        self.runtime.list_work_results(conversation_id).await
    }

    pub async fn list_room_members(
        &mut self,
        room: RoomRef,
    ) -> Result<Vec<RoomMember>, CollaborationError> {
        let room = self.resolve_room(room).await?;
        self.runtime.list_room_members(room.id).await
    }

    pub async fn add_room_member(
        &mut self,
        command: AddRoomMember,
    ) -> Result<MembershipChange, CollaborationError> {
        let room = self.resolve_room(command.room).await?;
        let agent = self.resolve_agent(command.agent).await?;
        self.runtime
            .add_room_member(room.id, agent.id, command.role, command.changed_at)
            .await
    }

    pub async fn remove_room_member(
        &mut self,
        command: RemoveRoomMember,
    ) -> Result<MembershipChange, CollaborationError> {
        let room = self.resolve_room(command.room).await?;
        let agent = self.resolve_agent(command.agent).await?;
        self.runtime
            .remove_room_member(room.id, agent.id, command.changed_at)
            .await
    }

    pub async fn create_thread(
        &mut self,
        command: CreateThread,
    ) -> Result<CreatedThread, CollaborationError> {
        let room = self.resolve_room(command.room).await?;
        let mut agent_ids = Vec::with_capacity(command.initial_agents.len());
        for agent in command.initial_agents {
            agent_ids.push(self.resolve_agent(agent).await?.id);
        }
        let thread = Conversation {
            id: command.thread_id,
            kind: ConversationKind::Thread,
            room_id: Some(room.id),
            title: Some(command.title),
            goal: command.goal,
            parent_conversation_id: None,
            origin_conversation_id: None,
            status: "open".into(),
            created_at: command.created_at.clone(),
            updated_at: command.created_at,
        };
        let work = self
            .runtime
            .create_thread(thread, command.primary_work_id, command.user_id, agent_ids)
            .await?;
        Ok(CreatedThread {
            thread_id: command.thread_id,
            primary_work_id: work.id,
        })
    }

    pub async fn list_threads(
        &mut self,
        room: RoomRef,
    ) -> Result<Vec<Conversation>, CollaborationError> {
        let room = self.resolve_room(room).await?;
        self.runtime.list_threads(room.id).await
    }

    pub async fn list_thread_members(
        &mut self,
        thread_id: ConversationId,
    ) -> Result<Vec<ConversationMember>, CollaborationError> {
        self.runtime.list_thread_members(thread_id).await
    }

    pub async fn add_thread_member(
        &mut self,
        command: AddThreadMember,
    ) -> Result<MembershipChange, CollaborationError> {
        let agent = self.resolve_agent(command.agent).await?;
        self.runtime
            .add_thread_member(command.thread_id, agent.id, command.changed_at)
            .await
    }

    pub async fn remove_thread_member(
        &mut self,
        command: RemoveThreadMember,
    ) -> Result<MembershipChange, CollaborationError> {
        let agent = self.resolve_agent(command.agent).await?;
        self.runtime
            .remove_thread_member(command.thread_id, agent.id, command.changed_at)
            .await
    }

    pub async fn resolve_room(&mut self, reference: RoomRef) -> Result<Room, CollaborationError> {
        let (found, display) = match reference {
            RoomRef::Id(id) => (self.runtime.get_room(id).await?, id.to_string()),
            RoomRef::Name(name) => (self.runtime.get_room_by_name(name.clone()).await?, name),
        };
        found.ok_or(CollaborationError::RoomNotFound(display))
    }

    pub async fn resolve_agent(
        &mut self,
        reference: AgentRef,
    ) -> Result<Agent, CollaborationError> {
        let (found, display) = match reference {
            AgentRef::Id(id) => (self.runtime.get_agent(id).await?, id.to_string()),
            AgentRef::Name(name) => (self.runtime.get_agent_by_name(name.clone()).await?, name),
        };
        found.ok_or(CollaborationError::AgentNotFound(display))
    }

    /// Onboard a project-owned agent. This creates a persistent logical Agent
    /// identity bound to a project; it starts no AgentSession and grants no
    /// Room membership.
    pub async fn add_agent(&mut self, command: AddAgent) -> Result<Agent, CollaborationError> {
        if self
            .runtime
            .get_agent_by_name(command.name.clone())
            .await?
            .is_some()
        {
            return Err(CollaborationError::AgentNameConflict(command.name));
        }
        let mut fields = serde_json::Map::new();
        if let Some(runtime) = &command.runtime {
            fields.insert("runtime".to_owned(), serde_json::json!(runtime));
        }
        if let Some(description) = command
            .description
            .as_deref()
            .map(str::trim)
            .filter(|description| !description.is_empty())
        {
            fields.insert("description".to_owned(), serde_json::json!(description));
        }
        let metadata = serde_json::Value::Object(fields);
        let agent = Agent {
            id: command.agent_id,
            name: command.name,
            project_root: command.project_root,
            transport_type: command.transport_type,
            transport_config: command.transport_config,
            status: "active".into(),
            metadata,
            created_at: command.created_at.clone(),
            updated_at: command.created_at,
        };
        agent
            .validate()
            .map_err(|error| CollaborationError::InvalidCommand(error.to_string()))?;
        self.runtime.create_agent(agent.clone()).await?;
        Ok(agent)
    }

    /// Retire an agent identity. Rooms, Threads, and transcripts are left
    /// untouched; only the agent stops being active.
    pub async fn remove_agent(
        &mut self,
        reference: AgentRef,
        changed_at: String,
    ) -> Result<Agent, CollaborationError> {
        let mut agent = self.resolve_agent(reference).await?;
        agent.status = "inactive".into();
        agent.updated_at = changed_at;
        self.runtime.update_agent(agent.clone()).await?;
        Ok(agent)
    }

    /// Replace an existing agent's transport without changing its identity.
    pub async fn set_agent_transport(
        &mut self,
        reference: AgentRef,
        transport_type: String,
        transport_config: serde_json::Value,
        changed_at: String,
    ) -> Result<Agent, CollaborationError> {
        let mut agent = self.resolve_agent(reference).await?;
        agent.transport_type = transport_type;
        agent.transport_config = transport_config;
        agent.updated_at = changed_at;
        agent
            .validate()
            .map_err(|error| CollaborationError::InvalidCommand(error.to_string()))?;
        self.runtime.update_agent(agent.clone()).await?;
        Ok(agent)
    }
}
