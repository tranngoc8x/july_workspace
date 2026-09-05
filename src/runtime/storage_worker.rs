use super::{RuntimeError, timestamp};
use crate::application::{
    BuildRecoveryCapsule, CollaborationError, CollaborationRuntime, DeliberationError,
    DeliberationRuntime, DependencyError, DependencyOutcome, DependencyRuntime,
    FailedMessageDelivery, MembershipChange, MembershipState, PublishError, PublishRuntime,
    PublishedResult, RecoveryCapsule, RecoveryError, RecoveryInput, RecoveryRuntime, WorkError,
    WorkRuntime, format_recovery_capsule,
};
use crate::domain::{
    Agent, AgentId, Checkpoint, Conversation, ConversationId, ConversationMember, Decision,
    DecisionId, DecisionOutcome, DecisionOwner, DecisionWork, Handoff, HandoffChallenge, HandoffId,
    HandoffResponse, MemberType, Memory, MemoryKind, MemoryScopeType, Message, MessageDelivery,
    MessageId, PermissionDecision, Proposal, ProposalId, ProposalResponse, Publish, PublishId,
    ResultId, Room, RoomId, RoomMember, SessionBinding, SessionBindingId, SessionBindingStatus,
    SessionRecovery, WorkDependency, WorkItem, WorkItemId, WorkResult, WorkStatus,
};
use crate::storage::{SqliteStore, StoreError};
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;
use tokio::sync::{mpsc, oneshot};

const STORAGE_CAPACITY: usize = 64;

type Reply<T> = oneshot::Sender<Result<T, StoreError>>;

enum Command {
    GetAgent(AgentId, Reply<Option<Agent>>),
    GetAgentByName(String, Reply<Option<Agent>>),
    CreateRoom(Room, Reply<()>),
    GetRoom(RoomId, Reply<Option<Room>>),
    GetRoomByName(String, Reply<Option<Room>>),
    ListRooms(Reply<Vec<Room>>),
    ListAgents(Reply<Vec<Agent>>),
    CreateAgent(Box<Agent>, Reply<()>),
    UpdateAgent(Box<Agent>, Reply<bool>),
    ListWorkItems(ConversationId, Reply<Vec<WorkItem>>),
    ListWorkResults(ConversationId, Reply<Vec<WorkResult>>),
    ListPublishTargets(ConversationId, Reply<Vec<ConversationId>>),
    ListRoomMembers(RoomId, Reply<Vec<RoomMember>>),
    AddRoomMember(RoomId, AgentId, Option<String>, String, Reply<bool>),
    RemoveRoomMember(RoomId, AgentId, String, Reply<bool>),
    CreateThread(
        Conversation,
        WorkItemId,
        String,
        Vec<AgentId>,
        Reply<WorkItem>,
    ),
    ListThreads(RoomId, Reply<Vec<Conversation>>),
    ListThreadMembers(ConversationId, Reply<Vec<ConversationMember>>),
    AddThreadMember(ConversationId, AgentId, String, Reply<bool>),
    RemoveThreadMember(ConversationId, AgentId, String, Reply<bool>),
    AssignWorkOwner(WorkItemId, AgentId, String, Reply<WorkItem>),
    TransitionWork(WorkItemId, WorkStatus, String, Reply<WorkItem>),
    CreateWorkResult(WorkResult, Reply<WorkResult>),
    AddWorkDependency(WorkItemId, WorkItemId, String, Reply<WorkDependency>),
    ListWorkDependencyOutcomesForDownstream(
        WorkItemId,
        Reply<Vec<(WorkDependency, Option<WorkResult>)>>,
    ),
    PublishResult(
        PublishId,
        ResultId,
        ConversationId,
        String,
        Reply<(Publish, WorkResult)>,
    ),
    ListPublishedResults(ConversationId, Reply<Vec<(Publish, WorkResult)>>),
    ProposeHandoff(Box<Handoff>, Reply<Handoff>),
    RespondToHandoff(HandoffId, Box<HandoffResponse>, String, Reply<Handoff>),
    ChallengeHandoff(
        HandoffId,
        Box<HandoffChallenge>,
        String,
        Reply<(Handoff, Option<Decision>)>,
    ),
    ResolveHandoff(HandoffId, AgentId, String, Reply<Handoff>),
    CreateProposal(Box<Proposal>, Reply<Proposal>),
    RespondToProposal(Box<ProposalResponse>, Reply<ProposalResponse>),
    WithdrawProposal(ProposalId, AgentId, String, Reply<Proposal>),
    RecordDecision(Box<Decision>, Reply<Decision>),
    DecideDecision(DecisionId, Box<DecisionOutcome>, String, Reply<Decision>),
    CancelDecision(DecisionId, DecisionOwner, String, String, Reply<Decision>),
    ListPendingDecisions(Reply<Vec<Decision>>),
    ConvertDecisionToWork(DecisionId, Vec<DecisionWork>, String, Reply<Vec<WorkItem>>),
    AdmitThreadSession(
        ConversationId,
        AgentId,
        String,
        Reply<(Agent, Conversation, Option<SessionBinding>)>,
    ),
    GetOrCreateDm(
        String,
        AgentId,
        String,
        oneshot::Sender<Result<Conversation, StoreError>>,
    ),
    GetOrCreateAgentDm(AgentId, AgentId, String, Reply<Conversation>),
    InsertMessage(Message, oneshot::Sender<Result<(), StoreError>>),
    PersistAgentDirectMessage(
        MessageId,
        AgentId,
        AgentId,
        String,
        String,
        Reply<Option<(Message, MessageDelivery)>>,
    ),
    PersistThreadMention(
        Message,
        AgentId,
        AgentId,
        String,
        Reply<Option<(bool, MessageDelivery)>>,
    ),
    MarkDeliveryCapsuleDelivered(MessageId, AgentId, String, Reply<bool>),
    MarkDeliveryDelivered(MessageId, AgentId, String, Reply<bool>),
    MarkDeliveryFailed(MessageId, AgentId, String, Reply<bool>),
    ListFailedMessageDeliveries(Reply<Vec<FailedMessageDelivery>>),
    ClaimThreadMentionRetry(
        MessageId,
        AgentId,
        String,
        Reply<Option<(Message, MessageDelivery)>>,
    ),
    ClaimAgentDirectMessageRetry(
        MessageId,
        AgentId,
        String,
        Reply<Option<(Message, MessageDelivery)>>,
    ),
    ListMessages(
        ConversationId,
        oneshot::Sender<Result<Vec<Message>, StoreError>>,
    ),
    ListRecentMessages(ConversationId, usize, Reply<(Vec<Message>, bool)>),
    InsertCheckpoint(Checkpoint, oneshot::Sender<Result<(), StoreError>>),
    GetLatestCheckpoint(
        ConversationId,
        AgentId,
        oneshot::Sender<Result<Option<Checkpoint>, StoreError>>,
    ),
    PromoteMemory(Memory, Reply<()>),
    ListMemories(
        MemoryScopeType,
        String,
        Option<MemoryKind>,
        Reply<Vec<Memory>>,
    ),
    BuildRecoveryCapsule(
        BuildRecoveryCapsule,
        oneshot::Sender<Result<RecoveryCapsule, RecoveryError>>,
    ),
    InsertBinding(SessionBinding, oneshot::Sender<Result<(), StoreError>>),
    GetCurrentBinding(
        ConversationId,
        AgentId,
        oneshot::Sender<Result<Option<SessionBinding>, StoreError>>,
    ),
    GetLatestBinding(
        ConversationId,
        AgentId,
        oneshot::Sender<Result<Option<SessionBinding>, StoreError>>,
    ),
    ListCurrentBindings(
        AgentId,
        oneshot::Sender<Result<Vec<SessionBinding>, StoreError>>,
    ),
    UpdateBindingStatus(
        SessionBindingId,
        SessionBindingStatus,
        String,
        oneshot::Sender<Result<bool, StoreError>>,
    ),
    MarkDisconnected(
        SessionBindingId,
        String,
        oneshot::Sender<Result<bool, StoreError>>,
    ),
    BeginSessionReplacement(
        SessionBindingId,
        SessionBindingId,
        String,
        String,
        Reply<(SessionBinding, SessionRecovery)>,
    ),
    GetSessionRecovery(SessionBindingId, Reply<Option<SessionRecovery>>),
    AttachReplacementRemoteSession(SessionBindingId, String, String, Reply<SessionBinding>),
    MarkSessionRecoveryCapsuleDelivered(SessionBindingId, String, Reply<bool>),
    InsertPermission(PermissionDecision, oneshot::Sender<Result<(), StoreError>>),
    GetPermission(
        String,
        oneshot::Sender<Result<Option<PermissionDecision>, StoreError>>,
    ),
    Shutdown(Option<oneshot::Sender<()>>),
}

#[derive(Clone)]
pub struct StorageHandle {
    commands: mpsc::Sender<Command>,
}

pub struct StorageWorker {
    handle: StorageHandle,
    thread: Option<JoinHandle<()>>,
}

impl StorageWorker {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, RuntimeError> {
        Self::open_inner(path, true)
    }

    pub(crate) fn open_for_inspection(path: impl AsRef<Path>) -> Result<Self, RuntimeError> {
        Self::open_inner(path, false)
    }

    fn open_inner(path: impl AsRef<Path>, reconcile: bool) -> Result<Self, RuntimeError> {
        let path = PathBuf::from(path.as_ref());
        let (commands, receiver) = mpsc::channel(STORAGE_CAPACITY);
        let (started, ready) = std::sync::mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            let store = match SqliteStore::open(path) {
                Ok(mut store) => {
                    if reconcile
                        && let Err(error) = store.reconcile_pending_deliveries(&timestamp())
                    {
                        let _ = started.send(Err(error));
                        return;
                    }
                    let _ = started.send(Ok(()));
                    store
                }
                Err(error) => {
                    let _ = started.send(Err(error));
                    return;
                }
            };
            run(store, receiver);
        });
        match ready.recv().map_err(|_| RuntimeError::ChannelClosed)? {
            Ok(()) => {}
            Err(error) => {
                thread
                    .join()
                    .map_err(|_| RuntimeError::StorageWorkerPanicked)?;
                return Err(error.into());
            }
        }
        Ok(Self {
            handle: StorageHandle { commands },
            thread: Some(thread),
        })
    }

    pub(crate) fn handle(&self) -> StorageHandle {
        self.handle.clone()
    }

    pub async fn list_failed_message_deliveries(
        &self,
    ) -> Result<Vec<FailedMessageDelivery>, RuntimeError> {
        self.handle.list_failed_message_deliveries().await
    }

    pub async fn shutdown(&mut self) -> Result<(), RuntimeError> {
        let (done, stopped) = oneshot::channel();
        self.handle
            .commands
            .send(Command::Shutdown(Some(done)))
            .await
            .map_err(|_| RuntimeError::ChannelClosed)?;
        stopped.await.map_err(|_| RuntimeError::ChannelClosed)?;
        self.join()
    }

    fn join(&mut self) -> Result<(), RuntimeError> {
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| RuntimeError::StorageWorkerPanicked)?;
        }
        Ok(())
    }
}

impl StorageHandle {
    pub async fn get_agent(&self, id: AgentId) -> Result<Option<Agent>, RuntimeError> {
        self.request(|reply| Command::GetAgent(id, reply)).await
    }

    pub async fn get_agent_by_name(&self, name: String) -> Result<Option<Agent>, RuntimeError> {
        self.request(|reply| Command::GetAgentByName(name, reply))
            .await
    }

    pub(crate) async fn admit_thread_session(
        &self,
        thread_id: ConversationId,
        agent_id: AgentId,
        admitted_at: String,
    ) -> Result<(Agent, Conversation, Option<SessionBinding>), CollaborationError> {
        self.collaboration_request(|reply| {
            Command::AdmitThreadSession(thread_id, agent_id, admitted_at, reply)
        })
        .await
    }

    pub async fn get_or_create_dm(
        &self,
        user_id: String,
        agent_id: AgentId,
        now: String,
    ) -> Result<Conversation, RuntimeError> {
        self.request(|reply| Command::GetOrCreateDm(user_id, agent_id, now, reply))
            .await
    }

    pub async fn get_or_create_agent_dm(
        &self,
        source_agent_id: AgentId,
        target_agent_id: AgentId,
        now: String,
    ) -> Result<Conversation, RuntimeError> {
        self.request(|reply| {
            Command::GetOrCreateAgentDm(source_agent_id, target_agent_id, now, reply)
        })
        .await
    }

    pub async fn insert_message(&self, message: Message) -> Result<(), RuntimeError> {
        self.request(|reply| Command::InsertMessage(message, reply))
            .await
    }

    pub(crate) async fn persist_agent_direct_message(
        &self,
        message_id: MessageId,
        source_agent_id: AgentId,
        target_agent_id: AgentId,
        body: String,
        sent_at: String,
    ) -> Result<Option<(Message, MessageDelivery)>, RuntimeError> {
        self.request(|reply| {
            Command::PersistAgentDirectMessage(
                message_id,
                source_agent_id,
                target_agent_id,
                body,
                sent_at,
                reply,
            )
        })
        .await
    }

    pub async fn persist_thread_mention(
        &self,
        message: Message,
        source_agent_id: AgentId,
        target_agent_id: AgentId,
        capsule: String,
    ) -> Result<Option<(bool, MessageDelivery)>, CollaborationError> {
        self.collaboration_request(|reply| {
            Command::PersistThreadMention(message, source_agent_id, target_agent_id, capsule, reply)
        })
        .await
    }

    pub(crate) async fn mark_delivery_capsule_delivered(
        &self,
        message_id: MessageId,
        target_agent_id: AgentId,
        delivered_at: String,
    ) -> Result<bool, CollaborationError> {
        self.collaboration_request(|reply| {
            Command::MarkDeliveryCapsuleDelivered(message_id, target_agent_id, delivered_at, reply)
        })
        .await
    }

    pub(crate) async fn mark_delivery_delivered(
        &self,
        message_id: MessageId,
        target_agent_id: AgentId,
        delivered_at: String,
    ) -> Result<bool, CollaborationError> {
        self.collaboration_request(|reply| {
            Command::MarkDeliveryDelivered(message_id, target_agent_id, delivered_at, reply)
        })
        .await
    }

    pub(crate) async fn mark_delivery_failed(
        &self,
        message_id: MessageId,
        target_agent_id: AgentId,
        failed_at: String,
    ) -> Result<bool, CollaborationError> {
        self.collaboration_request(|reply| {
            Command::MarkDeliveryFailed(message_id, target_agent_id, failed_at, reply)
        })
        .await
    }

    pub async fn list_failed_message_deliveries(
        &self,
    ) -> Result<Vec<FailedMessageDelivery>, RuntimeError> {
        self.request(Command::ListFailedMessageDeliveries).await
    }

    pub async fn list_pending_decisions(&self) -> Result<Vec<Decision>, DeliberationError> {
        self.deliberation_request(Command::ListPendingDecisions)
            .await
    }

    pub(crate) async fn claim_thread_mention_retry(
        &self,
        message_id: MessageId,
        target_agent_id: AgentId,
        claimed_at: String,
    ) -> Result<Option<(Message, MessageDelivery)>, CollaborationError> {
        self.collaboration_request(|reply| {
            Command::ClaimThreadMentionRetry(message_id, target_agent_id, claimed_at, reply)
        })
        .await
    }

    pub(crate) async fn claim_agent_direct_message_retry(
        &self,
        message_id: MessageId,
        target_agent_id: AgentId,
        claimed_at: String,
    ) -> Result<Option<(Message, MessageDelivery)>, RuntimeError> {
        self.request(|reply| {
            Command::ClaimAgentDirectMessageRetry(message_id, target_agent_id, claimed_at, reply)
        })
        .await
    }

    pub async fn list_messages(
        &self,
        conversation_id: ConversationId,
    ) -> Result<Vec<Message>, RuntimeError> {
        self.request(|reply| Command::ListMessages(conversation_id, reply))
            .await
    }

    pub(crate) async fn list_recent_messages(
        &self,
        conversation_id: ConversationId,
        limit: usize,
    ) -> Result<(Vec<Message>, bool), RuntimeError> {
        self.request(|reply| Command::ListRecentMessages(conversation_id, limit, reply))
            .await
    }

    pub async fn insert_checkpoint(&self, checkpoint: Checkpoint) -> Result<(), RuntimeError> {
        self.request(|reply| Command::InsertCheckpoint(checkpoint, reply))
            .await
    }

    pub async fn get_latest_checkpoint(
        &self,
        conversation_id: ConversationId,
        agent_id: AgentId,
    ) -> Result<Option<Checkpoint>, RuntimeError> {
        self.request(|reply| Command::GetLatestCheckpoint(conversation_id, agent_id, reply))
            .await
    }

    pub async fn promote_memory(&self, memory: Memory) -> Result<(), RuntimeError> {
        self.request(|reply| Command::PromoteMemory(memory, reply))
            .await
    }

    pub async fn list_memories(
        &self,
        scope_type: MemoryScopeType,
        scope_id: String,
        kind: Option<MemoryKind>,
    ) -> Result<Vec<Memory>, RuntimeError> {
        self.request(|reply| Command::ListMemories(scope_type, scope_id, kind, reply))
            .await
    }

    pub async fn insert_session_binding(
        &self,
        binding: SessionBinding,
    ) -> Result<(), RuntimeError> {
        self.request(|reply| Command::InsertBinding(binding, reply))
            .await
    }

    pub async fn get_current_session_binding(
        &self,
        conversation_id: ConversationId,
        agent_id: AgentId,
    ) -> Result<Option<SessionBinding>, RuntimeError> {
        self.request(|reply| Command::GetCurrentBinding(conversation_id, agent_id, reply))
            .await
    }

    pub async fn get_latest_session_binding(
        &self,
        conversation_id: ConversationId,
        agent_id: AgentId,
    ) -> Result<Option<SessionBinding>, RuntimeError> {
        self.request(|reply| Command::GetLatestBinding(conversation_id, agent_id, reply))
            .await
    }

    pub async fn list_current_session_bindings_for_agent(
        &self,
        agent_id: AgentId,
    ) -> Result<Vec<SessionBinding>, RuntimeError> {
        self.request(|reply| Command::ListCurrentBindings(agent_id, reply))
            .await
    }

    pub async fn update_session_binding_status(
        &self,
        id: SessionBindingId,
        status: SessionBindingStatus,
        last_used_at: String,
    ) -> Result<bool, RuntimeError> {
        self.request(|reply| Command::UpdateBindingStatus(id, status, last_used_at, reply))
            .await
    }

    pub async fn mark_binding_disconnected(
        &self,
        binding_id: SessionBindingId,
        last_used_at: String,
    ) -> Result<bool, RuntimeError> {
        self.request(|reply| Command::MarkDisconnected(binding_id, last_used_at, reply))
            .await
    }

    pub async fn begin_session_replacement(
        &self,
        source_binding_id: SessionBindingId,
        replacement_binding_id: SessionBindingId,
        capsule: String,
        replaced_at: String,
    ) -> Result<(SessionBinding, SessionRecovery), RuntimeError> {
        self.request(|reply| {
            Command::BeginSessionReplacement(
                source_binding_id,
                replacement_binding_id,
                capsule,
                replaced_at,
                reply,
            )
        })
        .await
    }

    pub async fn get_session_recovery(
        &self,
        session_binding_id: SessionBindingId,
    ) -> Result<Option<SessionRecovery>, RuntimeError> {
        self.request(|reply| Command::GetSessionRecovery(session_binding_id, reply))
            .await
    }

    pub async fn attach_replacement_remote_session(
        &self,
        session_binding_id: SessionBindingId,
        remote_session_id: String,
        attached_at: String,
    ) -> Result<SessionBinding, RuntimeError> {
        self.request(|reply| {
            Command::AttachReplacementRemoteSession(
                session_binding_id,
                remote_session_id,
                attached_at,
                reply,
            )
        })
        .await
    }

    pub async fn mark_session_recovery_capsule_delivered(
        &self,
        session_binding_id: SessionBindingId,
        delivered_at: String,
    ) -> Result<bool, RuntimeError> {
        self.request(|reply| {
            Command::MarkSessionRecoveryCapsuleDelivered(session_binding_id, delivered_at, reply)
        })
        .await
    }

    pub async fn insert_permission_decision(
        &self,
        decision: PermissionDecision,
    ) -> Result<(), RuntimeError> {
        self.request(|reply| Command::InsertPermission(decision, reply))
            .await
    }

    pub async fn get_permission_decision(
        &self,
        id: String,
    ) -> Result<Option<PermissionDecision>, RuntimeError> {
        self.request(|reply| Command::GetPermission(id, reply))
            .await
    }

    async fn request<R>(
        &self,
        build: impl FnOnce(oneshot::Sender<Result<R, StoreError>>) -> Command,
    ) -> Result<R, RuntimeError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(build(reply))
            .await
            .map_err(|_| RuntimeError::ChannelClosed)?;
        Ok(response.await.map_err(|_| RuntimeError::ChannelClosed)??)
    }

    async fn collaboration_request<R>(
        &self,
        build: impl FnOnce(Reply<R>) -> Command,
    ) -> Result<R, CollaborationError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(build(reply))
            .await
            .map_err(|_| CollaborationError::Runtime("storage owner channel closed".into()))?;
        response
            .await
            .map_err(|_| CollaborationError::Runtime("storage owner channel closed".into()))?
            .map_err(map_store_error)
    }

    async fn deliberation_request<R>(
        &self,
        build: impl FnOnce(Reply<R>) -> Command,
    ) -> Result<R, DeliberationError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(build(reply))
            .await
            .map_err(|_| DeliberationError::Runtime("storage owner channel closed".into()))?;
        response
            .await
            .map_err(|_| DeliberationError::Runtime("storage owner channel closed".into()))?
            .map_err(map_deliberation_error)
    }

    async fn work_request<R>(
        &self,
        build: impl FnOnce(Reply<R>) -> Command,
    ) -> Result<R, WorkError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(build(reply))
            .await
            .map_err(|_| WorkError::Runtime("storage owner channel closed".into()))?;
        response
            .await
            .map_err(|_| WorkError::Runtime("storage owner channel closed".into()))?
            .map_err(map_work_error)
    }

    async fn dependency_request<R>(
        &self,
        build: impl FnOnce(Reply<R>) -> Command,
    ) -> Result<R, DependencyError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(build(reply))
            .await
            .map_err(|_| DependencyError::Runtime("storage owner channel closed".into()))?;
        response
            .await
            .map_err(|_| DependencyError::Runtime("storage owner channel closed".into()))?
            .map_err(map_dependency_error)
    }

    async fn publish_request<R>(
        &self,
        build: impl FnOnce(Reply<R>) -> Command,
    ) -> Result<R, PublishError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(build(reply))
            .await
            .map_err(|_| PublishError::Runtime("storage owner channel closed".into()))?;
        response
            .await
            .map_err(|_| PublishError::Runtime("storage owner channel closed".into()))?
            .map_err(map_publish_error)
    }

    pub(crate) async fn build_recovery_capsule(
        &self,
        command: BuildRecoveryCapsule,
    ) -> Result<RecoveryCapsule, RecoveryError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(Command::BuildRecoveryCapsule(command, reply))
            .await
            .map_err(|_| RecoveryError::Runtime("storage owner channel closed".into()))?;
        response
            .await
            .map_err(|_| RecoveryError::Runtime("storage owner channel closed".into()))?
    }
}

impl std::ops::Deref for StorageWorker {
    type Target = StorageHandle;

    fn deref(&self) -> &Self::Target {
        &self.handle
    }
}

impl CollaborationRuntime for StorageHandle {
    async fn create_room(&mut self, room: Room) -> Result<(), CollaborationError> {
        self.collaboration_request(|reply| Command::CreateRoom(room, reply))
            .await
    }

    async fn get_room(&mut self, room_id: RoomId) -> Result<Option<Room>, CollaborationError> {
        self.collaboration_request(|reply| Command::GetRoom(room_id, reply))
            .await
    }

    async fn get_room_by_name(&mut self, name: String) -> Result<Option<Room>, CollaborationError> {
        self.collaboration_request(|reply| Command::GetRoomByName(name, reply))
            .await
    }

    async fn list_rooms(&mut self) -> Result<Vec<Room>, CollaborationError> {
        self.collaboration_request(Command::ListRooms).await
    }

    async fn list_agents(&mut self) -> Result<Vec<Agent>, CollaborationError> {
        self.collaboration_request(Command::ListAgents).await
    }

    async fn create_agent(&mut self, agent: Agent) -> Result<(), CollaborationError> {
        self.collaboration_request(|reply| Command::CreateAgent(Box::new(agent), reply))
            .await
    }

    async fn update_agent(&mut self, agent: Agent) -> Result<bool, CollaborationError> {
        self.collaboration_request(|reply| Command::UpdateAgent(Box::new(agent), reply))
            .await
    }

    async fn list_work_items(
        &mut self,
        conversation_id: ConversationId,
    ) -> Result<Vec<WorkItem>, CollaborationError> {
        self.collaboration_request(|reply| Command::ListWorkItems(conversation_id, reply))
            .await
    }

    async fn list_work_results(
        &mut self,
        conversation_id: ConversationId,
    ) -> Result<Vec<WorkResult>, CollaborationError> {
        self.collaboration_request(|reply| Command::ListWorkResults(conversation_id, reply))
            .await
    }

    async fn get_agent(&mut self, agent_id: AgentId) -> Result<Option<Agent>, CollaborationError> {
        self.collaboration_request(|reply| Command::GetAgent(agent_id, reply))
            .await
    }

    async fn get_agent_by_name(
        &mut self,
        name: String,
    ) -> Result<Option<Agent>, CollaborationError> {
        self.collaboration_request(|reply| Command::GetAgentByName(name, reply))
            .await
    }

    async fn list_room_members(
        &mut self,
        room_id: RoomId,
    ) -> Result<Vec<RoomMember>, CollaborationError> {
        self.collaboration_request(|reply| Command::ListRoomMembers(room_id, reply))
            .await
    }

    async fn add_room_member(
        &mut self,
        room_id: RoomId,
        agent_id: AgentId,
        role: Option<String>,
        changed_at: String,
    ) -> Result<MembershipChange, CollaborationError> {
        let changed = self
            .collaboration_request(|reply| {
                Command::AddRoomMember(room_id, agent_id, role, changed_at, reply)
            })
            .await?;
        Ok(MembershipChange {
            state: MembershipState::Active,
            changed,
        })
    }

    async fn remove_room_member(
        &mut self,
        room_id: RoomId,
        agent_id: AgentId,
        changed_at: String,
    ) -> Result<MembershipChange, CollaborationError> {
        let changed = self
            .collaboration_request(|reply| {
                Command::RemoveRoomMember(room_id, agent_id, changed_at, reply)
            })
            .await?;
        Ok(MembershipChange {
            state: MembershipState::Left,
            changed,
        })
    }

    async fn create_thread(
        &mut self,
        thread: Conversation,
        primary_work_id: WorkItemId,
        user_id: String,
        initial_agents: Vec<AgentId>,
    ) -> Result<WorkItem, CollaborationError> {
        self.collaboration_request(|reply| {
            Command::CreateThread(thread, primary_work_id, user_id, initial_agents, reply)
        })
        .await
    }

    async fn list_threads(
        &mut self,
        room_id: RoomId,
    ) -> Result<Vec<Conversation>, CollaborationError> {
        self.collaboration_request(|reply| Command::ListThreads(room_id, reply))
            .await
    }

    async fn list_thread_members(
        &mut self,
        thread_id: ConversationId,
    ) -> Result<Vec<ConversationMember>, CollaborationError> {
        self.collaboration_request(|reply| Command::ListThreadMembers(thread_id, reply))
            .await
    }

    async fn add_thread_member(
        &mut self,
        thread_id: ConversationId,
        agent_id: AgentId,
        changed_at: String,
    ) -> Result<MembershipChange, CollaborationError> {
        let changed = self
            .collaboration_request(|reply| {
                Command::AddThreadMember(thread_id, agent_id, changed_at, reply)
            })
            .await?;
        Ok(MembershipChange {
            state: MembershipState::Active,
            changed,
        })
    }

    async fn remove_thread_member(
        &mut self,
        thread_id: ConversationId,
        agent_id: AgentId,
        changed_at: String,
    ) -> Result<MembershipChange, CollaborationError> {
        let changed = self
            .collaboration_request(|reply| {
                Command::RemoveThreadMember(thread_id, agent_id, changed_at, reply)
            })
            .await?;
        Ok(MembershipChange {
            state: MembershipState::Left,
            changed,
        })
    }
}

impl CollaborationRuntime for StorageWorker {
    async fn create_room(&mut self, room: Room) -> Result<(), CollaborationError> {
        self.handle.create_room(room).await
    }

    async fn get_room(&mut self, room_id: RoomId) -> Result<Option<Room>, CollaborationError> {
        self.handle.get_room(room_id).await
    }

    async fn get_room_by_name(&mut self, name: String) -> Result<Option<Room>, CollaborationError> {
        self.handle.get_room_by_name(name).await
    }

    async fn list_rooms(&mut self) -> Result<Vec<Room>, CollaborationError> {
        self.handle.list_rooms().await
    }

    async fn list_agents(&mut self) -> Result<Vec<Agent>, CollaborationError> {
        self.handle.list_agents().await
    }

    async fn create_agent(&mut self, agent: Agent) -> Result<(), CollaborationError> {
        self.handle.create_agent(agent).await
    }

    async fn update_agent(&mut self, agent: Agent) -> Result<bool, CollaborationError> {
        self.handle.update_agent(agent).await
    }

    async fn list_work_items(
        &mut self,
        conversation_id: ConversationId,
    ) -> Result<Vec<WorkItem>, CollaborationError> {
        self.handle.list_work_items(conversation_id).await
    }

    async fn list_work_results(
        &mut self,
        conversation_id: ConversationId,
    ) -> Result<Vec<WorkResult>, CollaborationError> {
        self.handle.list_work_results(conversation_id).await
    }

    async fn get_agent(&mut self, agent_id: AgentId) -> Result<Option<Agent>, CollaborationError> {
        self.handle
            .get_agent(agent_id)
            .await
            .map_err(|error| CollaborationError::Runtime(error.to_string()))
    }

    async fn get_agent_by_name(
        &mut self,
        name: String,
    ) -> Result<Option<Agent>, CollaborationError> {
        self.handle
            .get_agent_by_name(name)
            .await
            .map_err(|error| CollaborationError::Runtime(error.to_string()))
    }

    async fn list_room_members(
        &mut self,
        room_id: RoomId,
    ) -> Result<Vec<RoomMember>, CollaborationError> {
        self.handle.list_room_members(room_id).await
    }

    async fn add_room_member(
        &mut self,
        room_id: RoomId,
        agent_id: AgentId,
        role: Option<String>,
        changed_at: String,
    ) -> Result<MembershipChange, CollaborationError> {
        self.handle
            .add_room_member(room_id, agent_id, role, changed_at)
            .await
    }

    async fn remove_room_member(
        &mut self,
        room_id: RoomId,
        agent_id: AgentId,
        changed_at: String,
    ) -> Result<MembershipChange, CollaborationError> {
        self.handle
            .remove_room_member(room_id, agent_id, changed_at)
            .await
    }

    async fn create_thread(
        &mut self,
        thread: Conversation,
        primary_work_id: WorkItemId,
        user_id: String,
        initial_agents: Vec<AgentId>,
    ) -> Result<WorkItem, CollaborationError> {
        self.handle
            .create_thread(thread, primary_work_id, user_id, initial_agents)
            .await
    }

    async fn list_threads(
        &mut self,
        room_id: RoomId,
    ) -> Result<Vec<Conversation>, CollaborationError> {
        self.handle.list_threads(room_id).await
    }

    async fn list_thread_members(
        &mut self,
        thread_id: ConversationId,
    ) -> Result<Vec<ConversationMember>, CollaborationError> {
        self.handle.list_thread_members(thread_id).await
    }

    async fn add_thread_member(
        &mut self,
        thread_id: ConversationId,
        agent_id: AgentId,
        changed_at: String,
    ) -> Result<MembershipChange, CollaborationError> {
        self.handle
            .add_thread_member(thread_id, agent_id, changed_at)
            .await
    }

    async fn remove_thread_member(
        &mut self,
        thread_id: ConversationId,
        agent_id: AgentId,
        changed_at: String,
    ) -> Result<MembershipChange, CollaborationError> {
        self.handle
            .remove_thread_member(thread_id, agent_id, changed_at)
            .await
    }
}

impl WorkRuntime for StorageHandle {
    async fn assign_work_owner(
        &mut self,
        work_id: WorkItemId,
        owner_agent_id: AgentId,
        assigned_at: String,
    ) -> Result<WorkItem, WorkError> {
        self.work_request(|reply| {
            Command::AssignWorkOwner(work_id, owner_agent_id, assigned_at, reply)
        })
        .await
    }

    async fn transition_work(
        &mut self,
        work_id: WorkItemId,
        target: WorkStatus,
        transitioned_at: String,
    ) -> Result<WorkItem, WorkError> {
        self.work_request(|reply| Command::TransitionWork(work_id, target, transitioned_at, reply))
            .await
    }

    async fn create_work_result(&mut self, result: WorkResult) -> Result<WorkResult, WorkError> {
        self.work_request(|reply| Command::CreateWorkResult(result, reply))
            .await
    }
}

impl WorkRuntime for StorageWorker {
    async fn assign_work_owner(
        &mut self,
        work_id: WorkItemId,
        owner_agent_id: AgentId,
        assigned_at: String,
    ) -> Result<WorkItem, WorkError> {
        self.handle
            .assign_work_owner(work_id, owner_agent_id, assigned_at)
            .await
    }

    async fn transition_work(
        &mut self,
        work_id: WorkItemId,
        target: WorkStatus,
        transitioned_at: String,
    ) -> Result<WorkItem, WorkError> {
        self.handle
            .transition_work(work_id, target, transitioned_at)
            .await
    }

    async fn create_work_result(&mut self, result: WorkResult) -> Result<WorkResult, WorkError> {
        self.handle.create_work_result(result).await
    }
}

impl DependencyRuntime for StorageWorker {
    async fn add_work_dependency(
        &mut self,
        upstream_work_id: WorkItemId,
        downstream_work_id: WorkItemId,
        created_at: String,
    ) -> Result<WorkDependency, DependencyError> {
        self.dependency_request(|reply| {
            Command::AddWorkDependency(upstream_work_id, downstream_work_id, created_at, reply)
        })
        .await
    }

    async fn list_work_dependency_outcomes_for_downstream(
        &mut self,
        downstream_work_id: WorkItemId,
    ) -> Result<Vec<DependencyOutcome>, DependencyError> {
        self.dependency_request(|reply| {
            Command::ListWorkDependencyOutcomesForDownstream(downstream_work_id, reply)
        })
        .await
        .map(|outcomes| outcomes.into_iter().map(DependencyOutcome::from).collect())
    }
}

impl PublishRuntime for StorageHandle {
    async fn publish_result(
        &mut self,
        publish_id: PublishId,
        result_id: ResultId,
        target_conversation_id: ConversationId,
        published_at: String,
    ) -> Result<PublishedResult, PublishError> {
        self.publish_request(|reply| {
            Command::PublishResult(
                publish_id,
                result_id,
                target_conversation_id,
                published_at,
                reply,
            )
        })
        .await
        .map(PublishedResult::from)
    }

    async fn list_published_results(
        &mut self,
        target_conversation_id: ConversationId,
    ) -> Result<Vec<PublishedResult>, PublishError> {
        self.publish_request(|reply| Command::ListPublishedResults(target_conversation_id, reply))
            .await
            .map(|results| results.into_iter().map(PublishedResult::from).collect())
    }

    async fn list_publish_targets(
        &mut self,
        source_conversation_id: ConversationId,
    ) -> Result<Vec<ConversationId>, PublishError> {
        self.publish_request(|reply| Command::ListPublishTargets(source_conversation_id, reply))
            .await
    }
}

impl PublishRuntime for StorageWorker {
    async fn publish_result(
        &mut self,
        publish_id: PublishId,
        result_id: ResultId,
        target_conversation_id: ConversationId,
        published_at: String,
    ) -> Result<PublishedResult, PublishError> {
        self.publish_request(|reply| {
            Command::PublishResult(
                publish_id,
                result_id,
                target_conversation_id,
                published_at,
                reply,
            )
        })
        .await
        .map(PublishedResult::from)
    }

    async fn list_published_results(
        &mut self,
        target_conversation_id: ConversationId,
    ) -> Result<Vec<PublishedResult>, PublishError> {
        self.publish_request(|reply| Command::ListPublishedResults(target_conversation_id, reply))
            .await
            .map(|results| results.into_iter().map(PublishedResult::from).collect())
    }

    async fn list_publish_targets(
        &mut self,
        source_conversation_id: ConversationId,
    ) -> Result<Vec<ConversationId>, PublishError> {
        self.publish_request(|reply| Command::ListPublishTargets(source_conversation_id, reply))
            .await
    }
}

impl RecoveryRuntime for StorageWorker {
    async fn build_recovery_capsule(
        &mut self,
        command: BuildRecoveryCapsule,
    ) -> Result<RecoveryCapsule, RecoveryError> {
        self.handle.build_recovery_capsule(command).await
    }
}

impl Drop for StorageWorker {
    fn drop(&mut self) {
        if self.thread.is_some() {
            let (closed, receiver) = mpsc::channel(1);
            drop(receiver);
            let commands = std::mem::replace(&mut self.handle.commands, closed);
            let _ = commands.try_send(Command::Shutdown(None));
            drop(commands);
            let _ = self.join();
        }
    }
}

fn run(mut store: SqliteStore, mut commands: mpsc::Receiver<Command>) {
    while let Some(command) = commands.blocking_recv() {
        match command {
            Command::GetAgent(agent_id, reply) => {
                let _ = reply.send(store.get_agent(agent_id));
            }
            Command::GetAgentByName(name, reply) => {
                let _ = reply.send(store.get_agent_by_name(&name));
            }
            Command::CreateRoom(room, reply) => {
                let _ = reply.send(store.create_room(&room));
            }
            Command::GetRoom(room_id, reply) => {
                let _ = reply.send(store.get_room(room_id));
            }
            Command::GetRoomByName(name, reply) => {
                let _ = reply.send(store.get_room_by_name(&name));
            }
            Command::ListAgents(reply) => {
                let _ = reply.send(store.list_agents());
            }
            Command::CreateAgent(agent, reply) => {
                let _ = reply.send(store.insert_agent(&agent));
            }
            Command::UpdateAgent(agent, reply) => {
                let _ = reply.send(store.update_agent(&agent));
            }
            Command::ListWorkItems(conversation_id, reply) => {
                let _ = reply.send(store.list_work_items(conversation_id));
            }
            Command::ListWorkResults(conversation_id, reply) => {
                let _ = reply.send(store.list_work_results(conversation_id));
            }
            Command::ListPublishTargets(conversation_id, reply) => {
                let _ = reply.send(store.list_downstream_conversations(conversation_id));
            }
            Command::ListRooms(reply) => {
                let _ = reply.send(store.list_rooms());
            }
            Command::ListRoomMembers(room_id, reply) => {
                let _ = reply.send(store.list_room_members(room_id));
            }
            Command::AddRoomMember(room_id, agent_id, role, changed_at, reply) => {
                let _ = reply.send(store.add_room_member(
                    room_id,
                    agent_id,
                    role.as_deref(),
                    &changed_at,
                ));
            }
            Command::RemoveRoomMember(room_id, agent_id, changed_at, reply) => {
                let _ = reply.send(store.remove_room_member(room_id, agent_id, &changed_at));
            }
            Command::CreateThread(thread, primary_work_id, user_id, initial_agents, reply) => {
                let _ = reply.send(store.create_thread_with_primary_work(
                    &thread,
                    primary_work_id,
                    &user_id,
                    &initial_agents,
                ));
            }
            Command::ListThreads(room_id, reply) => {
                let _ = reply.send(store.list_threads(room_id));
            }
            Command::ListThreadMembers(thread_id, reply) => {
                let result = store.get_thread(thread_id).and_then(|thread| match thread {
                    Some(_) => store.list_conversation_members(thread_id),
                    None => Err(StoreError::ThreadNotFound(thread_id)),
                });
                let _ = reply.send(result);
            }
            Command::AddThreadMember(thread_id, agent_id, changed_at, reply) => {
                let _ = reply.send(store.add_thread_member(thread_id, agent_id, &changed_at));
            }
            Command::RemoveThreadMember(thread_id, agent_id, changed_at, reply) => {
                let _ = reply.send(store.remove_thread_member(thread_id, agent_id, &changed_at));
            }
            Command::AssignWorkOwner(work_id, owner_agent_id, assigned_at, reply) => {
                let _ = reply.send(store.assign_work_owner(work_id, owner_agent_id, &assigned_at));
            }
            Command::TransitionWork(work_id, target, transitioned_at, reply) => {
                let _ = reply.send(store.transition_work(work_id, target, &transitioned_at));
            }
            Command::CreateWorkResult(result, reply) => {
                let _ = reply.send(store.create_work_result(&result));
            }
            Command::AddWorkDependency(upstream_work_id, downstream_work_id, created_at, reply) => {
                let _ = reply.send(store.add_work_dependency(
                    upstream_work_id,
                    downstream_work_id,
                    &created_at,
                ));
            }
            Command::ListWorkDependencyOutcomesForDownstream(downstream_work_id, reply) => {
                let _ = reply
                    .send(store.list_work_dependency_outcomes_for_downstream(downstream_work_id));
            }
            Command::PublishResult(
                publish_id,
                result_id,
                target_conversation_id,
                published_at,
                reply,
            ) => {
                let _ = reply.send(store.publish_result(
                    publish_id,
                    result_id,
                    target_conversation_id,
                    &published_at,
                ));
            }
            Command::ListPublishedResults(target_conversation_id, reply) => {
                let _ = reply.send(store.list_published_results(target_conversation_id));
            }
            Command::ProposeHandoff(handoff, reply) => {
                let _ = reply.send(store.propose_handoff(&handoff));
            }
            Command::RespondToHandoff(handoff_id, response, responded_at, reply) => {
                let _ = reply.send(store.respond_to_handoff(handoff_id, &response, &responded_at));
            }
            Command::ChallengeHandoff(handoff_id, challenge, challenged_at, reply) => {
                let _ = reply.send(store.challenge_handoff(handoff_id, &challenge, &challenged_at));
            }
            Command::ResolveHandoff(handoff_id, agent_id, resolved_at, reply) => {
                let _ = reply.send(store.resolve_handoff(handoff_id, agent_id, &resolved_at));
            }
            Command::CreateProposal(proposal, reply) => {
                let _ = reply.send(store.create_proposal(&proposal));
            }
            Command::RespondToProposal(response, reply) => {
                let _ = reply.send(store.respond_to_proposal(&response));
            }
            Command::WithdrawProposal(proposal_id, agent_id, withdrawn_at, reply) => {
                let _ = reply.send(store.withdraw_proposal(proposal_id, agent_id, &withdrawn_at));
            }
            Command::RecordDecision(decision, reply) => {
                let _ = reply.send(store.record_decision(&decision));
            }
            Command::DecideDecision(decision_id, outcome, decided_at, reply) => {
                let _ = reply.send(store.decide(decision_id, &outcome, &decided_at));
            }
            Command::CancelDecision(decision_id, cancelled_by, reason, cancelled_at, reply) => {
                let _ = reply.send(store.cancel_decision(
                    decision_id,
                    cancelled_by,
                    &reason,
                    &cancelled_at,
                ));
            }
            Command::ListPendingDecisions(reply) => {
                let _ = reply.send(store.list_pending_decisions());
            }
            Command::ConvertDecisionToWork(decision_id, items, created_at, reply) => {
                let _ =
                    reply.send(store.convert_decision_to_work(decision_id, &items, &created_at));
            }
            Command::AdmitThreadSession(thread_id, agent_id, admitted_at, reply) => {
                let _ = reply.send(store.admit_thread_session(thread_id, agent_id, &admitted_at));
            }
            Command::GetOrCreateDm(user_id, agent_id, now, reply) => {
                let _ = reply.send(store.get_or_create_dm(&user_id, agent_id, &now));
            }
            Command::GetOrCreateAgentDm(source_agent_id, target_agent_id, now, reply) => {
                let _ = reply.send(store.get_or_create_agent_dm(
                    source_agent_id,
                    target_agent_id,
                    &now,
                ));
            }
            Command::InsertMessage(message, reply) => {
                let _ = reply.send(store.insert_message(&message));
            }
            Command::PersistAgentDirectMessage(
                message_id,
                source_agent_id,
                target_agent_id,
                body,
                sent_at,
                reply,
            ) => {
                let _ = reply.send(store.persist_agent_direct_message(
                    message_id,
                    source_agent_id,
                    target_agent_id,
                    &body,
                    &sent_at,
                ));
            }
            Command::PersistThreadMention(
                message,
                source_agent_id,
                target_agent_id,
                capsule,
                reply,
            ) => {
                let _ = reply.send(store.persist_thread_mention(
                    &message,
                    source_agent_id,
                    target_agent_id,
                    &capsule,
                ));
            }
            Command::MarkDeliveryCapsuleDelivered(
                message_id,
                target_agent_id,
                delivered_at,
                reply,
            ) => {
                let _ = reply.send(store.mark_delivery_capsule_delivered(
                    message_id,
                    target_agent_id,
                    &delivered_at,
                ));
            }
            Command::MarkDeliveryDelivered(message_id, target_agent_id, delivered_at, reply) => {
                let _ = reply.send(store.mark_delivery_delivered(
                    message_id,
                    target_agent_id,
                    &delivered_at,
                ));
            }
            Command::MarkDeliveryFailed(message_id, target_agent_id, failed_at, reply) => {
                let _ =
                    reply.send(store.mark_delivery_failed(message_id, target_agent_id, &failed_at));
            }
            Command::ListFailedMessageDeliveries(reply) => {
                let _ = reply.send(store.list_failed_message_deliveries());
            }
            Command::ClaimThreadMentionRetry(message_id, target_agent_id, claimed_at, reply) => {
                let _ = reply.send(store.claim_failed_thread_mention_delivery(
                    message_id,
                    target_agent_id,
                    &claimed_at,
                ));
            }
            Command::ClaimAgentDirectMessageRetry(
                message_id,
                target_agent_id,
                claimed_at,
                reply,
            ) => {
                let _ = reply.send(store.claim_failed_agent_direct_message_delivery(
                    message_id,
                    target_agent_id,
                    &claimed_at,
                ));
            }
            Command::ListMessages(conversation_id, reply) => {
                let _ = reply.send(store.list_messages(conversation_id));
            }
            Command::ListRecentMessages(conversation_id, limit, reply) => {
                let _ = reply.send(store.list_recent_messages_after(conversation_id, None, limit));
            }
            Command::InsertCheckpoint(checkpoint, reply) => {
                let _ = reply.send(store.insert_checkpoint(&checkpoint));
            }
            Command::GetLatestCheckpoint(conversation_id, agent_id, reply) => {
                let _ = reply.send(store.get_latest_checkpoint(conversation_id, agent_id));
            }
            Command::PromoteMemory(memory, reply) => {
                let _ = reply.send(store.promote_memory(&memory));
            }
            Command::ListMemories(scope_type, scope_id, kind, reply) => {
                let _ = reply.send(store.list_memories(scope_type, &scope_id, kind));
            }
            Command::BuildRecoveryCapsule(command, reply) => {
                let _ = reply.send(build_recovery_capsule(&store, command));
            }
            Command::InsertBinding(binding, reply) => {
                let _ = reply.send(store.insert_session_binding(&binding));
            }
            Command::GetCurrentBinding(conversation_id, agent_id, reply) => {
                let _ = reply.send(store.get_current_session_binding(conversation_id, agent_id));
            }
            Command::GetLatestBinding(conversation_id, agent_id, reply) => {
                let _ = reply.send(store.get_latest_session_binding(conversation_id, agent_id));
            }
            Command::ListCurrentBindings(agent_id, reply) => {
                let _ = reply.send(store.list_current_session_bindings_for_agent(agent_id));
            }
            Command::UpdateBindingStatus(id, status, last_used_at, reply) => {
                let _ = reply.send(store.update_session_binding_status(id, status, &last_used_at));
            }
            Command::MarkDisconnected(binding_id, last_used_at, reply) => {
                let _ = reply.send(store.mark_binding_disconnected(binding_id, &last_used_at));
            }
            Command::BeginSessionReplacement(
                source_binding_id,
                replacement_binding_id,
                capsule,
                replaced_at,
                reply,
            ) => {
                let _ = reply.send(store.begin_session_replacement(
                    source_binding_id,
                    replacement_binding_id,
                    &capsule,
                    &replaced_at,
                ));
            }
            Command::GetSessionRecovery(session_binding_id, reply) => {
                let _ = reply.send(store.get_session_recovery(session_binding_id));
            }
            Command::AttachReplacementRemoteSession(
                session_binding_id,
                remote_session_id,
                attached_at,
                reply,
            ) => {
                let _ = reply.send(store.attach_replacement_remote_session(
                    session_binding_id,
                    &remote_session_id,
                    &attached_at,
                ));
            }
            Command::MarkSessionRecoveryCapsuleDelivered(
                session_binding_id,
                delivered_at,
                reply,
            ) => {
                let _ = reply.send(
                    store
                        .mark_session_recovery_capsule_delivered(session_binding_id, &delivered_at),
                );
            }
            Command::InsertPermission(decision, reply) => {
                let _ = reply.send(store.insert_permission_decision(&decision));
            }
            Command::GetPermission(id, reply) => {
                let _ = reply.send(store.get_permission_decision(&id));
            }
            Command::Shutdown(done) => {
                if let Some(done) = done {
                    let _ = done.send(());
                }
                return;
            }
        }
    }
}

fn build_recovery_capsule(
    store: &SqliteStore,
    command: BuildRecoveryCapsule,
) -> Result<RecoveryCapsule, RecoveryError> {
    let agent = store
        .get_agent(command.agent_id)
        .map_err(recovery_runtime_error)?
        .ok_or(RecoveryError::AgentNotFound(command.agent_id))?;
    if agent.status != "active" {
        return Err(RecoveryError::AgentInactive(agent.id));
    }
    let conversation = store
        .get_conversation(command.conversation_id)
        .map_err(recovery_runtime_error)?
        .ok_or(RecoveryError::ConversationNotFound(command.conversation_id))?;
    let active_member = store
        .list_conversation_members(conversation.id)
        .map_err(recovery_runtime_error)?
        .into_iter()
        .any(|member| {
            member.member_type == MemberType::Agent
                && member.member_id == agent.id.to_string()
                && member.left_at.is_none()
        });
    if !active_member {
        return Err(RecoveryError::AgentNotMember {
            conversation_id: conversation.id,
            agent_id: agent.id,
        });
    }

    let checkpoint = store
        .get_latest_checkpoint(conversation.id, agent.id)
        .map_err(recovery_runtime_error)?;
    let anchor = match checkpoint.as_ref() {
        Some(Checkpoint {
            id: checkpoint_id,
            last_message_id: Some(message_id),
            ..
        }) => {
            let (checkpoint_id, message_id) = (*checkpoint_id, *message_id);
            let message = store
                .get_message(message_id)
                .map_err(recovery_runtime_error)?
                .filter(|message| message.conversation_id == conversation.id)
                .ok_or(RecoveryError::InvalidCheckpointAnchor {
                    checkpoint_id,
                    message_id,
                })?;
            Some(message)
        }
        _ => None,
    };
    let memories = store
        .list_current_recovery_memories(&agent.project_root, conversation.room_id)
        .map_err(recovery_runtime_error)?;
    let published_results = store
        .list_published_results(conversation.id)
        .map_err(recovery_runtime_error)?;
    let (messages, messages_truncated) = store
        .list_recent_messages_after(
            conversation.id,
            anchor.as_ref(),
            crate::application::RECENT_MESSAGE_LIMIT,
        )
        .map_err(recovery_runtime_error)?;

    format_recovery_capsule(RecoveryInput {
        agent,
        conversation,
        memories,
        checkpoint,
        published_results,
        messages,
        messages_truncated,
    })
}

fn recovery_runtime_error(error: StoreError) -> RecoveryError {
    RecoveryError::Runtime(error.to_string())
}

fn map_store_error(error: StoreError) -> CollaborationError {
    match error {
        StoreError::RoomNotFound(id) => CollaborationError::RoomNotFound(id.to_string()),
        StoreError::RoomInactive(id) => CollaborationError::RoomInactive(id),
        StoreError::RoomIdConflict(id) => CollaborationError::RoomIdConflict(id),
        StoreError::RoomNameConflict(name) => CollaborationError::RoomNameConflict(name),
        StoreError::AgentNotFound(id) => CollaborationError::AgentNotFound(id.to_string()),
        StoreError::AgentInactive(id) => CollaborationError::AgentInactive(id),
        StoreError::ThreadNotFound(id) | StoreError::NotThread(id) => {
            CollaborationError::ThreadNotFound(id)
        }
        StoreError::ThreadNotOpen(id) => CollaborationError::ThreadNotOpen(id),
        StoreError::RoomMembershipRequired { room_id, agent_id } => {
            CollaborationError::RoomMembershipRequired { room_id, agent_id }
        }
        StoreError::ThreadMembershipRequired {
            thread_id,
            agent_id,
        } => CollaborationError::ThreadMembershipRequired {
            thread_id,
            agent_id,
        },
        StoreError::RoomRemovalBlocked { room_id, agent_id } => {
            CollaborationError::RoomRemovalBlocked { room_id, agent_id }
        }
        StoreError::ThreadIdConflict(id) => CollaborationError::ThreadIdConflict(id),
        StoreError::PrimaryWorkIdConflict(id) => CollaborationError::PrimaryWorkIdConflict(id),
        StoreError::MessageSenderMismatch(id) => {
            CollaborationError::InvalidCommand(format!("message sender must be agent {id}"))
        }
        StoreError::Domain(error) => CollaborationError::InvalidCommand(error.to_string()),
        error => CollaborationError::Runtime(error.to_string()),
    }
}

fn map_work_error(error: StoreError) -> WorkError {
    match error {
        StoreError::WorkItemNotFound(id) => WorkError::WorkNotFound(id),
        StoreError::AgentNotFound(id) => WorkError::OwnerNotFound(id),
        StoreError::AgentInactive(id) => WorkError::OwnerInactive(id),
        StoreError::WorkOwnerScopeRequired {
            work_id,
            owner_agent_id,
        } => WorkError::OwnerOutOfScope {
            work_id,
            owner_agent_id,
        },
        StoreError::TerminalWorkOwnerImmutable(id) => WorkError::TerminalOwnershipImmutable(id),
        StoreError::InvalidWorkTransition { work_id, from, to } => {
            WorkError::InvalidTransition { work_id, from, to }
        }
        StoreError::InvalidWorkTimestamp => WorkError::InvalidTimestamp,
        StoreError::WorkResultConflict(id) => WorkError::ResultConflict(id),
        StoreError::SupersededWorkResultNotFound(id) => WorkError::SupersededResultNotFound(id),
        StoreError::CrossWorkResultSupersede {
            result_id,
            supersedes_result_id,
        } => WorkError::CrossWorkSupersede {
            result_id,
            supersedes_result_id,
        },
        error => WorkError::Runtime(error.to_string()),
    }
}

fn map_dependency_error(error: StoreError) -> DependencyError {
    match error {
        StoreError::WorkItemNotFound(id) => DependencyError::WorkNotFound(id),
        StoreError::WorkDependencySelf(id) => DependencyError::SelfDependency(id),
        StoreError::WorkDependencyCycle {
            upstream_work_id,
            downstream_work_id,
        } => DependencyError::Cycle {
            upstream_work_id,
            downstream_work_id,
        },
        StoreError::WorkDependencyConflict {
            upstream_work_id,
            downstream_work_id,
        } => DependencyError::Conflict {
            upstream_work_id,
            downstream_work_id,
        },
        StoreError::Domain(crate::domain::DomainError::EmptyField(
            "work_dependency.created_at",
        )) => DependencyError::InvalidTimestamp,
        error => DependencyError::Runtime(error.to_string()),
    }
}

fn map_deliberation_error(error: StoreError) -> DeliberationError {
    match &error {
        StoreError::HandoffNotFound(_)
        | StoreError::ProposalNotFound(_)
        | StoreError::DecisionNotFound(_)
        | StoreError::WorkItemNotFound(_)
        | StoreError::ThreadNotFound(_)
        | StoreError::AgentNotFound(_) => DeliberationError::NotFound(error.to_string()),
        StoreError::HandoffIdConflict(_)
        | StoreError::HandoffAlreadyOpen(_)
        | StoreError::ProposalIdConflict(_)
        | StoreError::ProposalResponseIdConflict(_)
        | StoreError::DecisionIdConflict(_)
        | StoreError::DecisionWorkConflict { .. }
        | StoreError::WorkDependencyConflict { .. } => {
            DeliberationError::Conflict(error.to_string())
        }
        StoreError::HandoffRespondentMismatch { .. }
        | StoreError::HandoffSourceMismatch { .. }
        | StoreError::ProposalAuthorMismatch { .. }
        | StoreError::DecisionOwnerMismatch { .. }
        | StoreError::ThreadMembershipRequired { .. }
        | StoreError::RoomMembershipRequired { .. }
        | StoreError::WorkOwnerScopeRequired { .. }
        | StoreError::AgentInactive(_) => DeliberationError::NotPermitted(error.to_string()),
        StoreError::InvalidHandoffTransition { .. }
        | StoreError::InvalidProposalTransition { .. }
        | StoreError::InvalidDecisionTransition { .. }
        | StoreError::ProposalNotLive { .. }
        | StoreError::DecisionNotDecided { .. }
        | StoreError::TerminalWorkOwnerImmutable(_) => {
            DeliberationError::InvalidTransition(error.to_string())
        }
        StoreError::Domain(_)
        | StoreError::HandoffChallengeMissingEvidence(_)
        | StoreError::HandoffWorkOutOfThread { .. }
        | StoreError::ProposalOutOfThread { .. }
        | StoreError::ProposalSupersedeOutOfThread { .. }
        | StoreError::DecisionSupersedeOutOfThread { .. }
        | StoreError::DecisionHasNoHandoff(_)
        | StoreError::InvalidHandoffTimestamp
        | StoreError::InvalidProposalTimestamp
        | StoreError::InvalidDecisionTimestamp
        | StoreError::InvalidWorkTimestamp => DeliberationError::Invalid(error.to_string()),
        _ => DeliberationError::Runtime(error.to_string()),
    }
}

impl DeliberationRuntime for StorageHandle {
    async fn propose_handoff(&mut self, handoff: Handoff) -> Result<Handoff, DeliberationError> {
        self.deliberation_request(|reply| Command::ProposeHandoff(Box::new(handoff), reply))
            .await
    }

    async fn respond_to_handoff(
        &mut self,
        handoff_id: HandoffId,
        response: HandoffResponse,
        responded_at: String,
    ) -> Result<Handoff, DeliberationError> {
        self.deliberation_request(|reply| {
            Command::RespondToHandoff(handoff_id, Box::new(response), responded_at, reply)
        })
        .await
    }

    async fn challenge_handoff(
        &mut self,
        handoff_id: HandoffId,
        challenge: HandoffChallenge,
        challenged_at: String,
    ) -> Result<(Handoff, Option<Decision>), DeliberationError> {
        self.deliberation_request(|reply| {
            Command::ChallengeHandoff(handoff_id, Box::new(challenge), challenged_at, reply)
        })
        .await
    }

    async fn resolve_handoff(
        &mut self,
        handoff_id: HandoffId,
        agent_id: AgentId,
        resolved_at: String,
    ) -> Result<Handoff, DeliberationError> {
        self.deliberation_request(|reply| {
            Command::ResolveHandoff(handoff_id, agent_id, resolved_at, reply)
        })
        .await
    }

    async fn create_proposal(&mut self, proposal: Proposal) -> Result<Proposal, DeliberationError> {
        self.deliberation_request(|reply| Command::CreateProposal(Box::new(proposal), reply))
            .await
    }

    async fn respond_to_proposal(
        &mut self,
        response: ProposalResponse,
    ) -> Result<ProposalResponse, DeliberationError> {
        self.deliberation_request(|reply| Command::RespondToProposal(Box::new(response), reply))
            .await
    }

    async fn withdraw_proposal(
        &mut self,
        proposal_id: ProposalId,
        agent_id: AgentId,
        withdrawn_at: String,
    ) -> Result<Proposal, DeliberationError> {
        self.deliberation_request(|reply| {
            Command::WithdrawProposal(proposal_id, agent_id, withdrawn_at, reply)
        })
        .await
    }

    async fn record_decision(&mut self, decision: Decision) -> Result<Decision, DeliberationError> {
        self.deliberation_request(|reply| Command::RecordDecision(Box::new(decision), reply))
            .await
    }

    async fn decide(
        &mut self,
        decision_id: DecisionId,
        outcome: DecisionOutcome,
        decided_at: String,
    ) -> Result<Decision, DeliberationError> {
        self.deliberation_request(|reply| {
            Command::DecideDecision(decision_id, Box::new(outcome), decided_at, reply)
        })
        .await
    }

    async fn cancel_decision(
        &mut self,
        decision_id: DecisionId,
        cancelled_by: DecisionOwner,
        reason: String,
        cancelled_at: String,
    ) -> Result<Decision, DeliberationError> {
        self.deliberation_request(|reply| {
            Command::CancelDecision(decision_id, cancelled_by, reason, cancelled_at, reply)
        })
        .await
    }

    async fn convert_decision_to_work(
        &mut self,
        decision_id: DecisionId,
        items: Vec<DecisionWork>,
        created_at: String,
    ) -> Result<Vec<WorkItem>, DeliberationError> {
        self.deliberation_request(|reply| {
            Command::ConvertDecisionToWork(decision_id, items, created_at, reply)
        })
        .await
    }
}

impl DeliberationRuntime for StorageWorker {
    async fn propose_handoff(&mut self, handoff: Handoff) -> Result<Handoff, DeliberationError> {
        self.handle.propose_handoff(handoff).await
    }

    async fn respond_to_handoff(
        &mut self,
        handoff_id: HandoffId,
        response: HandoffResponse,
        responded_at: String,
    ) -> Result<Handoff, DeliberationError> {
        self.handle
            .respond_to_handoff(handoff_id, response, responded_at)
            .await
    }

    async fn challenge_handoff(
        &mut self,
        handoff_id: HandoffId,
        challenge: HandoffChallenge,
        challenged_at: String,
    ) -> Result<(Handoff, Option<Decision>), DeliberationError> {
        self.handle
            .challenge_handoff(handoff_id, challenge, challenged_at)
            .await
    }

    async fn resolve_handoff(
        &mut self,
        handoff_id: HandoffId,
        agent_id: AgentId,
        resolved_at: String,
    ) -> Result<Handoff, DeliberationError> {
        self.handle
            .resolve_handoff(handoff_id, agent_id, resolved_at)
            .await
    }

    async fn create_proposal(&mut self, proposal: Proposal) -> Result<Proposal, DeliberationError> {
        self.handle.create_proposal(proposal).await
    }

    async fn respond_to_proposal(
        &mut self,
        response: ProposalResponse,
    ) -> Result<ProposalResponse, DeliberationError> {
        self.handle.respond_to_proposal(response).await
    }

    async fn withdraw_proposal(
        &mut self,
        proposal_id: ProposalId,
        agent_id: AgentId,
        withdrawn_at: String,
    ) -> Result<Proposal, DeliberationError> {
        self.handle
            .withdraw_proposal(proposal_id, agent_id, withdrawn_at)
            .await
    }

    async fn record_decision(&mut self, decision: Decision) -> Result<Decision, DeliberationError> {
        self.handle.record_decision(decision).await
    }

    async fn decide(
        &mut self,
        decision_id: DecisionId,
        outcome: DecisionOutcome,
        decided_at: String,
    ) -> Result<Decision, DeliberationError> {
        self.handle.decide(decision_id, outcome, decided_at).await
    }

    async fn cancel_decision(
        &mut self,
        decision_id: DecisionId,
        cancelled_by: DecisionOwner,
        reason: String,
        cancelled_at: String,
    ) -> Result<Decision, DeliberationError> {
        self.handle
            .cancel_decision(decision_id, cancelled_by, reason, cancelled_at)
            .await
    }

    async fn convert_decision_to_work(
        &mut self,
        decision_id: DecisionId,
        items: Vec<DecisionWork>,
        created_at: String,
    ) -> Result<Vec<WorkItem>, DeliberationError> {
        self.handle
            .convert_decision_to_work(decision_id, items, created_at)
            .await
    }
}

fn map_publish_error(error: StoreError) -> PublishError {
    match error {
        StoreError::PublishResultNotFound(id) => PublishError::ResultNotFound(id),
        StoreError::WorkItemNotFound(id) => PublishError::WorkNotFound(id),
        StoreError::PublishSourceNotFound(id) => PublishError::SourceNotFound(id),
        StoreError::PublishTargetNotFound(id) => PublishError::TargetNotFound(id),
        StoreError::PublishIdConflict(id) => PublishError::PublishIdConflict(id),
        StoreError::InvalidPublishTimestamp => PublishError::InvalidTimestamp,
        error => PublishError::Runtime(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ConversationKind, MemberType};

    #[tokio::test]
    async fn bounded_recent_message_request_preserves_order_limit_and_conversation() {
        let directory = std::env::temp_dir().join(format!(
            "july-storage-worker-test-{}",
            ulid::Ulid::generate()
        ));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("workspace.db");
        let requested = conversation(ConversationId::new());
        let unrelated = conversation(ConversationId::new());
        let store = SqliteStore::open(&path).unwrap();
        store.insert_conversation(&requested).unwrap();
        store.insert_conversation(&unrelated).unwrap();
        for number in 1_u128..=52 {
            store
                .insert_message(&message(number, requested.id))
                .unwrap();
        }
        store.insert_message(&message(100, unrelated.id)).unwrap();
        drop(store);

        let mut worker = StorageWorker::open(&path).unwrap();
        let (messages, truncated) = worker
            .handle()
            .list_recent_messages(requested.id, 50)
            .await
            .unwrap();

        assert!(truncated);
        assert_eq!(messages.len(), 50);
        assert!(
            messages
                .iter()
                .all(|message| message.conversation_id == requested.id)
        );
        assert_eq!(messages.first().unwrap().body, "message-03");
        assert_eq!(messages.last().unwrap().body, "message-52");
        worker.shutdown().await.unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    fn conversation(id: ConversationId) -> Conversation {
        Conversation {
            id,
            kind: ConversationKind::Dm,
            room_id: None,
            title: None,
            goal: None,
            parent_conversation_id: None,
            origin_conversation_id: None,
            status: "open".into(),
            created_at: "2026-09-01T10:00:00Z".into(),
            updated_at: "2026-09-01T10:00:00Z".into(),
        }
    }

    fn message(number: u128, conversation_id: ConversationId) -> Message {
        Message {
            id: ulid::Ulid::from(number).into(),
            conversation_id,
            sender_type: MemberType::User,
            sender_id: "tony".into(),
            body: format!("message-{number:02}"),
            reply_to: None,
            metadata: serde_json::Value::Null,
            created_at: "2026-09-01T10:00:00Z".into(),
        }
    }
}
