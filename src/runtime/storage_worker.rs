use super::{RuntimeError, timestamp};
use crate::application::{
    CollaborationError, CollaborationRuntime, MembershipChange, MembershipState, PublishError,
    PublishRuntime, PublishedResult, WorkError, WorkRuntime,
};
use crate::domain::{
    Agent, AgentId, Conversation, ConversationId, ConversationMember, Message, MessageDelivery,
    MessageId, PermissionDecision, Publish, PublishId, ResultId, Room, RoomId, RoomMember,
    SessionBinding, SessionBindingId, SessionBindingStatus, WorkItem, WorkItemId, WorkResult,
    WorkStatus,
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
    PublishResult(
        PublishId,
        ResultId,
        ConversationId,
        String,
        Reply<(Publish, WorkResult)>,
    ),
    ListPublishedResults(ConversationId, Reply<Vec<(Publish, WorkResult)>>),
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
        let path = PathBuf::from(path.as_ref());
        let (commands, receiver) = mpsc::channel(STORAGE_CAPACITY);
        let (started, ready) = std::sync::mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            let store = match SqliteStore::open(path) {
                Ok(mut store) => {
                    if let Err(error) = store.reconcile_pending_deliveries(&timestamp()) {
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
}

impl std::ops::Deref for StorageWorker {
    type Target = StorageHandle;

    fn deref(&self) -> &Self::Target {
        &self.handle
    }
}

impl CollaborationRuntime for StorageWorker {
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

impl WorkRuntime for StorageWorker {
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
