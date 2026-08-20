use july_workspace::application::{
    CollaborationError, OpenThreadForAgent, OpenedThread, ThreadRuntime,
};
use july_workspace::domain::{
    Agent, AgentId, Conversation, ConversationId, ConversationKind, Room, RoomId, SessionBinding,
    SessionBindingId, SessionBindingStatus, WorkItemId,
};
use july_workspace::runtime::{AgentThreadRuntime, StorageWorker, WorkspaceRuntime};
use july_workspace::storage::SqliteStore;
use july_workspace::transport::{
    AgentConnection, AgentTransport, CreateSession, PermissionResponse, ResumeSession, SendMessage,
    SessionCreated, SessionRef, SessionResumed, TransportError, TransportEvents,
};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const NOW: &str = "2026-08-13T10:00:00Z";
const LATER: &str = "2026-08-13T11:00:00Z";

struct TestDatabase {
    directory: PathBuf,
    path: PathBuf,
}

impl TestDatabase {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("july-thread-runtime-{}", ulid::Ulid::generate()));
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
    resumes: Vec<ResumeSession>,
    messages: Vec<SendMessage>,
    shutdowns: usize,
}

struct FakeTransport {
    events: Option<tokio::sync::mpsc::Receiver<july_workspace::transport::TransportEvent>>,
    _event_source: tokio::sync::mpsc::Sender<july_workspace::transport::TransportEvent>,
    observed: Arc<Mutex<ObservedTransport>>,
    remote_session_id: String,
    resume_lost: bool,
    connect_fails: bool,
    create_fails: bool,
}

impl FakeTransport {
    fn new(remote_session_id: &str) -> (Self, Arc<Mutex<ObservedTransport>>) {
        let (sender, receiver) = tokio::sync::mpsc::channel(4);
        let observed = Arc::new(Mutex::new(ObservedTransport::default()));
        (
            Self {
                events: Some(receiver),
                _event_source: sender,
                observed: observed.clone(),
                remote_session_id: remote_session_id.into(),
                resume_lost: false,
                connect_fails: false,
                create_fails: false,
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
        if self.connect_fails {
            Err(TransportError::Protocol("connect failed".into()))
        } else {
            Ok(())
        }
    }

    async fn create_session(
        &mut self,
        request: CreateSession,
    ) -> Result<SessionCreated, TransportError> {
        self.observed.lock().unwrap().creates.push(request.clone());
        if self.create_fails {
            return Err(TransportError::Protocol("create failed".into()));
        }
        Ok(SessionCreated {
            session: SessionRef {
                binding_id: request.binding_id,
                remote_session_id: self.remote_session_id.clone(),
            },
        })
    }

    async fn resume_session(
        &mut self,
        request: ResumeSession,
    ) -> Result<SessionResumed, TransportError> {
        self.observed.lock().unwrap().resumes.push(request.clone());
        if self.resume_lost {
            Err(TransportError::SessionLost(
                request.session.remote_session_id,
            ))
        } else {
            Ok(SessionResumed {
                session: request.session,
            })
        }
    }

    async fn send_message(&mut self, request: SendMessage) -> Result<(), TransportError> {
        self.observed.lock().unwrap().messages.push(request);
        Ok(())
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
        self.observed.lock().unwrap().shutdowns += 1;
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
    agent: Agent,
    room: Room,
    thread: Conversation,
    primary_work_id: WorkItemId,
}

fn seed(
    database: &TestDatabase,
    include_room_member: bool,
    include_thread_member: bool,
) -> Fixture {
    let agent = Agent {
        id: AgentId::new(),
        name: "codex".into(),
        project_root: "/workspace/exact thread root".into(),
        transport_type: "acp".into(),
        transport_config: json!({}),
        status: "active".into(),
        metadata: json!({}),
        created_at: NOW.into(),
        updated_at: NOW.into(),
    };
    let room = Room {
        id: RoomId::new(),
        name: format!("room-{}", ulid::Ulid::generate()),
        description: None,
        status: "active".into(),
        created_at: NOW.into(),
        updated_at: NOW.into(),
    };
    let thread = Conversation {
        id: ConversationId::new(),
        kind: ConversationKind::Thread,
        room_id: Some(room.id),
        title: Some("Focused Thread".into()),
        goal: Some("Keep the context isolated".into()),
        parent_conversation_id: None,
        origin_conversation_id: None,
        status: "open".into(),
        created_at: NOW.into(),
        updated_at: NOW.into(),
    };
    let mut store = SqliteStore::open(database.path()).unwrap();
    store.insert_agent(&agent).unwrap();
    store.create_room(&room).unwrap();
    if include_room_member {
        store.add_room_member(room.id, agent.id, None, NOW).unwrap();
    }
    let initial_agents = include_thread_member
        .then_some(agent.id)
        .into_iter()
        .collect::<Vec<_>>();
    let primary_work_id = WorkItemId::new();
    store
        .create_thread_with_primary_work(&thread, primary_work_id, "tony", &initial_agents)
        .unwrap();
    Fixture {
        agent,
        room,
        thread,
        primary_work_id,
    }
}

fn runtime(database: &TestDatabase, transport: FakeTransport) -> TestThreadRuntime {
    let root = WorkspaceRuntime::new(StorageWorker::open(database.path()).unwrap()).unwrap();
    let context = root.thread_with_transport(transport).unwrap();
    TestThreadRuntime { root, context }
}

struct TestThreadRuntime {
    root: WorkspaceRuntime<FakeTransport>,
    context: AgentThreadRuntime<FakeTransport>,
}

impl ThreadRuntime for TestThreadRuntime {
    async fn open_thread_for_agent(
        &mut self,
        command: OpenThreadForAgent,
    ) -> Result<OpenedThread, CollaborationError> {
        self.context.open_thread_for_agent(command).await
    }

    async fn shutdown(&mut self, stopped_at: String) -> Result<(), CollaborationError> {
        let context = self.context.shutdown(stopped_at.clone()).await;
        let root = self
            .root
            .shutdown(stopped_at)
            .await
            .map_err(|error| CollaborationError::Runtime(error.to_string()));
        context.and(root)
    }
}

fn command(fixture: &Fixture, opened_at: &str) -> OpenThreadForAgent {
    OpenThreadForAgent {
        thread_id: fixture.thread.id,
        agent_id: fixture.agent.id,
        opened_at: opened_at.into(),
    }
}

#[tokio::test]
async fn rejects_missing_room_and_thread_membership_before_transport() {
    let database = TestDatabase::new();
    let fixture = seed(&database, false, false);
    let (transport, observed) = FakeTransport::new("unused");
    let mut runtime = runtime(&database, transport);

    assert_eq!(
        runtime.open_thread_for_agent(command(&fixture, NOW)).await,
        Err(CollaborationError::RoomMembershipRequired {
            room_id: fixture.room.id,
            agent_id: fixture.agent.id,
        })
    );
    assert!(observed.lock().unwrap().connections.is_empty());

    let mut store = SqliteStore::open(database.path()).unwrap();
    store
        .add_room_member(fixture.room.id, fixture.agent.id, None, LATER)
        .unwrap();
    drop(store);
    assert_eq!(
        runtime
            .open_thread_for_agent(command(&fixture, LATER))
            .await,
        Err(CollaborationError::ThreadMembershipRequired {
            thread_id: fixture.thread.id,
            agent_id: fixture.agent.id,
        })
    );
    assert!(observed.lock().unwrap().connections.is_empty());
    runtime.shutdown(LATER.into()).await.unwrap();
}

#[tokio::test]
async fn rejects_inactive_parents_before_transport() {
    for (table, id, status) in [
        ("agents", 0, "inactive"),
        ("rooms", 1, "archived"),
        ("conversations", 2, "closed"),
    ] {
        let database = TestDatabase::new();
        let fixture = seed(&database, true, true);
        let row_id = match id {
            0 => fixture.agent.id.to_string(),
            1 => fixture.room.id.to_string(),
            _ => fixture.thread.id.to_string(),
        };
        rusqlite::Connection::open(database.path())
            .unwrap()
            .execute(
                &format!("UPDATE {table} SET status = ?1 WHERE id = ?2"),
                rusqlite::params![status, row_id],
            )
            .unwrap();
        let (transport, observed) = FakeTransport::new("unused");
        let mut runtime = runtime(&database, transport);

        let error = runtime
            .open_thread_for_agent(command(&fixture, LATER))
            .await
            .unwrap_err();
        assert_eq!(
            error,
            match id {
                0 => CollaborationError::AgentInactive(fixture.agent.id),
                1 => CollaborationError::RoomInactive(fixture.room.id),
                _ => CollaborationError::ThreadNotOpen(fixture.thread.id),
            }
        );
        assert!(observed.lock().unwrap().connections.is_empty());
        runtime.shutdown(LATER.into()).await.unwrap();
    }
}

#[tokio::test]
async fn rejects_missing_or_non_thread_conversations_before_transport() {
    let database = TestDatabase::new();
    let fixture = seed(&database, true, true);
    let dm = SqliteStore::open(database.path())
        .unwrap()
        .get_or_create_dm("tony", fixture.agent.id, NOW)
        .unwrap();
    for thread_id in [ConversationId::new(), dm.id] {
        let (transport, observed) = FakeTransport::new("unused");
        let mut runtime = runtime(&database, transport);

        assert_eq!(
            runtime
                .open_thread_for_agent(OpenThreadForAgent {
                    thread_id,
                    agent_id: fixture.agent.id,
                    opened_at: LATER.into(),
                })
                .await,
            Err(CollaborationError::ThreadNotFound(thread_id))
        );
        assert!(observed.lock().unwrap().connections.is_empty());
        runtime.shutdown(LATER.into()).await.unwrap();
    }
}

#[tokio::test]
async fn two_threads_create_distinct_sessions_without_injected_content() {
    let database = TestDatabase::new();
    let first = seed(&database, true, true);
    let mut store = SqliteStore::open(database.path()).unwrap();
    let second_thread = Conversation {
        id: ConversationId::new(),
        room_id: Some(first.room.id),
        title: Some("Second Thread".into()),
        ..first.thread.clone()
    };
    store
        .create_thread_with_primary_work(
            &second_thread,
            WorkItemId::new(),
            "tony",
            &[first.agent.id],
        )
        .unwrap();
    drop(store);

    let (first_transport, first_observed) = FakeTransport::new("remote-thread-one");
    let mut first_runtime = runtime(&database, first_transport);
    let first_opened = first_runtime
        .open_thread_for_agent(command(&first, NOW))
        .await
        .unwrap();
    first_runtime.shutdown(NOW.into()).await.unwrap();

    let second = Fixture {
        agent: first.agent.clone(),
        room: first.room.clone(),
        thread: second_thread,
        primary_work_id: WorkItemId::new(),
    };
    let (second_transport, second_observed) = FakeTransport::new("remote-thread-two");
    let mut second_runtime = runtime(&database, second_transport);
    let second_opened = second_runtime
        .open_thread_for_agent(command(&second, LATER))
        .await
        .unwrap();

    assert_eq!(
        first_opened,
        OpenedThread {
            thread_id: first.thread.id,
            room_id: first.room.id,
            agent_id: first.agent.id,
            session_binding_id: first_opened.session_binding_id,
        }
    );
    assert_eq!(second_opened.thread_id, second.thread.id);
    assert_ne!(
        first_opened.session_binding_id,
        second_opened.session_binding_id
    );
    for observed in [&first_observed, &second_observed] {
        let observed = observed.lock().unwrap();
        assert_eq!(
            observed.connections[0].project_root,
            PathBuf::from(&first.agent.project_root)
        );
        assert!(observed.messages.is_empty(), "Thread open injected content");
    }
    second_runtime.shutdown(LATER.into()).await.unwrap();
}

#[tokio::test]
async fn shutting_down_one_thread_keeps_the_other_thread_active() {
    let database = TestDatabase::new();
    let first = seed(&database, true, true);
    let second_thread = Conversation {
        id: ConversationId::new(),
        room_id: Some(first.room.id),
        title: Some("Second Thread".into()),
        ..first.thread.clone()
    };
    SqliteStore::open(database.path())
        .unwrap()
        .create_thread_with_primary_work(
            &second_thread,
            WorkItemId::new(),
            "tony",
            &[first.agent.id],
        )
        .unwrap();
    let second = Fixture {
        agent: first.agent.clone(),
        room: first.room.clone(),
        thread: second_thread,
        primary_work_id: WorkItemId::new(),
    };

    let (first_transport, _) = FakeTransport::new("remote-thread-one");
    let (second_transport, _) = FakeTransport::new("remote-thread-two");
    let mut first_runtime = runtime(&database, first_transport);
    let mut second_runtime = runtime(&database, second_transport);
    let first_opened = first_runtime
        .open_thread_for_agent(command(&first, NOW))
        .await
        .unwrap();
    let second_opened = second_runtime
        .open_thread_for_agent(command(&second, NOW))
        .await
        .unwrap();

    first_runtime.shutdown(LATER.into()).await.unwrap();

    let store = SqliteStore::open(database.path()).unwrap();
    assert_eq!(
        store
            .get_session_binding(first_opened.session_binding_id)
            .unwrap()
            .unwrap()
            .status,
        SessionBindingStatus::Disconnected
    );
    assert_eq!(
        store
            .get_session_binding(second_opened.session_binding_id)
            .unwrap()
            .unwrap()
            .status,
        SessionBindingStatus::Active
    );
    drop(store);
    second_runtime.shutdown(LATER.into()).await.unwrap();
}

#[tokio::test]
async fn thread_context_is_terminal_after_shutdown() {
    let database = TestDatabase::new();
    let fixture = seed(&database, true, true);
    let (transport, observed) = FakeTransport::new("remote-thread");
    let mut workspace =
        WorkspaceRuntime::new(StorageWorker::open(database.path()).unwrap()).unwrap();
    let mut context = workspace.thread_with_transport(transport).unwrap();

    let opened = context
        .open_thread_for_agent(command(&fixture, NOW))
        .await
        .unwrap();
    context.shutdown(LATER.into()).await.unwrap();
    context.shutdown(LATER.into()).await.unwrap();
    let binding = SqliteStore::open(database.path())
        .unwrap()
        .get_session_binding(opened.session_binding_id)
        .unwrap()
        .unwrap();

    assert_eq!(
        context
            .open_thread_for_agent(command(&fixture, LATER))
            .await,
        Err(CollaborationError::ContextStopped)
    );
    assert_eq!(observed.lock().unwrap().creates.len(), 1);
    assert!(observed.lock().unwrap().resumes.is_empty());
    assert_eq!(
        SqliteStore::open(database.path())
            .unwrap()
            .get_session_binding(opened.session_binding_id)
            .unwrap(),
        Some(binding)
    );

    workspace.shutdown(LATER.into()).await.unwrap();
}

#[tokio::test]
async fn failed_thread_detach_remains_terminal_and_can_be_retried() {
    let database = TestDatabase::new();
    let fixture = seed(&database, true, true);
    let (transport, _) = FakeTransport::new("remote-thread");
    let mut workspace =
        WorkspaceRuntime::new(StorageWorker::open(database.path()).unwrap()).unwrap();
    let mut context = workspace.thread_with_transport(transport).unwrap();
    let opened = context
        .open_thread_for_agent(command(&fixture, NOW))
        .await
        .unwrap();
    let lock = rusqlite::Connection::open(database.path()).unwrap();
    lock.execute_batch("BEGIN EXCLUSIVE").unwrap();

    assert!(matches!(
        context.shutdown(LATER.into()).await,
        Err(CollaborationError::Runtime(_))
    ));
    assert_eq!(
        context
            .open_thread_for_agent(command(&fixture, LATER))
            .await,
        Err(CollaborationError::ContextStopped)
    );

    lock.execute_batch("ROLLBACK").unwrap();
    context.shutdown(LATER.into()).await.unwrap();
    context.shutdown(LATER.into()).await.unwrap();
    assert_eq!(
        SqliteStore::open(database.path())
            .unwrap()
            .get_session_binding(opened.session_binding_id)
            .unwrap()
            .unwrap()
            .status,
        SessionBindingStatus::Disconnected
    );

    workspace.shutdown(LATER.into()).await.unwrap();
}

#[tokio::test]
async fn transport_failure_does_not_roll_back_the_thread_aggregate() {
    for failure in ["connect", "create"] {
        let database = TestDatabase::new();
        let fixture = seed(&database, true, true);
        let (mut transport, observed) = FakeTransport::new("unused");
        transport.connect_fails = failure == "connect";
        transport.create_fails = failure == "create";
        let mut runtime = runtime(&database, transport);

        assert!(
            runtime
                .open_thread_for_agent(command(&fixture, LATER))
                .await
                .is_err()
        );
        let store = SqliteStore::open(database.path()).unwrap();
        assert_eq!(
            store.get_thread(fixture.thread.id).unwrap(),
            Some(fixture.thread.clone())
        );
        assert_eq!(
            store
                .get_work_item(fixture.primary_work_id)
                .unwrap()
                .unwrap()
                .conversation_id,
            fixture.thread.id
        );
        assert_eq!(
            store
                .list_conversation_members(fixture.thread.id)
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            observed.lock().unwrap().shutdowns,
            usize::from(failure == "connect")
        );
        runtime.shutdown(LATER.into()).await.unwrap();
        assert_eq!(observed.lock().unwrap().shutdowns, 1);
    }
}

#[tokio::test]
async fn removal_blocks_the_next_open_before_transport() {
    let database = TestDatabase::new();
    let fixture = seed(&database, true, true);
    let (transport, _) = FakeTransport::new("remote-thread");
    let mut first = runtime(&database, transport);
    first
        .open_thread_for_agent(command(&fixture, NOW))
        .await
        .unwrap();
    first.shutdown(NOW.into()).await.unwrap();
    SqliteStore::open(database.path())
        .unwrap()
        .remove_thread_member(fixture.thread.id, fixture.agent.id, LATER)
        .unwrap();

    let (transport, observed) = FakeTransport::new("unused");
    let mut second = runtime(&database, transport);
    assert_eq!(
        second.open_thread_for_agent(command(&fixture, LATER)).await,
        Err(CollaborationError::ThreadMembershipRequired {
            thread_id: fixture.thread.id,
            agent_id: fixture.agent.id,
        })
    );
    assert!(observed.lock().unwrap().connections.is_empty());
    second.shutdown(LATER.into()).await.unwrap();
}

#[tokio::test]
async fn shutdown_is_idempotent_and_next_open_resumes_the_same_binding() {
    let database = TestDatabase::new();
    let fixture = seed(&database, true, true);
    let (first_transport, first_observed) = FakeTransport::new("remote-thread");
    let mut first = runtime(&database, first_transport);
    let opened = first
        .open_thread_for_agent(command(&fixture, NOW))
        .await
        .unwrap();
    first.shutdown(NOW.into()).await.unwrap();
    first.shutdown(NOW.into()).await.unwrap();
    assert_eq!(first_observed.lock().unwrap().shutdowns, 1);

    let stored = SqliteStore::open(database.path())
        .unwrap()
        .get_latest_session_binding(fixture.thread.id, fixture.agent.id)
        .unwrap()
        .unwrap();
    assert_eq!(stored.status, SessionBindingStatus::Disconnected);

    let (second_transport, second_observed) = FakeTransport::new("unused");
    let mut second = runtime(&database, second_transport);
    let resumed = second
        .open_thread_for_agent(command(&fixture, LATER))
        .await
        .unwrap();
    assert_eq!(resumed.session_binding_id, opened.session_binding_id);
    {
        let observed = second_observed.lock().unwrap();
        assert!(observed.creates.is_empty());
        assert_eq!(
            observed.resumes[0].session.binding_id,
            opened.session_binding_id
        );
        assert_eq!(
            observed.resumes[0].project_root,
            PathBuf::from(&fixture.agent.project_root)
        );
    }
    second.shutdown(LATER.into()).await.unwrap();
}

#[tokio::test]
async fn missing_remote_and_terminal_bindings_are_not_replaced() {
    for (status, remote_session_id, expected) in [
        (
            SessionBindingStatus::Active,
            None,
            CollaborationError::SessionLost,
        ),
        (
            SessionBindingStatus::Lost,
            Some("remote-lost"),
            CollaborationError::SessionLost,
        ),
        (
            SessionBindingStatus::Closed,
            Some("remote-closed"),
            CollaborationError::SessionUnavailable(SessionBindingStatus::Closed),
        ),
    ] {
        let database = TestDatabase::new();
        let fixture = seed(&database, true, true);
        let binding = SessionBinding {
            id: SessionBindingId::new(),
            conversation_id: fixture.thread.id,
            agent_id: fixture.agent.id,
            transport_type: "acp".into(),
            remote_session_id: remote_session_id.map(str::to_owned),
            generation: 1,
            status,
            created_at: NOW.into(),
            last_used_at: NOW.into(),
        };
        SqliteStore::open(database.path())
            .unwrap()
            .insert_session_binding(&binding)
            .unwrap();
        let (transport, observed) = FakeTransport::new("replacement");
        let mut runtime = runtime(&database, transport);

        assert_eq!(
            runtime
                .open_thread_for_agent(command(&fixture, LATER))
                .await,
            Err(expected)
        );
        assert!(observed.lock().unwrap().connections.is_empty());
        runtime.shutdown(LATER.into()).await.unwrap();
        let latest = SqliteStore::open(database.path())
            .unwrap()
            .get_latest_session_binding(fixture.thread.id, fixture.agent.id)
            .unwrap()
            .unwrap();
        assert_eq!(latest.id, binding.id);
        assert_eq!(latest.generation, 1);
        assert_eq!(
            latest.status,
            if status == SessionBindingStatus::Active {
                SessionBindingStatus::Lost
            } else {
                status
            }
        );
    }
}

#[tokio::test]
async fn provider_missing_remote_marks_the_resumed_binding_lost() {
    let database = TestDatabase::new();
    let fixture = seed(&database, true, true);
    let binding = SessionBinding {
        id: SessionBindingId::new(),
        conversation_id: fixture.thread.id,
        agent_id: fixture.agent.id,
        transport_type: "acp".into(),
        remote_session_id: Some("remote-missing".into()),
        generation: 1,
        status: SessionBindingStatus::Disconnected,
        created_at: NOW.into(),
        last_used_at: NOW.into(),
    };
    SqliteStore::open(database.path())
        .unwrap()
        .insert_session_binding(&binding)
        .unwrap();
    let (mut transport, observed) = FakeTransport::new("replacement");
    transport.resume_lost = true;
    let mut runtime = runtime(&database, transport);

    assert_eq!(
        runtime
            .open_thread_for_agent(command(&fixture, LATER))
            .await,
        Err(CollaborationError::SessionLost)
    );
    assert_eq!(observed.lock().unwrap().resumes.len(), 1);
    runtime.shutdown(LATER.into()).await.unwrap();
    let latest = SqliteStore::open(database.path())
        .unwrap()
        .get_latest_session_binding(fixture.thread.id, fixture.agent.id)
        .unwrap()
        .unwrap();
    assert_eq!(latest.id, binding.id);
    assert_eq!(latest.status, SessionBindingStatus::Lost);
}
