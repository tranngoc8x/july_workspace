use crate::domain::{
    Agent, AgentId, Conversation, ConversationId, ConversationKind, ConversationMember, MessageId,
    Room, RoomId, RoomMember, SessionBindingId, SessionBindingStatus, WorkItem, WorkItemId,
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateRoom {
    pub room_id: RoomId,
    pub name: String,
    pub description: Option<String>,
    pub created_at: String,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MentionedThreadAgent {
    pub opened: OpenedThread,
    pub membership_changed: bool,
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
    #[error("thread id {0} already exists")]
    ThreadIdConflict(ConversationId),
    #[error("primary work id {0} already exists")]
    PrimaryWorkIdConflict(WorkItemId),
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
    #[error("Thread context is stopped")]
    ContextStopped,
    #[error("the durable Agent session was lost")]
    SessionLost,
    #[error("the durable Agent session is unavailable with status {0}")]
    SessionUnavailable(SessionBindingStatus),
    #[error("session binding {0} is already attached to this runtime owner")]
    SessionAlreadyAttached(SessionBindingId),
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
    ) -> Result<Option<MentionedThreadAgent>, CollaborationError>;

    async fn shutdown(&mut self, stopped_at: String) -> Result<(), CollaborationError>;
}

#[allow(async_fn_in_trait)]
pub trait CollaborationRuntime {
    async fn create_room(&mut self, room: Room) -> Result<(), CollaborationError>;
    async fn get_room(&mut self, room_id: RoomId) -> Result<Option<Room>, CollaborationError>;
    async fn get_room_by_name(&mut self, name: String) -> Result<Option<Room>, CollaborationError>;
    async fn list_rooms(&mut self) -> Result<Vec<Room>, CollaborationError>;
    async fn get_agent(&mut self, agent_id: AgentId) -> Result<Option<Agent>, CollaborationError>;
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

    async fn resolve_room(&mut self, reference: RoomRef) -> Result<Room, CollaborationError> {
        let (found, display) = match reference {
            RoomRef::Id(id) => (self.runtime.get_room(id).await?, id.to_string()),
            RoomRef::Name(name) => (self.runtime.get_room_by_name(name.clone()).await?, name),
        };
        found.ok_or(CollaborationError::RoomNotFound(display))
    }

    async fn resolve_agent(&mut self, reference: AgentRef) -> Result<Agent, CollaborationError> {
        let (found, display) = match reference {
            AgentRef::Id(id) => (self.runtime.get_agent(id).await?, id.to_string()),
            AgentRef::Name(name) => (self.runtime.get_agent_by_name(name.clone()).await?, name),
        };
        found.ok_or(CollaborationError::AgentNotFound(display))
    }
}
