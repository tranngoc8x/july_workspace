use july_workspace::application::{
    DirectMessageError, DirectMessageEvent, DirectMessageRuntime, DirectMessageRuntimeEvent,
    DirectMessageService, OpenThreadForAgent, ThreadRuntime,
};
use july_workspace::domain::{
    Agent, AgentId, Conversation, ConversationId, ConversationKind, MemberType, Message,
    PermissionOption, PermissionOutcome, Room, RoomId, SessionBinding, SessionBindingStatus,
    WorkItemId,
};
use july_workspace::runtime::{RuntimeError, StorageWorker, WorkspaceRuntime};
use july_workspace::storage::SqliteStore;
use july_workspace::transport::{
    AgentConnection, AgentTransport, CreateSession, PermissionRequest, PermissionRequestId,
    PermissionResponse, ResumeSession, SendMessage, SessionCreated, SessionRef, SessionResumed,
    TransportError, TransportEvent, TransportEvents,
};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const NOW: &str = "2026-08-17T10:00:00Z";

#[test]
fn workspace_new_outside_tokio_returns_typed_error_without_panicking() {
    let database = TestDatabase::new();
    let storage = StorageWorker::open(database.path()).unwrap();

    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        WorkspaceRuntime::<FakeTransport>::new(storage)
    }));

    assert!(outcome.is_ok(), "WorkspaceRuntime::new must not panic");
    assert!(matches!(
        outcome.unwrap(),
        Err(RuntimeError::TokioRuntimeUnavailable)
    ));
}

struct TestDatabase {
    directory: PathBuf,
    path: PathBuf,
}

impl TestDatabase {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("july-workspace-runtime-{}", ulid::Ulid::generate()));
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
    connects: usize,
    subscribes: usize,
    creates: Vec<CreateSession>,
    resumes: Vec<ResumeSession>,
    messages: Vec<SendMessage>,
    permissions: Vec<PermissionResponse>,
    shutdowns: usize,
}

struct FakeTransport {
    events: Option<tokio::sync::mpsc::Receiver<july_workspace::transport::TransportEvent>>,
    observed: Arc<Mutex<ObservedTransport>>,
}

impl FakeTransport {
    fn new() -> (
        Self,
        tokio::sync::mpsc::Sender<TransportEvent>,
        Arc<Mutex<ObservedTransport>>,
    ) {
        let (sender, receiver) = tokio::sync::mpsc::channel(256);
        let observed = Arc::new(Mutex::new(ObservedTransport::default()));
        (
            Self {
                events: Some(receiver),
                observed: observed.clone(),
            },
            sender,
            observed,
        )
    }
}

impl AgentTransport for FakeTransport {
    async fn connect(&mut self, _agent: &AgentConnection) -> Result<(), TransportError> {
        self.observed.lock().unwrap().connects += 1;
        Ok(())
    }

    async fn create_session(
        &mut self,
        request: CreateSession,
    ) -> Result<SessionCreated, TransportError> {
        self.observed.lock().unwrap().creates.push(request.clone());
        Ok(SessionCreated {
            session: SessionRef {
                binding_id: request.binding_id,
                remote_session_id: format!("remote-{}", request.binding_id),
            },
        })
    }

    async fn resume_session(
        &mut self,
        request: ResumeSession,
    ) -> Result<SessionResumed, TransportError> {
        self.observed.lock().unwrap().resumes.push(request.clone());
        Ok(SessionResumed {
            session: request.session,
        })
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
        response: PermissionResponse,
    ) -> Result<(), TransportError> {
        self.observed.lock().unwrap().permissions.push(response);
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
        self.observed.lock().unwrap().subscribes += 1;
        self.events
            .take()
            .map(TransportEvents::new)
            .ok_or(TransportError::AlreadySubscribed)
    }
}

#[tokio::test]
async fn duplicate_agent_is_rejected_before_second_connect() {
    let database = TestDatabase::new();
    let (agent, _dm_id, _thread_id) = seed_contexts(&database);
    let (first, _first_events, first_observed) = FakeTransport::new();
    let (second, _second_events, second_observed) = FakeTransport::new();
    let mut runtime = WorkspaceRuntime::new(StorageWorker::open(database.path()).unwrap()).unwrap();
    let mut first = DirectMessageService::new(runtime.direct_message(first).unwrap());
    first
        .open("tony".into(), agent.name.clone(), NOW.into())
        .await
        .unwrap();
    let mut second = DirectMessageService::new(runtime.direct_message(second).unwrap());
    assert!(matches!(
        second
            .open("another-user".into(), agent.name.clone(), NOW.into())
            .await,
        Err(DirectMessageError::Runtime(message)) if message.contains("already has a runtime owner")
    ));

    assert_eq!(first_observed.lock().unwrap().connects, 1);
    assert_eq!(first_observed.lock().unwrap().subscribes, 1);
    assert_eq!(second_observed.lock().unwrap().connects, 0);
    assert_eq!(second_observed.lock().unwrap().subscribes, 0);
    first.shutdown(NOW.into()).await.unwrap();
    runtime.shutdown(NOW.into()).await.unwrap();
}

fn seed_contexts(database: &TestDatabase) -> (Agent, ConversationId, ConversationId) {
    let agent = Agent {
        id: AgentId::new(),
        name: "codex".into(),
        project_root: "/workspace/project".into(),
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
        title: Some("Thread".into()),
        goal: Some("Keep context isolated".into()),
        parent_conversation_id: None,
        origin_conversation_id: None,
        status: "open".into(),
        created_at: NOW.into(),
        updated_at: NOW.into(),
    };
    let mut store = SqliteStore::open(database.path()).unwrap();
    store.insert_agent(&agent).unwrap();
    let dm = store.get_or_create_dm("tony", agent.id, NOW).unwrap();
    store.create_room(&room).unwrap();
    store.add_room_member(room.id, agent.id, None, NOW).unwrap();
    store
        .create_thread_with_primary_work(&thread, WorkItemId::new(), "tony", &[agent.id])
        .unwrap();
    (agent, dm.id, thread.id)
}

fn binding(conversation_id: ConversationId, agent_id: AgentId) -> SessionBinding {
    SessionBinding {
        id: Default::default(),
        conversation_id,
        agent_id,
        transport_type: "acp".into(),
        remote_session_id: None,
        generation: 1,
        status: SessionBindingStatus::Active,
        created_at: NOW.into(),
        last_used_at: NOW.into(),
    }
}

#[tokio::test]
async fn dm_and_thread_share_one_owner_and_route_events_by_binding() {
    let database = TestDatabase::new();
    let (agent, _dm_id, thread_id) = seed_contexts(&database);
    let (transport, events, observed) = FakeTransport::new();
    let mut runtime = WorkspaceRuntime::new(StorageWorker::open(database.path()).unwrap()).unwrap();
    let mut dm = DirectMessageService::new(runtime.direct_message(transport).unwrap());
    dm.open("tony".into(), agent.name.clone(), NOW.into())
        .await
        .unwrap();
    let mut thread = runtime.thread(agent.id).unwrap();
    let opened_thread = thread
        .open_thread_for_agent(OpenThreadForAgent {
            thread_id,
            agent_id: agent.id,
            opened_at: NOW.into(),
        })
        .await
        .unwrap();
    let creates = observed.lock().unwrap().creates.clone();
    assert_eq!(creates.len(), 2);
    let dm_session = SessionRef {
        binding_id: creates[0].binding_id,
        remote_session_id: format!("remote-{}", creates[0].binding_id),
    };
    let thread_session = SessionRef {
        binding_id: opened_thread.session_binding_id,
        remote_session_id: format!("remote-{}", opened_thread.session_binding_id),
    };
    assert_ne!(dm_session.binding_id, thread_session.binding_id);
    assert_eq!(observed.lock().unwrap().connects, 1);
    assert_eq!(observed.lock().unwrap().subscribes, 1);

    events
        .send(TransportEvent::AgentTextDelta {
            session: dm_session,
            text: "dm".into(),
        })
        .await
        .unwrap();
    let dm_permission = PermissionRequest {
        session: SessionRef {
            binding_id: creates[0].binding_id,
            remote_session_id: format!("remote-{}", creates[0].binding_id),
        },
        request_id: PermissionRequestId::from("dm-permission"),
        options: vec![PermissionOption {
            id: "allow".into(),
            label: "Allow".into(),
        }],
    };
    events
        .send(TransportEvent::PermissionRequested(dm_permission))
        .await
        .unwrap();
    let permission = PermissionRequest {
        session: thread_session.clone(),
        request_id: PermissionRequestId::from("thread-permission"),
        options: vec![PermissionOption {
            id: "allow".into(),
            label: "Allow".into(),
        }],
    };
    events
        .send(TransportEvent::PermissionRequested(permission.clone()))
        .await
        .unwrap();
    events
        .send(TransportEvent::AgentTextDelta {
            session: thread_session.clone(),
            text: "thread".into(),
        })
        .await
        .unwrap();

    assert_eq!(
        dm.next_event(NOW.into()).await.unwrap(),
        Some(DirectMessageEvent::TextDelta("dm".into()))
    );
    let dm_request_id = match dm.next_event(NOW.into()).await.unwrap() {
        Some(DirectMessageEvent::PermissionRequested { request_id, .. }) => request_id,
        other => panic!("unexpected DM event: {other:?}"),
    };
    assert!(matches!(
        thread
            .respond_permission(
                PermissionRequestId::from("dm-permission"),
                PermissionOutcome::Selected("allow".into()),
                NOW.into(),
            )
            .await,
        Err(RuntimeError::PermissionRequestNotFound(id)) if id == "dm-permission"
    ));
    assert!(observed.lock().unwrap().permissions.is_empty());
    dm.respond_permission(
        dm_request_id,
        PermissionOutcome::Selected("allow".into()),
        NOW.into(),
    )
    .await
    .unwrap();
    assert_eq!(observed.lock().unwrap().permissions.len(), 1);
    assert_eq!(
        thread.next_event().await.unwrap(),
        Some(TransportEvent::PermissionRequested(permission.clone()))
    );
    assert_eq!(
        thread.next_event().await.unwrap(),
        Some(TransportEvent::AgentTextDelta {
            session: thread_session,
            text: "thread".into(),
        })
    );

    dm.shutdown(NOW.into()).await.unwrap();
    thread.shutdown(NOW.into()).await.unwrap();
    runtime.shutdown(NOW.into()).await.unwrap();
}

#[tokio::test]
async fn detaching_one_session_does_not_stop_or_mutate_the_other() {
    let database = TestDatabase::new();
    let (agent, dm_id, thread_id) = seed_contexts(&database);
    let (transport, events, observed) = FakeTransport::new();
    let mut runtime = WorkspaceRuntime::new(StorageWorker::open(database.path()).unwrap()).unwrap();
    let mut dm = DirectMessageService::new(runtime.direct_message(transport).unwrap());
    dm.open("tony".into(), agent.name.clone(), NOW.into())
        .await
        .unwrap();
    let mut thread = runtime.thread(agent.id).unwrap();
    let opened_thread = thread
        .open_thread_for_agent(OpenThreadForAgent {
            thread_id,
            agent_id: agent.id,
            opened_at: NOW.into(),
        })
        .await
        .unwrap();
    let creates = observed.lock().unwrap().creates.clone();
    let dm_session = SessionRef {
        binding_id: creates[0].binding_id,
        remote_session_id: format!("remote-{}", creates[0].binding_id),
    };
    let thread_session = SessionRef {
        binding_id: opened_thread.session_binding_id,
        remote_session_id: format!("remote-{}", opened_thread.session_binding_id),
    };
    let dm_binding_id = dm_session.binding_id;
    let thread_binding_id = thread_session.binding_id;
    let option = PermissionOption {
        id: "allow".into(),
        label: "Allow".into(),
    };
    events
        .send(TransportEvent::PermissionRequested(PermissionRequest {
            session: dm_session.clone(),
            request_id: PermissionRequestId::from("dm-pending"),
            options: vec![option.clone()],
        }))
        .await
        .unwrap();
    events
        .send(TransportEvent::PermissionRequested(PermissionRequest {
            session: thread_session.clone(),
            request_id: PermissionRequestId::from("thread-pending"),
            options: vec![option],
        }))
        .await
        .unwrap();
    assert!(matches!(
        dm.next_event(NOW.into()).await,
        Ok(Some(DirectMessageEvent::PermissionRequested { .. }))
    ));
    assert!(matches!(
        thread.next_event().await,
        Ok(Some(TransportEvent::PermissionRequested(_)))
    ));

    dm.shutdown(NOW.into()).await.unwrap();
    events
        .send(TransportEvent::SessionLost {
            session: dm_session.clone(),
        })
        .await
        .unwrap();
    events
        .send(TransportEvent::PermissionRequested(PermissionRequest {
            session: dm_session,
            request_id: PermissionRequestId::from("late-detached"),
            options: vec![],
        }))
        .await
        .unwrap();
    events
        .send(TransportEvent::AgentTextDelta {
            session: thread_session.clone(),
            text: "event barrier".into(),
        })
        .await
        .unwrap();
    assert_eq!(
        thread.next_event().await.unwrap(),
        Some(TransportEvent::AgentTextDelta {
            session: thread_session,
            text: "event barrier".into(),
        })
    );
    thread.send_exact("still active".into()).await.unwrap();

    assert_eq!(observed.lock().unwrap().shutdowns, 0);
    assert_eq!(observed.lock().unwrap().messages.len(), 1);
    assert_eq!(
        observed.lock().unwrap().messages[0].session.binding_id,
        thread_binding_id
    );
    let store = SqliteStore::open(database.path()).unwrap();
    assert_eq!(
        store
            .get_latest_session_binding(dm_id, agent.id)
            .unwrap()
            .unwrap()
            .status,
        SessionBindingStatus::Disconnected
    );
    assert_eq!(
        store
            .get_latest_session_binding(thread_id, agent.id)
            .unwrap()
            .unwrap()
            .status,
        SessionBindingStatus::Active
    );
    drop(store);
    runtime.shutdown(NOW.into()).await.unwrap();
    let connection = rusqlite::Connection::open(database.path()).unwrap();
    let decisions = connection
        .prepare(
            "SELECT session_binding_id, correlation_id, outcome
             FROM permission_decisions ORDER BY correlation_id",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        decisions,
        vec![
            (
                dm_binding_id.to_string(),
                "dm-pending".into(),
                "cancelled".into(),
            ),
            (
                thread_binding_id.to_string(),
                "thread-pending".into(),
                "cancelled".into(),
            ),
        ]
    );
    drop(connection);
}

#[tokio::test]
async fn failed_permission_audit_is_retried_by_root_shutdown() {
    let database = TestDatabase::new();
    let (agent, dm_id, _) = seed_contexts(&database);
    let (transport, events, observed) = FakeTransport::new();
    let mut runtime = WorkspaceRuntime::new(StorageWorker::open(database.path()).unwrap()).unwrap();
    let mut dm = runtime.direct_message(transport).unwrap();
    dm.open("tony".into(), agent.name.clone(), NOW.into())
        .await
        .unwrap();
    let binding_id = observed.lock().unwrap().creates[0].binding_id;
    let session = SessionRef {
        binding_id,
        remote_session_id: format!("remote-{binding_id}"),
    };
    events
        .send(TransportEvent::PermissionRequested(PermissionRequest {
            session,
            request_id: PermissionRequestId::from("retry-audit"),
            options: vec![],
        }))
        .await
        .unwrap();
    assert!(matches!(
        dm.next_runtime_event(NOW.into()).await.unwrap(),
        Some(DirectMessageRuntimeEvent::PermissionRequested { .. })
    ));

    let connection = rusqlite::Connection::open(database.path()).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER fail_permission_audit
             BEFORE INSERT ON permission_decisions
             BEGIN SELECT RAISE(ABORT, 'fixture audit failure'); END;",
        )
        .unwrap();
    assert!(dm.shutdown(NOW.into()).await.is_err());
    connection
        .execute_batch("DROP TRIGGER fail_permission_audit")
        .unwrap();

    runtime.shutdown(NOW.into()).await.unwrap();
    let store = SqliteStore::open(database.path()).unwrap();
    assert_eq!(
        store
            .get_latest_session_binding(dm_id, agent.id)
            .unwrap()
            .unwrap()
            .status,
        SessionBindingStatus::Disconnected
    );
    drop(store);
    let audited: i64 = connection
        .query_row(
            "SELECT count(*) FROM permission_decisions
             WHERE correlation_id = 'retry-audit' AND outcome = 'cancelled'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(audited, 1);
}

#[tokio::test]
async fn full_context_event_queue_does_not_block_detach_or_root_shutdown() {
    let database = TestDatabase::new();
    let (agent, _dm_id, thread_id) = seed_contexts(&database);
    let (transport, events, observed) = FakeTransport::new();
    let mut runtime = WorkspaceRuntime::new(StorageWorker::open(database.path()).unwrap()).unwrap();
    let mut dm = DirectMessageService::new(runtime.direct_message(transport).unwrap());
    dm.open("tony".into(), agent.name.clone(), NOW.into())
        .await
        .unwrap();
    let mut thread = runtime.thread(agent.id).unwrap();
    thread
        .open_thread_for_agent(OpenThreadForAgent {
            thread_id,
            agent_id: agent.id,
            opened_at: NOW.into(),
        })
        .await
        .unwrap();
    let dm_binding_id = observed.lock().unwrap().creates[0].binding_id;
    let dm_session = SessionRef {
        binding_id: dm_binding_id,
        remote_session_id: format!("remote-{dm_binding_id}"),
    };
    for _ in 0..100 {
        events
            .send(TransportEvent::TurnStarted {
                session: dm_session.clone(),
            })
            .await
            .unwrap();
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        thread.shutdown(NOW.into()),
    )
    .await
    .expect("detach must remain processable while a sibling event queue is full")
    .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        runtime.shutdown(NOW.into()),
    )
    .await
    .expect("root shutdown must remain processable while a context queue is full")
    .unwrap();
    assert_eq!(observed.lock().unwrap().shutdowns, 1);
}

#[tokio::test]
async fn workspace_shutdown_is_terminal_for_new_and_existing_context_handles() {
    let database = TestDatabase::new();
    let (agent, _dm_id, _thread_id) = seed_contexts(&database);
    let (transport, _events, observed) = FakeTransport::new();
    let (late_transport, _late_events, late_observed) = FakeTransport::new();
    let mut runtime = WorkspaceRuntime::new(StorageWorker::open(database.path()).unwrap()).unwrap();
    let mut existing = DirectMessageService::new(runtime.direct_message(transport).unwrap());

    runtime.shutdown(NOW.into()).await.unwrap();

    assert!(matches!(
        runtime.direct_message(late_transport),
        Err(RuntimeError::WorkspaceStopped)
    ));
    assert!(matches!(
        existing
            .open("tony".into(), agent.name.clone(), NOW.into())
            .await,
        Err(DirectMessageError::Runtime(message)) if message.contains("workspace runtime is stopped")
    ));
    assert_eq!(observed.lock().unwrap().connects, 0);
    assert_eq!(late_observed.lock().unwrap().connects, 0);
}

#[tokio::test]
async fn duplicate_attached_binding_is_rejected_before_resume_or_route_replacement() {
    let database = TestDatabase::new();
    let (agent, _dm_id, _thread_id) = seed_contexts(&database);
    let (transport, _events, observed) = FakeTransport::new();
    let mut runtime = WorkspaceRuntime::new(StorageWorker::open(database.path()).unwrap()).unwrap();
    let mut first = DirectMessageService::new(runtime.direct_message(transport).unwrap());
    first
        .open("tony".into(), agent.name.clone(), NOW.into())
        .await
        .unwrap();
    let mut duplicate =
        DirectMessageService::new(runtime.direct_message_for_agent(agent.id).unwrap());

    assert_eq!(
        duplicate
            .open("tony".into(), agent.name.clone(), NOW.into())
            .await,
        Err(DirectMessageError::SessionAlreadyAttached(
            observed.lock().unwrap().creates[0].binding_id,
        ))
    );
    assert_eq!(observed.lock().unwrap().creates.len(), 1);
    assert!(observed.lock().unwrap().resumes.is_empty());
    first
        .send_message("still attached".into(), NOW.into())
        .await
        .unwrap();
    assert_eq!(observed.lock().unwrap().messages.len(), 1);

    duplicate.shutdown(NOW.into()).await.unwrap();
    first.shutdown(NOW.into()).await.unwrap();
    runtime.shutdown(NOW.into()).await.unwrap();
}

#[tokio::test]
async fn owner_event_failure_still_shuts_transport_exactly_once() {
    let database = TestDatabase::new();
    let (agent, _dm_id, _thread_id) = seed_contexts(&database);
    let (transport, events, observed) = FakeTransport::new();
    let mut runtime = WorkspaceRuntime::new(StorageWorker::open(database.path()).unwrap()).unwrap();
    let mut dm = DirectMessageService::new(runtime.direct_message(transport).unwrap());
    dm.open("tony".into(), agent.name.clone(), NOW.into())
        .await
        .unwrap();
    let binding_id = observed.lock().unwrap().creates[0].binding_id;
    rusqlite::Connection::open(database.path())
        .unwrap()
        .execute(
            "DELETE FROM session_bindings WHERE id = ?1",
            [binding_id.to_string()],
        )
        .unwrap();
    events
        .send(TransportEvent::SessionLost {
            session: SessionRef {
                binding_id,
                remote_session_id: format!("remote-{binding_id}"),
            },
        })
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    assert_eq!(observed.lock().unwrap().shutdowns, 1);
    assert!(matches!(
        runtime.shutdown(NOW.into()).await,
        Err(RuntimeError::SessionBindingNotFound(id)) if id == binding_id
    ));
    assert_eq!(observed.lock().unwrap().shutdowns, 1);
    runtime.shutdown(NOW.into()).await.unwrap();
}

#[tokio::test]
async fn workspace_shutdown_stops_owner_once_and_disconnects_all_owned_bindings() {
    let database = TestDatabase::new();
    let (agent, dm_id, thread_id) = seed_contexts(&database);
    let foreign = Agent {
        id: AgentId::new(),
        name: "foreign".into(),
        project_root: "/workspace/foreign".into(),
        transport_type: "acp".into(),
        transport_config: json!({}),
        status: "active".into(),
        metadata: json!({}),
        created_at: NOW.into(),
        updated_at: NOW.into(),
    };
    let foreign_dm = {
        let mut store = SqliteStore::open(database.path()).unwrap();
        store.insert_agent(&foreign).unwrap();
        let conversation = store.get_or_create_dm("tony", foreign.id, NOW).unwrap();
        let mut foreign_binding = binding(conversation.id, foreign.id);
        foreign_binding.remote_session_id = Some("foreign-remote".into());
        store.insert_session_binding(&foreign_binding).unwrap();
        conversation.id
    };
    let (transport, events, observed) = FakeTransport::new();
    let mut runtime = WorkspaceRuntime::new(StorageWorker::open(database.path()).unwrap()).unwrap();
    let mut storage_probe = runtime.direct_message_for_agent(agent.id).unwrap();
    let mut dm = DirectMessageService::new(runtime.direct_message(transport).unwrap());
    dm.open("tony".into(), agent.name.clone(), NOW.into())
        .await
        .unwrap();
    let mut thread = runtime.thread(agent.id).unwrap();
    thread
        .open_thread_for_agent(OpenThreadForAgent {
            thread_id,
            agent_id: agent.id,
            opened_at: NOW.into(),
        })
        .await
        .unwrap();

    let _live_events = events;
    runtime.shutdown(NOW.into()).await.unwrap();
    runtime.shutdown(NOW.into()).await.unwrap();

    assert!(matches!(
        storage_probe
            .persist_message(Message {
                id: Default::default(),
                conversation_id: dm_id,
                sender_type: MemberType::User,
                sender_id: "tony".into(),
                body: "storage must be stopped".into(),
                reply_to: None,
                metadata: json!({}),
                created_at: NOW.into(),
            })
            .await,
        Err(DirectMessageError::Runtime(message))
            if message == RuntimeError::ChannelClosed.to_string()
    ));

    assert_eq!(observed.lock().unwrap().shutdowns, 1);
    let store = SqliteStore::open(database.path()).unwrap();
    for conversation_id in [dm_id, thread_id] {
        assert_eq!(
            store
                .get_latest_session_binding(conversation_id, agent.id)
                .unwrap()
                .unwrap()
                .status,
            SessionBindingStatus::Disconnected
        );
    }
    assert_eq!(
        store
            .get_latest_session_binding(foreign_dm, foreign.id)
            .unwrap()
            .unwrap()
            .status,
        SessionBindingStatus::Active
    );
}
