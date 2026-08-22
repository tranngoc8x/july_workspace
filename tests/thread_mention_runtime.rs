use july_workspace::application::{
    CollaborationError, MentionThreadAgent, OpenThreadForAgent, ThreadRuntime,
};
use july_workspace::domain::{
    Agent, AgentId, Conversation, ConversationId, ConversationKind, MemberType, Message, MessageId,
    Room, RoomId, WorkItemId,
};
use july_workspace::runtime::{StorageWorker, WorkspaceRuntime};
use july_workspace::storage::SqliteStore;
use july_workspace::transport::{
    AgentConnection, AgentTransport, CreateSession, PermissionResponse, ResumeSession, SendMessage,
    SessionCreated, SessionRef, SessionResumed, TransportError, TransportEvents,
};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const CREATED: &str = "2026-08-22T08:00:00Z";
const MENTIONED: &str = "2026-08-22T09:00:00Z";
const LATER: &str = "2026-08-22T10:00:00Z";
const CAPSULE: &str = "Goal: review only this thread";
const BODY: &str = "Please review the payment contract exactly.";

struct TestDatabase {
    directory: PathBuf,
    path: PathBuf,
}

impl TestDatabase {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("july-thread-routing-{}", ulid::Ulid::generate()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("workspace.db");
        Self { directory, path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[derive(Default)]
struct ObservedTransport {
    connections: Vec<AgentConnection>,
    creates: Vec<CreateSession>,
    messages: Vec<SendMessage>,
}

struct FakeTransport {
    events: Option<tokio::sync::mpsc::Receiver<july_workspace::transport::TransportEvent>>,
    _event_source: tokio::sync::mpsc::Sender<july_workspace::transport::TransportEvent>,
    observed: Arc<Mutex<ObservedTransport>>,
    remote_prefix: String,
    create_fails_at: Option<usize>,
    send_fails_at: Option<usize>,
}

impl FakeTransport {
    fn new(remote_prefix: &str) -> (Self, Arc<Mutex<ObservedTransport>>) {
        let (sender, receiver) = tokio::sync::mpsc::channel(4);
        let observed = Arc::new(Mutex::new(ObservedTransport::default()));
        (
            Self {
                events: Some(receiver),
                _event_source: sender,
                observed: observed.clone(),
                remote_prefix: remote_prefix.into(),
                create_fails_at: None,
                send_fails_at: None,
            },
            observed,
        )
    }
}

impl AgentTransport for FakeTransport {
    async fn connect(&mut self, agent: &AgentConnection) -> Result<(), TransportError> {
        self.observed
            .lock()
            .unwrap()
            .connections
            .push(agent.clone());
        Ok(())
    }

    async fn create_session(
        &mut self,
        request: CreateSession,
    ) -> Result<SessionCreated, TransportError> {
        let mut observed = self.observed.lock().unwrap();
        observed.creates.push(request.clone());
        if self.create_fails_at == Some(observed.creates.len()) {
            return Err(TransportError::Protocol("create failed".into()));
        }
        Ok(SessionCreated {
            session: SessionRef {
                binding_id: request.binding_id,
                remote_session_id: format!("{}-{}", self.remote_prefix, observed.creates.len()),
            },
        })
    }

    async fn resume_session(
        &mut self,
        request: ResumeSession,
    ) -> Result<SessionResumed, TransportError> {
        Ok(SessionResumed {
            session: request.session,
        })
    }

    async fn send_message(&mut self, request: SendMessage) -> Result<(), TransportError> {
        let mut observed = self.observed.lock().unwrap();
        observed.messages.push(request);
        if self.send_fails_at == Some(observed.messages.len()) {
            Err(TransportError::Protocol("send failed".into()))
        } else {
            Ok(())
        }
    }

    async fn cancel_turn(&mut self, _session: SessionRef) -> Result<(), TransportError> {
        Ok(())
    }

    async fn respond_permission(
        &mut self,
        _response: PermissionResponse,
    ) -> Result<(), TransportError> {
        Ok(())
    }

    async fn close_session(&mut self, _session: SessionRef) -> Result<(), TransportError> {
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), TransportError> {
        Ok(())
    }

    fn subscribe(&mut self) -> Result<TransportEvents, TransportError> {
        self.events
            .take()
            .map(TransportEvents::new)
            .ok_or(TransportError::AlreadySubscribed)
    }
}

struct Fixture {
    source: Agent,
    target: Agent,
    mention_thread: Conversation,
    other_mention_thread: Conversation,
    source_owner_thread: Conversation,
    target_owner_thread: Conversation,
    unrelated_message: Message,
}

fn agent(name: &str) -> Agent {
    Agent {
        id: AgentId::new(),
        name: name.into(),
        project_root: format!("/workspace/{name}"),
        transport_type: "acp".into(),
        transport_config: json!({}),
        status: "active".into(),
        metadata: json!({}),
        created_at: CREATED.into(),
        updated_at: CREATED.into(),
    }
}

fn thread(room_id: RoomId, title: &str) -> Conversation {
    Conversation {
        id: ConversationId::new(),
        kind: ConversationKind::Thread,
        room_id: Some(room_id),
        title: Some(title.into()),
        goal: None,
        parent_conversation_id: None,
        origin_conversation_id: None,
        status: "open".into(),
        created_at: CREATED.into(),
        updated_at: CREATED.into(),
    }
}

fn seed(database: &TestDatabase) -> Fixture {
    let source = agent("source");
    let target = agent("target");
    let room = Room {
        id: RoomId::new(),
        name: "routing".into(),
        description: None,
        status: "active".into(),
        created_at: CREATED.into(),
        updated_at: CREATED.into(),
    };
    let mention_thread = thread(room.id, "Mention target");
    let other_mention_thread = thread(room.id, "Wrong context");
    let source_owner_thread = thread(room.id, "Source owner");
    let target_owner_thread = thread(room.id, "Target owner");
    let unrelated_message = Message {
        id: MessageId::new(),
        conversation_id: source_owner_thread.id,
        sender_type: MemberType::Agent,
        sender_id: source.id.to_string(),
        body: "unrelated transcript must stay here".into(),
        reply_to: None,
        metadata: json!({}),
        created_at: CREATED.into(),
    };
    let mut store = SqliteStore::open(database.path()).unwrap();
    store.insert_agent(&source).unwrap();
    store.insert_agent(&target).unwrap();
    store.insert_room(&room).unwrap();
    store
        .add_room_member(room.id, source.id, None, CREATED)
        .unwrap();
    store
        .add_room_member(room.id, target.id, None, CREATED)
        .unwrap();
    for (thread, agents) in [
        (&mention_thread, &[source.id][..]),
        (&other_mention_thread, &[source.id][..]),
        (&source_owner_thread, &[source.id][..]),
        (&target_owner_thread, &[target.id][..]),
    ] {
        store
            .create_thread_with_primary_work(thread, WorkItemId::new(), "tony", agents)
            .unwrap();
    }
    store.insert_message(&unrelated_message).unwrap();
    Fixture {
        source,
        target,
        mention_thread,
        other_mention_thread,
        source_owner_thread,
        target_owner_thread,
        unrelated_message,
    }
}

fn open_command(thread_id: ConversationId, agent_id: AgentId) -> OpenThreadForAgent {
    OpenThreadForAgent {
        thread_id,
        agent_id,
        opened_at: CREATED.into(),
    }
}

fn mention_command(
    fixture: &Fixture,
    message_id: MessageId,
    thread_id: ConversationId,
    target_agent_id: AgentId,
    body: &str,
    capsule: &str,
    mentioned_at: &str,
) -> MentionThreadAgent {
    MentionThreadAgent {
        thread_id,
        source_agent_id: fixture.source.id,
        target_agent_id,
        message_id,
        body: body.into(),
        capsule: capsule.into(),
        mentioned_at: mentioned_at.into(),
    }
}

fn target_members(database: &TestDatabase, fixture: &Fixture, thread_id: ConversationId) -> usize {
    SqliteStore::open(database.path())
        .unwrap()
        .list_conversation_members(thread_id)
        .unwrap()
        .into_iter()
        .filter(|member| {
            member.member_type == MemberType::Agent
                && member.member_id == fixture.target.id.to_string()
                && member.left_at.is_none()
        })
        .count()
}

#[tokio::test]
async fn mention_joins_target_then_reuses_its_shared_owner_and_session_without_transcript_leak() {
    let database = TestDatabase::new();
    let fixture = seed(&database);
    let (source_transport, source_observed) = FakeTransport::new("source");
    let (target_transport, target_observed) = FakeTransport::new("target");
    let mut workspace =
        WorkspaceRuntime::new(StorageWorker::open(database.path()).unwrap()).unwrap();
    let mut source_owner = workspace.thread_with_transport(source_transport).unwrap();
    source_owner
        .open_thread_for_agent(open_command(
            fixture.source_owner_thread.id,
            fixture.source.id,
        ))
        .await
        .unwrap();
    let mut target_owner = workspace.thread_with_transport(target_transport).unwrap();
    target_owner
        .open_thread_for_agent(open_command(
            fixture.target_owner_thread.id,
            fixture.target.id,
        ))
        .await
        .unwrap();
    let mut mentions = workspace.thread(fixture.target.id).unwrap();
    let first_id = MessageId::new();

    let first_command = mention_command(
        &fixture,
        first_id,
        fixture.mention_thread.id,
        fixture.target.id,
        BODY,
        CAPSULE,
        MENTIONED,
    );
    let first = mentions
        .mention_thread_agent(first_command.clone())
        .await
        .unwrap()
        .unwrap();

    assert!(first.membership_changed);
    assert_eq!(first.opened.thread_id, fixture.mention_thread.id);
    assert_eq!(first.opened.agent_id, fixture.target.id);
    assert_eq!(
        target_members(&database, &fixture, fixture.mention_thread.id),
        1
    );
    assert!(source_observed.lock().unwrap().messages.is_empty());
    {
        let observed = target_observed.lock().unwrap();
        assert_eq!(observed.connections.len(), 1);
        assert_eq!(observed.creates.len(), 2);
        assert_eq!(
            observed
                .messages
                .iter()
                .map(|message| message.content.as_str())
                .collect::<Vec<_>>(),
            vec![CAPSULE, BODY]
        );
        assert!(
            observed
                .messages
                .iter()
                .all(|message| message.session.binding_id == first.opened.session_binding_id)
        );
    }

    assert_eq!(
        mentions.mention_thread_agent(first_command).await.unwrap(),
        None
    );
    assert_eq!(
        target_members(&database, &fixture, fixture.mention_thread.id),
        1
    );
    {
        let observed = target_observed.lock().unwrap();
        assert_eq!(observed.creates.len(), 2);
        assert_eq!(observed.messages.len(), 2);
    }

    let second_id = MessageId::new();
    let second_body = "Second exact body";
    let second = mentions
        .mention_thread_agent(mention_command(
            &fixture,
            second_id,
            fixture.mention_thread.id,
            fixture.target.id,
            second_body,
            "unused repeat capsule",
            LATER,
        ))
        .await
        .unwrap()
        .unwrap();

    assert!(!second.membership_changed);
    assert_eq!(second.opened, first.opened);
    {
        let observed = target_observed.lock().unwrap();
        assert_eq!(observed.creates.len(), 2);
        assert_eq!(observed.messages.len(), 3);
        assert_eq!(observed.messages[2].content, second_body);
    }
    let store = SqliteStore::open(database.path()).unwrap();
    assert_eq!(
        store
            .list_messages(fixture.mention_thread.id)
            .unwrap()
            .into_iter()
            .map(|message| (
                message.id,
                message.sender_type,
                message.sender_id,
                message.body
            ))
            .collect::<Vec<_>>(),
        vec![
            (
                first_id,
                MemberType::Agent,
                fixture.source.id.to_string(),
                BODY.into()
            ),
            (
                second_id,
                MemberType::Agent,
                fixture.source.id.to_string(),
                second_body.into(),
            ),
        ]
    );
    assert_eq!(
        store.list_messages(fixture.source_owner_thread.id).unwrap(),
        vec![fixture.unrelated_message.clone()]
    );
    drop(store);

    let wrong_context_id = MessageId::new();
    assert_eq!(
        mentions
            .mention_thread_agent(mention_command(
                &fixture,
                wrong_context_id,
                fixture.other_mention_thread.id,
                fixture.target.id,
                "must not persist",
                CAPSULE,
                LATER,
            ))
            .await,
        Err(CollaborationError::ThreadAlreadyOpen)
    );
    assert!(
        SqliteStore::open(database.path())
            .unwrap()
            .get_message(wrong_context_id)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        target_members(&database, &fixture, fixture.other_mention_thread.id),
        0
    );

    mentions.shutdown(LATER.into()).await.unwrap();
    target_owner.shutdown(LATER.into()).await.unwrap();
    source_owner.shutdown(LATER.into()).await.unwrap();
    workspace.shutdown(LATER.into()).await.unwrap();
}

#[tokio::test]
async fn neutral_mismatched_and_blank_mentions_fail_before_storage_or_transport() {
    let database = TestDatabase::new();
    let fixture = seed(&database);
    let (transport, observed) = FakeTransport::new("unused");
    let mut workspace =
        WorkspaceRuntime::new(StorageWorker::open(database.path()).unwrap()).unwrap();
    let mut neutral = workspace.thread_with_transport(transport).unwrap();
    let neutral_id = MessageId::new();
    assert_eq!(
        neutral
            .mention_thread_agent(mention_command(
                &fixture,
                neutral_id,
                fixture.mention_thread.id,
                fixture.target.id,
                BODY,
                CAPSULE,
                MENTIONED,
            ))
            .await,
        Err(CollaborationError::AgentTargetNotBound)
    );
    assert!(observed.lock().unwrap().connections.is_empty());

    let mut bound = workspace.thread(fixture.target.id).unwrap();
    let mismatch_id = MessageId::new();
    assert_eq!(
        bound
            .mention_thread_agent(mention_command(
                &fixture,
                mismatch_id,
                fixture.mention_thread.id,
                fixture.source.id,
                BODY,
                CAPSULE,
                MENTIONED,
            ))
            .await,
        Err(CollaborationError::AgentNotFound(
            fixture.source.id.to_string()
        ))
    );

    for (body, capsule) in [(" ", CAPSULE), (BODY, "\t")] {
        let message_id = MessageId::new();
        assert!(matches!(
            bound
                .mention_thread_agent(mention_command(
                    &fixture,
                    message_id,
                    fixture.mention_thread.id,
                    fixture.target.id,
                    body,
                    capsule,
                    MENTIONED,
                ))
                .await,
            Err(CollaborationError::InvalidCommand(_))
        ));
        assert!(
            SqliteStore::open(database.path())
                .unwrap()
                .get_message(message_id)
                .unwrap()
                .is_none()
        );
    }
    assert_eq!(
        target_members(&database, &fixture, fixture.mention_thread.id),
        0
    );
    assert!(
        SqliteStore::open(database.path())
            .unwrap()
            .get_message(neutral_id)
            .unwrap()
            .is_none()
    );
    assert!(
        SqliteStore::open(database.path())
            .unwrap()
            .get_message(mismatch_id)
            .unwrap()
            .is_none()
    );
    workspace.shutdown(LATER.into()).await.unwrap();
}

#[tokio::test]
async fn missing_and_non_thread_mentions_keep_typed_errors_without_persistence_or_transport() {
    let database = TestDatabase::new();
    let fixture = seed(&database);
    let dm_id = SqliteStore::open(database.path())
        .unwrap()
        .get_or_create_dm("tony", fixture.target.id, CREATED)
        .unwrap()
        .id;
    let (transport, observed) = FakeTransport::new("target");
    let mut workspace =
        WorkspaceRuntime::new(StorageWorker::open(database.path()).unwrap()).unwrap();
    let mut target_owner = workspace.thread_with_transport(transport).unwrap();
    target_owner
        .open_thread_for_agent(open_command(
            fixture.target_owner_thread.id,
            fixture.target.id,
        ))
        .await
        .unwrap();
    let mut mentions = workspace.thread(fixture.target.id).unwrap();

    for thread_id in [ConversationId::new(), dm_id] {
        let message_id = MessageId::new();
        assert_eq!(
            mentions
                .mention_thread_agent(mention_command(
                    &fixture,
                    message_id,
                    thread_id,
                    fixture.target.id,
                    BODY,
                    CAPSULE,
                    MENTIONED,
                ))
                .await,
            Err(CollaborationError::ThreadNotFound(thread_id))
        );
        assert!(
            SqliteStore::open(database.path())
                .unwrap()
                .get_message(message_id)
                .unwrap()
                .is_none()
        );
    }
    {
        let observed = observed.lock().unwrap();
        assert_eq!(observed.connections.len(), 1);
        assert_eq!(observed.creates.len(), 1);
        assert!(observed.messages.is_empty());
    }

    target_owner.shutdown(LATER.into()).await.unwrap();
    workspace.shutdown(LATER.into()).await.unwrap();
}

#[tokio::test]
async fn missing_target_owner_preserves_the_join_and_message() {
    let database = TestDatabase::new();
    let fixture = seed(&database);
    let mut workspace =
        WorkspaceRuntime::<FakeTransport>::new(StorageWorker::open(database.path()).unwrap())
            .unwrap();
    let mut mentions = workspace.thread(fixture.target.id).unwrap();
    let message_id = MessageId::new();

    assert!(matches!(
        mentions
            .mention_thread_agent(mention_command(
                &fixture,
                message_id,
                fixture.mention_thread.id,
                fixture.target.id,
                BODY,
                CAPSULE,
                MENTIONED,
            ))
            .await,
        Err(CollaborationError::Runtime(message)) if message.contains("no runtime owner")
    ));

    assert_eq!(
        target_members(&database, &fixture, fixture.mention_thread.id),
        1
    );
    assert_eq!(
        SqliteStore::open(database.path())
            .unwrap()
            .get_message(message_id)
            .unwrap()
            .unwrap()
            .body,
        BODY
    );
    workspace.shutdown(LATER.into()).await.unwrap();
}

#[tokio::test]
async fn open_or_send_failure_preserves_the_join_and_message() {
    for failure in ["open", "capsule", "body"] {
        let database = TestDatabase::new();
        let fixture = seed(&database);
        let (mut transport, observed) = FakeTransport::new(failure);
        transport.create_fails_at = (failure == "open").then_some(2);
        transport.send_fails_at = match failure {
            "capsule" => Some(1),
            "body" => Some(2),
            _ => None,
        };
        let mut workspace =
            WorkspaceRuntime::new(StorageWorker::open(database.path()).unwrap()).unwrap();
        let mut target_owner = workspace.thread_with_transport(transport).unwrap();
        target_owner
            .open_thread_for_agent(open_command(
                fixture.target_owner_thread.id,
                fixture.target.id,
            ))
            .await
            .unwrap();
        let mut mentions = workspace.thread(fixture.target.id).unwrap();
        let message_id = MessageId::new();

        assert!(
            mentions
                .mention_thread_agent(mention_command(
                    &fixture,
                    message_id,
                    fixture.mention_thread.id,
                    fixture.target.id,
                    BODY,
                    CAPSULE,
                    MENTIONED,
                ))
                .await
                .is_err()
        );

        assert_eq!(
            target_members(&database, &fixture, fixture.mention_thread.id),
            1
        );
        assert_eq!(
            SqliteStore::open(database.path())
                .unwrap()
                .get_message(message_id)
                .unwrap()
                .unwrap()
                .body,
            BODY
        );
        {
            let observed = observed.lock().unwrap();
            assert_eq!(observed.creates.len(), 2);
            assert_eq!(
                observed
                    .messages
                    .iter()
                    .map(|message| message.content.as_str())
                    .collect::<Vec<_>>(),
                match failure {
                    "capsule" => vec![CAPSULE],
                    "body" => vec![CAPSULE, BODY],
                    _ => Vec::new(),
                }
            );
        }
        if failure != "open" {
            mentions.shutdown(LATER.into()).await.unwrap();
        }
        target_owner.shutdown(LATER.into()).await.unwrap();
        workspace.shutdown(LATER.into()).await.unwrap();
    }
}
