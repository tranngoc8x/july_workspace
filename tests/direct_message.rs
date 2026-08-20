use july_workspace::application::{
    DirectMessageError, DirectMessageEvent, DirectMessagePermissionRequestId, DirectMessageRuntime,
    DirectMessageRuntimeEvent, DirectMessageService, OpenAgentDirectMessage, OpenedDirectMessage,
};
use july_workspace::domain::{
    Agent, AgentId, ConversationId, MemberType, Message, PermissionOption, PermissionOutcome,
    SessionBinding, SessionBindingId, SessionBindingStatus,
};
use july_workspace::runtime::{AgentDirectMessageRuntime, StorageWorker, WorkspaceRuntime};
use july_workspace::storage::SqliteStore;
use july_workspace::transport::{
    AgentConnection, AgentTransport, CreateSession, PermissionRequest, PermissionRequestId,
    PermissionResponse, ResumeSession, SendMessage, SessionCreated, SessionRef, SessionResumed,
    TransportError, TransportEvent, TransportEvents,
};
use serde_json::json;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const NOW: &str = "2026-08-11T10:00:00Z";
const LATER: &str = "2026-08-11T11:00:00Z";

struct TestDatabase {
    directory: PathBuf,
    path: PathBuf,
}

impl TestDatabase {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("july-dm-runtime-{}", ulid::Ulid::generate()));
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
    permissions: Vec<PermissionResponse>,
    cancellations: Vec<SessionRef>,
    shutdowns: usize,
}

struct FakeTransport {
    events: Option<tokio::sync::mpsc::Receiver<TransportEvent>>,
    observed: Arc<Mutex<ObservedTransport>>,
    resume_lost: bool,
}

impl FakeTransport {
    fn new() -> (
        Self,
        tokio::sync::mpsc::Sender<TransportEvent>,
        Arc<Mutex<ObservedTransport>>,
    ) {
        let (sender, receiver) = tokio::sync::mpsc::channel(16);
        let observed = Arc::new(Mutex::new(ObservedTransport::default()));
        (
            Self {
                events: Some(receiver),
                observed: observed.clone(),
                resume_lost: false,
            },
            sender,
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
        self.observed.lock().unwrap().creates.push(request.clone());
        Ok(SessionCreated {
            session: SessionRef {
                binding_id: request.binding_id,
                remote_session_id: "remote-dm".into(),
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

    async fn cancel_turn(&mut self, session: SessionRef) -> Result<(), TransportError> {
        self.observed.lock().unwrap().cancellations.push(session);
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
        self.events
            .take()
            .map(TransportEvents::new)
            .ok_or(TransportError::AlreadySubscribed)
    }
}

fn seed_agent(database: &TestDatabase) -> Agent {
    let agent = Agent {
        id: Default::default(),
        name: "codex".into(),
        project_root: "/workspace/exact root".into(),
        transport_type: "acp".into(),
        transport_config: json!({}),
        status: "active".into(),
        metadata: json!({}),
        created_at: NOW.into(),
        updated_at: NOW.into(),
    };
    let store = SqliteStore::open(database.path()).unwrap();
    store.insert_agent(&agent).unwrap();
    agent
}

fn runtime(database: &TestDatabase, transport: FakeTransport) -> TestDirectMessageRuntime {
    let root = WorkspaceRuntime::new(StorageWorker::open(database.path()).unwrap()).unwrap();
    let context = root.direct_message(transport).unwrap();
    TestDirectMessageRuntime { root, context }
}

struct TestDirectMessageRuntime {
    root: WorkspaceRuntime<FakeTransport>,
    context: AgentDirectMessageRuntime<FakeTransport>,
}

impl DirectMessageRuntime for TestDirectMessageRuntime {
    async fn open(
        &mut self,
        user_id: String,
        agent_name: String,
        opened_at: String,
    ) -> Result<OpenedDirectMessage, DirectMessageError> {
        self.context.open(user_id, agent_name, opened_at).await
    }

    async fn persist_message(&mut self, message: Message) -> Result<(), DirectMessageError> {
        self.context.persist_message(message).await
    }

    async fn send_exact(&mut self, content: String) -> Result<(), DirectMessageError> {
        self.context.send_exact(content).await
    }

    async fn next_runtime_event(
        &mut self,
        observed_at: String,
    ) -> Result<Option<DirectMessageRuntimeEvent>, DirectMessageError> {
        self.context.next_runtime_event(observed_at).await
    }

    async fn respond_permission(
        &mut self,
        request_id: DirectMessagePermissionRequestId,
        outcome: PermissionOutcome,
        decided_at: String,
    ) -> Result<(), DirectMessageError> {
        self.context
            .respond_permission(request_id, outcome, decided_at)
            .await
    }

    async fn cancel_turn(&mut self, cancelled_at: String) -> Result<(), DirectMessageError> {
        self.context.cancel_turn(cancelled_at).await
    }

    async fn shutdown(&mut self, stopped_at: String) -> Result<(), DirectMessageError> {
        let context = self.context.shutdown(stopped_at.clone()).await;
        let root = self
            .root
            .shutdown(stopped_at)
            .await
            .map_err(|error| DirectMessageError::Runtime(error.to_string()));
        context.and(root)
    }
}

#[tokio::test]
async fn sends_exact_content_and_persists_both_message_directions() {
    let database = TestDatabase::new();
    let agent = seed_agent(&database);
    let (transport, events, observed) = FakeTransport::new();
    let mut service = DirectMessageService::new(runtime(&database, transport));
    let opened = service
        .open("tony".into(), "codex".into(), NOW.into())
        .await
        .unwrap();

    let original = "  preserve me byte-for-byte\n";
    service
        .send_message(original.into(), NOW.into())
        .await
        .unwrap();
    assert_eq!(observed.lock().unwrap().messages[0].content, original);
    assert_eq!(
        observed.lock().unwrap().connections[0].project_root,
        PathBuf::from(&agent.project_root)
    );

    let session = observed.lock().unwrap().messages[0].session.clone();
    events
        .send(TransportEvent::AgentTextDelta {
            session: session.clone(),
            text: "Hello ".into(),
        })
        .await
        .unwrap();
    events
        .send(TransportEvent::PermissionRequested(PermissionRequest {
            session: session.clone(),
            request_id: PermissionRequestId::from("permission-1"),
            options: vec![PermissionOption {
                id: "allow".into(),
                label: "Allow".into(),
            }],
        }))
        .await
        .unwrap();
    events
        .send(TransportEvent::AgentTextDelta {
            session: session.clone(),
            text: "Tony".into(),
        })
        .await
        .unwrap();
    events
        .send(TransportEvent::AgentMessageCompleted { session })
        .await
        .unwrap();

    assert_eq!(
        service.next_event(NOW.into()).await.unwrap(),
        Some(DirectMessageEvent::TextDelta("Hello ".into()))
    );
    let request_id = match service.next_event(NOW.into()).await.unwrap() {
        Some(DirectMessageEvent::PermissionRequested {
            request_id,
            options,
        }) => {
            assert_eq!(options[0].id, "allow");
            request_id
        }
        other => panic!("unexpected permission event: {other:?}"),
    };
    service
        .respond_permission(
            request_id,
            PermissionOutcome::Selected("allow".into()),
            NOW.into(),
        )
        .await
        .unwrap();
    assert_eq!(
        service.next_event(NOW.into()).await.unwrap(),
        Some(DirectMessageEvent::TextDelta("Tony".into()))
    );
    let completed = match service.next_event(NOW.into()).await.unwrap() {
        Some(DirectMessageEvent::MessageCompleted(message)) => message,
        other => panic!("unexpected completion event: {other:?}"),
    };
    assert_eq!(completed.body, "Hello Tony");
    assert_eq!(completed.sender_type, MemberType::Agent);
    assert_eq!(completed.sender_id, agent.id.to_string());

    service.shutdown(LATER.into()).await.unwrap();
    let store = SqliteStore::open(database.path()).unwrap();
    let messages = store.list_messages(opened.conversation_id).unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].body, original);
    assert_eq!(
        messages[0].metadata,
        json!({"july": {"schema": 1, "channel": "dm", "direction": "outbound"}})
    );
    assert_eq!(messages[1], completed);
    assert_eq!(
        messages[1].metadata,
        json!({"july": {"schema": 1, "channel": "dm", "direction": "inbound"}})
    );
}

#[tokio::test]
async fn permissions_fail_closed_and_pending_requests_are_audited_on_shutdown() {
    let database = TestDatabase::new();
    let agent = seed_agent(&database);
    let (transport, events, observed) = FakeTransport::new();
    let mut service = DirectMessageService::new(runtime(&database, transport));
    service
        .open("tony".into(), "codex".into(), NOW.into())
        .await
        .unwrap();
    let session = SessionRef {
        binding_id: observed.lock().unwrap().creates[0].binding_id,
        remote_session_id: "remote-dm".into(),
    };
    let mut store = SqliteStore::open(database.path()).unwrap();
    let foreign_dm = store.get_or_create_dm("other-user", agent.id, NOW).unwrap();
    let foreign_binding = SessionBinding {
        id: SessionBindingId::new(),
        conversation_id: foreign_dm.id,
        agent_id: agent.id,
        transport_type: "acp".into(),
        remote_session_id: Some("foreign".into()),
        generation: 1,
        status: SessionBindingStatus::Active,
        created_at: NOW.into(),
        last_used_at: NOW.into(),
    };
    store.insert_session_binding(&foreign_binding).unwrap();
    drop(store);

    for correlation_id in ["invalid-option", "pending-shutdown"] {
        events
            .send(TransportEvent::PermissionRequested(PermissionRequest {
                session: session.clone(),
                request_id: PermissionRequestId::from(correlation_id),
                options: vec![PermissionOption {
                    id: "allow".into(),
                    label: "Allow".into(),
                }],
            }))
            .await
            .unwrap();
    }

    let invalid = match service.next_event(NOW.into()).await.unwrap() {
        Some(DirectMessageEvent::PermissionRequested { request_id, .. }) => request_id,
        other => panic!("unexpected event: {other:?}"),
    };
    assert!(
        service
            .respond_permission(
                invalid,
                PermissionOutcome::Selected("not-advertised".into()),
                NOW.into(),
            )
            .await
            .is_err()
    );
    assert_eq!(
        observed.lock().unwrap().permissions[0].outcome,
        PermissionOutcome::Cancelled
    );
    assert!(matches!(
        service.next_event(NOW.into()).await.unwrap(),
        Some(DirectMessageEvent::PermissionRequested { .. })
    ));
    events
        .send(TransportEvent::PermissionRequested(PermissionRequest {
            session: SessionRef {
                binding_id: foreign_binding.id,
                remote_session_id: "foreign".into(),
            },
            request_id: PermissionRequestId::from("foreign-session"),
            options: vec![],
        }))
        .await
        .unwrap();
    events
        .send(TransportEvent::SessionLost {
            session: SessionRef {
                binding_id: foreign_binding.id,
                remote_session_id: "foreign".into(),
            },
        })
        .await
        .unwrap();
    assert_eq!(
        SqliteStore::open(database.path())
            .unwrap()
            .get_session_binding(foreign_binding.id)
            .unwrap()
            .unwrap()
            .status,
        SessionBindingStatus::Active
    );
    events
        .send(TransportEvent::PermissionRequested(PermissionRequest {
            session: session.clone(),
            request_id: PermissionRequestId::from("queued-owned-session"),
            options: vec![],
        }))
        .await
        .unwrap();
    events
        .send(TransportEvent::SessionLost {
            session: session.clone(),
        })
        .await
        .unwrap();
    assert!(matches!(
        service.next_event(NOW.into()).await.unwrap(),
        Some(DirectMessageEvent::PermissionRequested { .. })
    ));
    assert_eq!(
        service.next_event(NOW.into()).await,
        Err(DirectMessageError::SessionLost)
    );
    events
        .send(TransportEvent::PermissionRequested(PermissionRequest {
            session: SessionRef {
                binding_id: foreign_binding.id,
                remote_session_id: "foreign".into(),
            },
            request_id: PermissionRequestId::from("queued-foreign-session"),
            options: vec![],
        }))
        .await
        .unwrap();
    events
        .send(TransportEvent::SessionLost {
            session: SessionRef {
                binding_id: foreign_binding.id,
                remote_session_id: "foreign".into(),
            },
        })
        .await
        .unwrap();
    service.shutdown(LATER.into()).await.unwrap();

    let connection = rusqlite::Connection::open(database.path()).unwrap();
    let cancelled: i64 = connection
        .query_row(
            "SELECT count(*) FROM permission_decisions
             WHERE correlation_id IN (
                 'invalid-option', 'pending-shutdown', 'queued-owned-session'
             )
               AND outcome = 'cancelled'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(cancelled, 3);
    let foreign: i64 = connection
        .query_row(
            "SELECT count(*) FROM permission_decisions
             WHERE correlation_id IN ('foreign-session', 'queued-foreign-session')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(foreign, 0);
    assert_eq!(
        SqliteStore::open(database.path())
            .unwrap()
            .get_session_binding(session.binding_id)
            .unwrap()
            .unwrap()
            .status,
        SessionBindingStatus::Lost
    );
    assert_eq!(
        SqliteStore::open(database.path())
            .unwrap()
            .get_session_binding(foreign_binding.id)
            .unwrap()
            .unwrap()
            .status,
        SessionBindingStatus::Active
    );
}

#[tokio::test]
async fn transport_disconnect_and_shutdown_only_disconnect_the_owned_binding() {
    let database = TestDatabase::new();
    let agent = seed_agent(&database);
    let (transport, events, observed) = FakeTransport::new();
    let mut service = DirectMessageService::new(runtime(&database, transport));
    service
        .open("tony".into(), "codex".into(), NOW.into())
        .await
        .unwrap();
    let owned_binding_id = observed.lock().unwrap().creates[0].binding_id;
    let mut store = SqliteStore::open(database.path()).unwrap();
    let foreign_dm = store.get_or_create_dm("other-user", agent.id, NOW).unwrap();
    let foreign_binding = SessionBinding {
        id: SessionBindingId::new(),
        conversation_id: foreign_dm.id,
        agent_id: agent.id,
        transport_type: "acp".into(),
        remote_session_id: Some("foreign".into()),
        generation: 1,
        status: SessionBindingStatus::Active,
        created_at: NOW.into(),
        last_used_at: NOW.into(),
    };
    store.insert_session_binding(&foreign_binding).unwrap();
    drop(store);

    events
        .send(TransportEvent::TransportDisconnected {
            agent_id: agent.id,
            reason: "wire closed".into(),
        })
        .await
        .unwrap();
    assert_eq!(
        service.next_event(LATER.into()).await.unwrap(),
        Some(DirectMessageEvent::Disconnected("wire closed".into()))
    );

    let store = SqliteStore::open(database.path()).unwrap();
    assert_eq!(
        store
            .get_session_binding(owned_binding_id)
            .unwrap()
            .unwrap()
            .status,
        SessionBindingStatus::Disconnected
    );
    assert_eq!(
        store
            .get_session_binding(foreign_binding.id)
            .unwrap()
            .unwrap()
            .status,
        SessionBindingStatus::Active
    );
    drop(store);

    service.shutdown(LATER.into()).await.unwrap();
    assert_eq!(
        SqliteStore::open(database.path())
            .unwrap()
            .get_session_binding(foreign_binding.id)
            .unwrap()
            .unwrap()
            .status,
        SessionBindingStatus::Active
    );
}

#[tokio::test]
async fn restart_reuses_dm_and_resumes_without_replaying_history() {
    let database = TestDatabase::new();
    let agent = seed_agent(&database);
    let (first_transport, _events, first_observed) = FakeTransport::new();
    let mut first = DirectMessageService::new(runtime(&database, first_transport));
    let first_open = first
        .open("tony".into(), "codex".into(), NOW.into())
        .await
        .unwrap();
    first
        .send_message("remembered".into(), NOW.into())
        .await
        .unwrap();
    let binding_id = first_observed.lock().unwrap().creates[0].binding_id;
    first.shutdown(NOW.into()).await.unwrap();

    let (second_transport, _events, second_observed) = FakeTransport::new();
    let mut second = DirectMessageService::new(runtime(&database, second_transport));
    let second_open = second
        .open("tony".into(), "codex".into(), LATER.into())
        .await
        .unwrap();

    assert_eq!(second_open.conversation_id, first_open.conversation_id);
    assert_eq!(second_open.messages.len(), 1);
    {
        let observed = second_observed.lock().unwrap();
        assert!(observed.creates.is_empty());
        assert!(
            observed.messages.is_empty(),
            "history was replayed to the agent"
        );
        assert_eq!(observed.resumes[0].session.binding_id, binding_id);
        assert_eq!(observed.resumes[0].session.remote_session_id, "remote-dm");
        assert_eq!(
            observed.resumes[0].project_root,
            PathBuf::from(&agent.project_root)
        );
    }
    second.shutdown(LATER.into()).await.unwrap();
}

#[tokio::test]
async fn lost_or_closed_binding_is_not_replaced() {
    for status in [SessionBindingStatus::Lost, SessionBindingStatus::Closed] {
        let database = TestDatabase::new();
        let agent = seed_agent(&database);
        let mut store = SqliteStore::open(database.path()).unwrap();
        let dm = store.get_or_create_dm("tony", agent.id, NOW).unwrap();
        let binding = SessionBinding {
            id: SessionBindingId::new(),
            conversation_id: dm.id,
            agent_id: agent.id,
            transport_type: "acp".into(),
            remote_session_id: Some("remote-old".into()),
            generation: 1,
            status,
            created_at: NOW.into(),
            last_used_at: NOW.into(),
        };
        store.insert_session_binding(&binding).unwrap();
        drop(store);
        let (transport, _events, observed) = FakeTransport::new();
        let mut service = DirectMessageService::new(runtime(&database, transport));

        let error = service
            .open("tony".into(), "codex".into(), LATER.into())
            .await
            .unwrap_err();
        assert_eq!(
            error,
            if status == SessionBindingStatus::Lost {
                DirectMessageError::SessionLost
            } else {
                DirectMessageError::SessionUnavailable(SessionBindingStatus::Closed)
            }
        );
        assert!(observed.lock().unwrap().connections.is_empty());
        service.shutdown(LATER.into()).await.unwrap();
        let store = SqliteStore::open(database.path()).unwrap();
        assert_eq!(
            store.get_latest_session_binding(dm.id, agent.id).unwrap(),
            Some(binding)
        );
    }
}

#[tokio::test]
async fn active_binding_without_remote_session_becomes_lost() {
    let database = TestDatabase::new();
    let agent = seed_agent(&database);
    let mut store = SqliteStore::open(database.path()).unwrap();
    let dm = store.get_or_create_dm("tony", agent.id, NOW).unwrap();
    let binding = SessionBinding {
        id: SessionBindingId::new(),
        conversation_id: dm.id,
        agent_id: agent.id,
        transport_type: "acp".into(),
        remote_session_id: None,
        generation: 1,
        status: SessionBindingStatus::Active,
        created_at: NOW.into(),
        last_used_at: NOW.into(),
    };
    store.insert_session_binding(&binding).unwrap();
    drop(store);
    let (transport, _events, observed) = FakeTransport::new();
    let mut service = DirectMessageService::new(runtime(&database, transport));

    assert_eq!(
        service
            .open("tony".into(), "codex".into(), LATER.into())
            .await
            .unwrap_err(),
        DirectMessageError::SessionLost
    );
    assert!(observed.lock().unwrap().connections.is_empty());
    service.shutdown(LATER.into()).await.unwrap();
    let store = SqliteStore::open(database.path()).unwrap();
    assert_eq!(
        store
            .get_latest_session_binding(dm.id, agent.id)
            .unwrap()
            .unwrap()
            .status,
        SessionBindingStatus::Lost
    );
}

#[tokio::test]
async fn provider_missing_remote_session_marks_the_same_binding_lost() {
    let database = TestDatabase::new();
    let agent = seed_agent(&database);
    let mut store = SqliteStore::open(database.path()).unwrap();
    let dm = store.get_or_create_dm("tony", agent.id, NOW).unwrap();
    let binding = SessionBinding {
        id: SessionBindingId::new(),
        conversation_id: dm.id,
        agent_id: agent.id,
        transport_type: "acp".into(),
        remote_session_id: Some("remote-missing".into()),
        generation: 1,
        status: SessionBindingStatus::Disconnected,
        created_at: NOW.into(),
        last_used_at: NOW.into(),
    };
    store.insert_session_binding(&binding).unwrap();
    drop(store);
    let (mut transport, _events, observed) = FakeTransport::new();
    transport.resume_lost = true;
    let mut service = DirectMessageService::new(runtime(&database, transport));

    assert_eq!(
        service
            .open("tony".into(), "codex".into(), LATER.into())
            .await
            .unwrap_err(),
        DirectMessageError::SessionLost
    );
    assert_eq!(observed.lock().unwrap().resumes.len(), 1);
    service.shutdown(LATER.into()).await.unwrap();
    let store = SqliteStore::open(database.path()).unwrap();
    let latest = store
        .get_latest_session_binding(dm.id, agent.id)
        .unwrap()
        .unwrap();
    assert_eq!(latest.id, binding.id);
    assert_eq!(latest.generation, 1);
    assert_eq!(latest.status, SessionBindingStatus::Lost);
}

#[tokio::test]
async fn rejects_blank_messages_and_does_not_observe_foreign_session_events() {
    let database = TestDatabase::new();
    seed_agent(&database);
    let (transport, events, observed) = FakeTransport::new();
    let mut service = DirectMessageService::new(runtime(&database, transport));
    service
        .open("tony".into(), "codex".into(), NOW.into())
        .await
        .unwrap();

    assert_eq!(
        service.send_message(" \n".into(), NOW.into()).await,
        Err(DirectMessageError::EmptyMessage)
    );
    assert!(observed.lock().unwrap().messages.is_empty());
    events
        .send(TransportEvent::AgentTextDelta {
            session: SessionRef {
                binding_id: SessionBindingId::new(),
                remote_session_id: "remote-dm".into(),
            },
            text: "foreign".into(),
        })
        .await
        .unwrap();
    let owned_binding_id = observed.lock().unwrap().creates[0].binding_id;
    events
        .send(TransportEvent::AgentTextDelta {
            session: SessionRef {
                binding_id: owned_binding_id,
                remote_session_id: "remote-dm".into(),
            },
            text: "owned".into(),
        })
        .await
        .unwrap();
    assert_eq!(
        service.next_event(NOW.into()).await.unwrap(),
        Some(DirectMessageEvent::TextDelta("owned".into()))
    );
    service.shutdown(LATER.into()).await.unwrap();
    service.shutdown(LATER.into()).await.unwrap();
    assert_eq!(observed.lock().unwrap().shutdowns, 1);
}

#[derive(Default)]
struct PersistenceObserved {
    attempts: Vec<Message>,
    persisted: Vec<Message>,
    event_reads: usize,
    shutdowns: usize,
}

#[derive(Default)]
struct FailingPersistencePort {
    opened: Option<OpenedDirectMessage>,
    events: VecDeque<DirectMessageRuntimeEvent>,
    observed: Arc<Mutex<PersistenceObserved>>,
    fail_next_persist: bool,
}

impl DirectMessageRuntime for FailingPersistencePort {
    async fn open(
        &mut self,
        _user_id: String,
        _agent_name: String,
        _opened_at: String,
    ) -> Result<OpenedDirectMessage, DirectMessageError> {
        self.opened.take().ok_or(DirectMessageError::AlreadyOpen)
    }

    async fn persist_message(&mut self, message: Message) -> Result<(), DirectMessageError> {
        self.observed.lock().unwrap().attempts.push(message.clone());
        if self.fail_next_persist {
            self.fail_next_persist = false;
            Err(DirectMessageError::Runtime("fixture write failure".into()))
        } else {
            self.observed.lock().unwrap().persisted.push(message);
            Ok(())
        }
    }

    async fn send_exact(&mut self, _content: String) -> Result<(), DirectMessageError> {
        Ok(())
    }

    async fn next_runtime_event(
        &mut self,
        _observed_at: String,
    ) -> Result<Option<DirectMessageRuntimeEvent>, DirectMessageError> {
        self.observed.lock().unwrap().event_reads += 1;
        Ok(self.events.pop_front())
    }

    async fn respond_permission(
        &mut self,
        _request_id: DirectMessagePermissionRequestId,
        _outcome: PermissionOutcome,
        _decided_at: String,
    ) -> Result<(), DirectMessageError> {
        Ok(())
    }

    async fn cancel_turn(&mut self, _cancelled_at: String) -> Result<(), DirectMessageError> {
        Ok(())
    }

    async fn shutdown(&mut self, _stopped_at: String) -> Result<(), DirectMessageError> {
        self.observed.lock().unwrap().shutdowns += 1;
        Ok(())
    }
}

#[tokio::test]
async fn cancel_turn_uses_the_hidden_active_session() {
    let database = TestDatabase::new();
    seed_agent(&database);
    let (transport, _events, observed) = FakeTransport::new();
    let mut service = DirectMessageService::new(runtime(&database, transport));
    service
        .open("tony".into(), "codex".into(), NOW.into())
        .await
        .unwrap();

    service.cancel_turn(LATER.into()).await.unwrap();

    let observed = observed.lock().unwrap();
    assert_eq!(observed.cancellations.len(), 1);
    assert_eq!(observed.cancellations[0].remote_session_id, "remote-dm");
}

#[tokio::test]
async fn completed_message_persistence_retries_before_reading_another_event() {
    let agent_id = AgentId::new();
    let observed = Arc::new(Mutex::new(PersistenceObserved::default()));
    let port = FailingPersistencePort {
        opened: Some(OpenedDirectMessage {
            conversation_id: ConversationId::new(),
            agent_id,
            agent_name: "codex".into(),
            messages: vec![],
        }),
        events: VecDeque::from([
            DirectMessageRuntimeEvent::TextDelta("answer".into()),
            DirectMessageRuntimeEvent::AgentMessageCompleted,
            DirectMessageRuntimeEvent::TextDelta("must remain queued".into()),
        ]),
        observed: observed.clone(),
        fail_next_persist: true,
    };
    let mut service = DirectMessageService::new(port);
    service
        .open("tony".into(), "codex".into(), NOW.into())
        .await
        .unwrap();
    assert_eq!(
        service.next_event(NOW.into()).await.unwrap(),
        Some(DirectMessageEvent::TextDelta("answer".into()))
    );
    assert_eq!(
        service.next_event(NOW.into()).await,
        Err(DirectMessageError::Runtime("fixture write failure".into()))
    );

    let completed = service.next_event(LATER.into()).await.unwrap().unwrap();
    let DirectMessageEvent::MessageCompleted(message) = completed else {
        panic!("expected completed message");
    };
    assert_eq!(message.body, "answer");
    assert_eq!(message.sender_id, agent_id.to_string());
    assert_eq!(message.created_at, NOW);
    let observed = observed.lock().unwrap();
    assert_eq!(observed.event_reads, 2);
    assert_eq!(observed.attempts, vec![message.clone(), message.clone()]);
    assert_eq!(observed.persisted, vec![message]);
}

#[tokio::test]
async fn shutdown_retries_pending_completion_before_stopping_runtime() {
    let agent_id = AgentId::new();
    let observed = Arc::new(Mutex::new(PersistenceObserved::default()));
    let port = FailingPersistencePort {
        opened: Some(OpenedDirectMessage {
            conversation_id: ConversationId::new(),
            agent_id,
            agent_name: "codex".into(),
            messages: vec![],
        }),
        events: VecDeque::from([
            DirectMessageRuntimeEvent::TextDelta("answer".into()),
            DirectMessageRuntimeEvent::AgentMessageCompleted,
        ]),
        observed: observed.clone(),
        fail_next_persist: true,
    };
    let mut service = DirectMessageService::new(port);
    service
        .open("tony".into(), "codex".into(), NOW.into())
        .await
        .unwrap();
    service.next_event(NOW.into()).await.unwrap();
    assert_eq!(
        service.next_event(NOW.into()).await,
        Err(DirectMessageError::Runtime("fixture write failure".into()))
    );

    service.shutdown(LATER.into()).await.unwrap();
    let observed = observed.lock().unwrap();
    assert_eq!(observed.shutdowns, 1);
    assert_eq!(observed.attempts.len(), 2);
    assert_eq!(observed.attempts[0], observed.attempts[1]);
    assert_eq!(observed.persisted, vec![observed.attempts[0].clone()]);
    assert_eq!(observed.persisted[0].body, "answer");
    assert_eq!(observed.persisted[0].sender_id, agent_id.to_string());
    assert_eq!(observed.persisted[0].created_at, NOW);
}

#[tokio::test]
async fn foreign_session_event_is_not_observed_or_added_to_accumulated_text() {
    let database = TestDatabase::new();
    seed_agent(&database);
    let (transport, events, observed) = FakeTransport::new();
    let mut service = DirectMessageService::new(runtime(&database, transport));
    service
        .open("tony".into(), "codex".into(), NOW.into())
        .await
        .unwrap();
    let session = SessionRef {
        binding_id: observed.lock().unwrap().creates[0].binding_id,
        remote_session_id: "remote-dm".into(),
    };
    events
        .send(TransportEvent::AgentTextDelta {
            session: session.clone(),
            text: "Hello ".into(),
        })
        .await
        .unwrap();
    events
        .send(TransportEvent::AgentTextDelta {
            session: SessionRef {
                binding_id: SessionBindingId::new(),
                remote_session_id: "remote-dm".into(),
            },
            text: "foreign".into(),
        })
        .await
        .unwrap();
    events
        .send(TransportEvent::AgentTextDelta {
            session: session.clone(),
            text: "Tony".into(),
        })
        .await
        .unwrap();
    events
        .send(TransportEvent::AgentMessageCompleted { session })
        .await
        .unwrap();

    assert_eq!(
        service.next_event(NOW.into()).await.unwrap(),
        Some(DirectMessageEvent::TextDelta("Hello ".into()))
    );
    assert_eq!(
        service.next_event(NOW.into()).await.unwrap(),
        Some(DirectMessageEvent::TextDelta("Tony".into()))
    );
    assert!(matches!(
        service.next_event(NOW.into()).await.unwrap(),
        Some(DirectMessageEvent::MessageCompleted(ref message)) if message.body == "Hello Tony"
    ));
    service.shutdown(LATER.into()).await.unwrap();
}

#[test]
fn application_dm_types_are_transport_neutral() {
    let source = include_str!("../src/application/dm.rs");
    assert!(!source.contains("agent_client_protocol"));
    assert!(!source.contains("crate::transport"));
    assert!(!std::any::type_name::<DirectMessagePermissionRequestId>().contains("acp"));
    let _conversation_id: Option<ConversationId> = None;
}

#[tokio::test]
async fn explicit_agent_dm_routes_only_to_target_owner_and_persists_both_agents() {
    let database = TestDatabase::new();
    let source = seed_agent(&database);
    let target = Agent {
        id: AgentId::new(),
        name: "claude".into(),
        project_root: "/workspace/target".into(),
        transport_type: "acp".into(),
        transport_config: json!({}),
        status: "active".into(),
        metadata: json!({}),
        created_at: NOW.into(),
        updated_at: NOW.into(),
    };
    SqliteStore::open(database.path())
        .unwrap()
        .insert_agent(&target)
        .unwrap();
    let (source_transport, _source_events, source_observed) = FakeTransport::new();
    let (target_transport, target_events, target_observed) = FakeTransport::new();
    let mut workspace =
        WorkspaceRuntime::new(StorageWorker::open(database.path()).unwrap()).unwrap();
    let mut source_owner =
        DirectMessageService::new(workspace.direct_message(source_transport).unwrap());
    source_owner
        .open("source-owner".into(), source.name.clone(), NOW.into())
        .await
        .unwrap();
    let mut routed =
        DirectMessageService::new(workspace.direct_message_for_agent(target.id).unwrap());
    let command = OpenAgentDirectMessage {
        source_agent_id: source.id,
        target_agent_id: target.id,
        opened_at: NOW.into(),
    };
    assert!(matches!(
        routed.open_agent(command.clone()).await,
        Err(DirectMessageError::Runtime(message))
            if message.contains(&format!("agent {} has no runtime owner", target.id))
    ));
    let mut target_owner =
        DirectMessageService::new(workspace.direct_message(target_transport).unwrap());
    target_owner
        .open("target-owner".into(), target.name.clone(), NOW.into())
        .await
        .unwrap();

    let opened = routed.open_agent(command).await.unwrap();
    assert!(opened.messages.is_empty());

    let body = "  inspect this exact payload\n";
    routed.send_message(body.into(), NOW.into()).await.unwrap();
    assert!(source_observed.lock().unwrap().messages.is_empty());
    let routed_session = {
        let observed = target_observed.lock().unwrap();
        assert_eq!(observed.connections.len(), 1);
        assert_eq!(observed.creates.len(), 2);
        assert_eq!(observed.messages.len(), 1);
        assert_eq!(observed.messages[0].content, body);
        assert_eq!(
            observed.messages[0].session.binding_id,
            observed.creates[1].binding_id
        );
        observed.messages[0].session.clone()
    };

    target_events
        .send(TransportEvent::AgentTextDelta {
            session: routed_session.clone(),
            text: "target response".into(),
        })
        .await
        .unwrap();
    target_events
        .send(TransportEvent::AgentMessageCompleted {
            session: routed_session,
        })
        .await
        .unwrap();
    assert_eq!(
        routed.next_event(LATER.into()).await.unwrap(),
        Some(DirectMessageEvent::TextDelta("target response".into()))
    );
    let completed = routed.next_event(LATER.into()).await.unwrap().unwrap();
    let DirectMessageEvent::MessageCompleted(completed) = completed else {
        panic!("expected completed target response")
    };
    assert_eq!(completed.sender_id, target.id.to_string());

    let messages = SqliteStore::open(database.path())
        .unwrap()
        .list_messages(opened.conversation_id)
        .unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].sender_type, MemberType::Agent);
    assert_eq!(messages[0].sender_id, source.id.to_string());
    assert_eq!(messages[0].body, body);
    assert_eq!(messages[1], completed);

    routed.shutdown(LATER.into()).await.unwrap();
    source_owner.shutdown(LATER.into()).await.unwrap();
    target_owner.shutdown(LATER.into()).await.unwrap();
    workspace.shutdown(LATER.into()).await.unwrap();
}

#[tokio::test]
async fn neutral_runtime_cannot_open_agent_dm_or_register_the_target_owner() {
    let database = TestDatabase::new();
    let source = seed_agent(&database);
    let target = Agent {
        id: AgentId::new(),
        name: "claude".into(),
        project_root: "/workspace/target".into(),
        transport_type: "acp".into(),
        transport_config: json!({}),
        status: "active".into(),
        metadata: json!({}),
        created_at: NOW.into(),
        updated_at: NOW.into(),
    };
    SqliteStore::open(database.path())
        .unwrap()
        .insert_agent(&target)
        .unwrap();
    let (transport, _events, observed) = FakeTransport::new();
    let mut workspace =
        WorkspaceRuntime::new(StorageWorker::open(database.path()).unwrap()).unwrap();
    let mut neutral = DirectMessageService::new(workspace.direct_message(transport).unwrap());

    assert_eq!(
        neutral
            .open_agent(OpenAgentDirectMessage {
                source_agent_id: source.id,
                target_agent_id: target.id,
                opened_at: NOW.into(),
            })
            .await,
        Err(DirectMessageError::AgentTargetNotBound)
    );
    assert!(observed.lock().unwrap().connections.is_empty());
    assert!(observed.lock().unwrap().creates.is_empty());
    let conversation_count: i64 = rusqlite::Connection::open(database.path())
        .unwrap()
        .query_row("SELECT COUNT(*) FROM conversations", [], |row| row.get(0))
        .unwrap();
    assert_eq!(conversation_count, 0);

    neutral.shutdown(LATER.into()).await.unwrap();
    workspace.shutdown(LATER.into()).await.unwrap();
}

#[tokio::test]
async fn user_direct_message_context_is_terminal_after_shutdown() {
    let database = TestDatabase::new();
    let agent = seed_agent(&database);
    let (transport, _events, observed) = FakeTransport::new();
    let mut workspace =
        WorkspaceRuntime::new(StorageWorker::open(database.path()).unwrap()).unwrap();
    let mut service = DirectMessageService::new(workspace.direct_message(transport).unwrap());
    let opened = service
        .open("tony".into(), agent.name.clone(), NOW.into())
        .await
        .unwrap();

    service.shutdown(LATER.into()).await.unwrap();
    service.shutdown(LATER.into()).await.unwrap();
    assert_eq!(
        service.open("tony".into(), agent.name, LATER.into()).await,
        Err(DirectMessageError::ContextStopped)
    );
    assert_eq!(observed.lock().unwrap().creates.len(), 1);
    assert!(observed.lock().unwrap().messages.is_empty());
    assert_eq!(
        SqliteStore::open(database.path())
            .unwrap()
            .get_latest_session_binding(opened.conversation_id, agent.id)
            .unwrap()
            .unwrap()
            .status,
        SessionBindingStatus::Disconnected
    );

    workspace.shutdown(LATER.into()).await.unwrap();
}

#[tokio::test]
async fn agent_direct_message_context_is_terminal_after_shutdown() {
    let database = TestDatabase::new();
    let source = seed_agent(&database);
    let target = Agent {
        id: AgentId::new(),
        name: "claude".into(),
        project_root: "/workspace/target".into(),
        transport_type: "acp".into(),
        transport_config: json!({}),
        status: "active".into(),
        metadata: json!({}),
        created_at: NOW.into(),
        updated_at: NOW.into(),
    };
    SqliteStore::open(database.path())
        .unwrap()
        .insert_agent(&target)
        .unwrap();
    let (transport, _events, observed) = FakeTransport::new();
    let mut workspace =
        WorkspaceRuntime::new(StorageWorker::open(database.path()).unwrap()).unwrap();
    let mut target_owner = DirectMessageService::new(workspace.direct_message(transport).unwrap());
    target_owner
        .open("target-owner".into(), target.name.clone(), NOW.into())
        .await
        .unwrap();
    let mut routed =
        DirectMessageService::new(workspace.direct_message_for_agent(target.id).unwrap());
    let command = OpenAgentDirectMessage {
        source_agent_id: source.id,
        target_agent_id: target.id,
        opened_at: NOW.into(),
    };
    let opened = routed.open_agent(command.clone()).await.unwrap();

    routed.shutdown(LATER.into()).await.unwrap();
    routed.shutdown(LATER.into()).await.unwrap();
    assert_eq!(
        routed.open_agent(command).await,
        Err(DirectMessageError::ContextStopped)
    );
    assert_eq!(observed.lock().unwrap().creates.len(), 2);
    assert!(observed.lock().unwrap().messages.is_empty());
    assert_eq!(
        SqliteStore::open(database.path())
            .unwrap()
            .get_latest_session_binding(opened.conversation_id, target.id)
            .unwrap()
            .unwrap()
            .status,
        SessionBindingStatus::Disconnected
    );

    target_owner.shutdown(LATER.into()).await.unwrap();
    workspace.shutdown(LATER.into()).await.unwrap();
}
