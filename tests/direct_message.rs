use july_workspace::application::{
    AgentDirectMessageOutcome, DirectMessageError, DirectMessageEvent,
    DirectMessagePermissionRequestId, DirectMessageRuntime, DirectMessageRuntimeEvent,
    DirectMessageService, OpenAgentDirectMessage, OpenedDirectMessage, RetryAgentDirectMessage,
    SendAgentDirectMessage,
};
use july_workspace::domain::{
    Agent, AgentId, ConversationId, DeliveryStatus, MemberType, Message, MessageId,
    PermissionOption, PermissionOutcome, SessionBinding, SessionBindingId, SessionBindingStatus,
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
    create_fails_at: Option<usize>,
    send_fails_at: Option<usize>,
    block_send_at: Option<(usize, std::sync::mpsc::SyncSender<()>)>,
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
                create_fails_at: None,
                send_fails_at: None,
                block_send_at: None,
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
        if self.create_fails_at == Some(self.observed.lock().unwrap().creates.len()) {
            return Err(TransportError::Protocol("fixture open failure".into()));
        }
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
        let sent = self.observed.lock().unwrap().messages.len();
        if let Some((blocked_at, reached)) = &self.block_send_at
            && *blocked_at == sent
        {
            reached.send(()).unwrap();
            return std::future::pending().await;
        }
        if self.send_fails_at == Some(sent) {
            return Err(TransportError::Protocol("fixture send failure".into()));
        }
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

fn seed_named_agent(database: &TestDatabase, name: &str) -> Agent {
    let agent = Agent {
        id: Default::default(),
        name: name.into(),
        project_root: format!("/workspace/{name}"),
        transport_type: "acp".into(),
        transport_config: json!({}),
        status: "active".into(),
        metadata: json!({}),
        created_at: NOW.into(),
        updated_at: NOW.into(),
    };
    SqliteStore::open(database.path())
        .unwrap()
        .insert_agent(&agent)
        .unwrap();
    agent
}

fn agent_send(
    message_id: MessageId,
    source_agent_id: AgentId,
    target_agent_id: AgentId,
    body: &str,
) -> SendAgentDirectMessage {
    SendAgentDirectMessage {
        message_id,
        source_agent_id,
        target_agent_id,
        body: body.into(),
        sent_at: NOW.into(),
    }
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

#[tokio::test]
async fn offline_agent_message_is_durable_and_explicit_retry_delivers_exact_body() {
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
    let message_id = MessageId::new();
    let body = "  durable exact body\n";
    let command = SendAgentDirectMessage {
        message_id,
        source_agent_id: source.id,
        target_agent_id: target.id,
        body: body.into(),
        sent_at: NOW.into(),
    };

    assert!(matches!(
        routed.send_agent_message(command.clone()).await.unwrap(),
        Some(AgentDirectMessageOutcome::PersistedFailed(DirectMessageError::Runtime(message)))
            if message.contains("no runtime owner")
    ));
    assert_eq!(routed.send_agent_message(command).await.unwrap(), None);
    let store = SqliteStore::open(database.path()).unwrap();
    let persisted = store.get_message(message_id).unwrap().unwrap();
    assert_eq!(persisted.sender_type, MemberType::Agent);
    assert_eq!(persisted.sender_id, source.id.to_string());
    assert_eq!(persisted.body, body);
    assert_eq!(
        store
            .get_message_delivery(message_id, target.id)
            .unwrap()
            .unwrap()
            .status,
        DeliveryStatus::Failed
    );
    drop(store);
    assert!(source_observed.lock().unwrap().messages.is_empty());

    let mut target_owner =
        DirectMessageService::new(workspace.direct_message(target_transport).unwrap());
    target_owner
        .open("target-owner".into(), target.name.clone(), NOW.into())
        .await
        .unwrap();
    let retry = RetryAgentDirectMessage {
        message_id,
        target_agent_id: target.id,
        retried_at: LATER.into(),
    };
    let Some(AgentDirectMessageOutcome::Delivered(delivered)) =
        routed.retry_agent_message(retry.clone()).await.unwrap()
    else {
        panic!("expected delivered Agent message retry")
    };
    assert_eq!(delivered.source_agent_id, source.id);
    assert_eq!(delivered.target_agent_id, target.id);
    assert_eq!(routed.retry_agent_message(retry).await.unwrap(), None);
    assert_eq!(
        target_observed
            .lock()
            .unwrap()
            .messages
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>(),
        vec![body]
    );
    assert_eq!(
        SqliteStore::open(database.path())
            .unwrap()
            .get_message_delivery(message_id, target.id)
            .unwrap()
            .unwrap()
            .status,
        DeliveryStatus::Delivered
    );
    let routed_session = target_observed.lock().unwrap().messages[0].session.clone();
    target_events
        .send(TransportEvent::AgentTextDelta {
            session: routed_session.clone(),
            text: "retry response".into(),
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
        Some(DirectMessageEvent::TextDelta("retry response".into()))
    );
    let Some(DirectMessageEvent::MessageCompleted(completed)) =
        routed.next_event(LATER.into()).await.unwrap()
    else {
        panic!("expected completed retry response")
    };
    assert_eq!(completed.sender_id, target.id.to_string());
    let messages = SqliteStore::open(database.path())
        .unwrap()
        .list_messages(delivered.conversation_id)
        .unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].id, message_id);
    assert_eq!(messages[0].sender_type, MemberType::Agent);
    assert_eq!(messages[0].sender_id, source.id.to_string());
    assert_eq!(messages[1].sender_type, MemberType::Agent);
    assert_eq!(messages[1], completed);

    routed.shutdown(LATER.into()).await.unwrap();
    source_owner.shutdown(LATER.into()).await.unwrap();
    target_owner.shutdown(LATER.into()).await.unwrap();
    workspace.shutdown(LATER.into()).await.unwrap();
}

#[tokio::test]
async fn cancelled_agent_message_is_reconciled_and_retried_only_to_its_stored_target() {
    let database = TestDatabase::new();
    let source = seed_agent(&database);
    let target = seed_named_agent(&database, "claude");
    let other = seed_named_agent(&database, "gemini");
    let (mut blocked_transport, _blocked_events, blocked_observed) = FakeTransport::new();
    let (blocked_send, blocked) = std::sync::mpsc::sync_channel(1);
    blocked_transport.block_send_at = Some((1, blocked_send));
    let (other_transport, _other_events, other_observed) = FakeTransport::new();
    let database_path = database.path().to_owned();
    let body = "  restart keeps this exact DM body\n";
    let message_id = MessageId::new();
    let source_id = source.id;
    let (stop_first_boot, stopped) = tokio::sync::oneshot::channel::<()>();
    let first_boot = std::thread::spawn({
        let target = target.clone();
        let other = other.clone();
        move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async move {
                    tokio::select! {
                        _ = stopped => {}
                        _ = async move {
                            let workspace = WorkspaceRuntime::new(
                                StorageWorker::open(database_path).unwrap(),
                            )
                            .unwrap();
                            let mut target_owner = DirectMessageService::new(
                                workspace.direct_message(blocked_transport).unwrap(),
                            );
                            target_owner
                                .open("target-owner".into(), target.name.clone(), NOW.into())
                                .await
                                .unwrap();
                            let mut other_owner = DirectMessageService::new(
                                workspace.direct_message(other_transport).unwrap(),
                            );
                            other_owner
                                .open("other-owner".into(), other.name.clone(), NOW.into())
                                .await
                                .unwrap();
                            let mut routed = DirectMessageService::new(
                                workspace.direct_message_for_agent(target.id).unwrap(),
                            );
                            let _ = routed
                                .send_agent_message(agent_send(
                                    message_id,
                                    source_id,
                                    target.id,
                                    body,
                                ))
                                .await;
                        } => panic!("first boot ended before process loss"),
                    }
                });
        }
    });

    blocked.recv().unwrap();
    let store = SqliteStore::open(database.path()).unwrap();
    assert_eq!(store.get_message(message_id).unwrap().unwrap().body, body);
    assert_eq!(
        store
            .get_message_delivery(message_id, target.id)
            .unwrap()
            .unwrap()
            .status,
        DeliveryStatus::Pending
    );
    assert_eq!(
        store.get_message_delivery(message_id, other.id).unwrap(),
        None
    );
    drop(store);
    stop_first_boot.send(()).unwrap();
    first_boot.join().unwrap();
    assert_eq!(
        SqliteStore::open(database.path())
            .unwrap()
            .get_message_delivery(message_id, target.id)
            .unwrap()
            .unwrap()
            .status,
        DeliveryStatus::Pending
    );

    let (target_transport, _target_events, target_observed) = FakeTransport::new();
    let (other_transport, _other_events, restart_other_observed) = FakeTransport::new();
    let mut workspace =
        WorkspaceRuntime::new(StorageWorker::open(database.path()).unwrap()).unwrap();
    assert_eq!(
        SqliteStore::open(database.path())
            .unwrap()
            .get_message_delivery(message_id, target.id)
            .unwrap()
            .unwrap()
            .status,
        DeliveryStatus::Failed
    );
    let mut target_owner =
        DirectMessageService::new(workspace.direct_message(target_transport).unwrap());
    target_owner
        .open("target-owner".into(), target.name.clone(), LATER.into())
        .await
        .unwrap();
    let mut other_owner =
        DirectMessageService::new(workspace.direct_message(other_transport).unwrap());
    other_owner
        .open("other-owner".into(), other.name.clone(), LATER.into())
        .await
        .unwrap();
    let mut routed =
        DirectMessageService::new(workspace.direct_message_for_agent(target.id).unwrap());

    assert!(matches!(
        routed
            .retry_agent_message(RetryAgentDirectMessage {
                message_id,
                target_agent_id: target.id,
                retried_at: LATER.into(),
            })
            .await
            .unwrap(),
        Some(AgentDirectMessageOutcome::Delivered(delivered))
            if delivered.source_agent_id == source.id && delivered.target_agent_id == target.id
    ));
    assert_eq!(
        blocked_observed
            .lock()
            .unwrap()
            .messages
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>(),
        vec![body]
    );
    assert_eq!(
        target_observed
            .lock()
            .unwrap()
            .messages
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>(),
        vec![body]
    );
    assert!(other_observed.lock().unwrap().messages.is_empty());
    assert!(restart_other_observed.lock().unwrap().messages.is_empty());
    assert_eq!(
        SqliteStore::open(database.path())
            .unwrap()
            .get_message_delivery(message_id, target.id)
            .unwrap()
            .unwrap()
            .status,
        DeliveryStatus::Delivered
    );

    routed.shutdown(LATER.into()).await.unwrap();
    target_owner.shutdown(LATER.into()).await.unwrap();
    other_owner.shutdown(LATER.into()).await.unwrap();
    workspace.shutdown(LATER.into()).await.unwrap();
}

#[tokio::test]
async fn agent_message_success_routes_only_to_the_exact_target() {
    let database = TestDatabase::new();
    let source = seed_agent(&database);
    let target = seed_named_agent(&database, "claude");
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
    let mut target_owner =
        DirectMessageService::new(workspace.direct_message(target_transport).unwrap());
    target_owner
        .open("target-owner".into(), target.name.clone(), NOW.into())
        .await
        .unwrap();
    let mut routed =
        DirectMessageService::new(workspace.direct_message_for_agent(target.id).unwrap());
    let message_id = MessageId::new();
    let command = agent_send(message_id, source.id, target.id, "exact target body");

    let Some(AgentDirectMessageOutcome::Delivered(delivered)) =
        routed.send_agent_message(command.clone()).await.unwrap()
    else {
        panic!("expected delivered Agent message")
    };
    assert_eq!(delivered.source_agent_id, source.id);
    assert_eq!(delivered.target_agent_id, target.id);
    assert_eq!(routed.send_agent_message(command).await.unwrap(), None);
    assert!(source_observed.lock().unwrap().messages.is_empty());
    assert_eq!(
        target_observed
            .lock()
            .unwrap()
            .messages
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>(),
        vec!["exact target body"]
    );
    assert_eq!(
        SqliteStore::open(database.path())
            .unwrap()
            .get_message_delivery(message_id, target.id)
            .unwrap()
            .unwrap()
            .status,
        DeliveryStatus::Delivered
    );
    let routed_session = target_observed.lock().unwrap().messages[0].session.clone();
    target_events
        .send(TransportEvent::AgentTextDelta {
            session: routed_session.clone(),
            text: "initial response".into(),
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
        Some(DirectMessageEvent::TextDelta("initial response".into()))
    );
    let Some(DirectMessageEvent::MessageCompleted(completed)) =
        routed.next_event(LATER.into()).await.unwrap()
    else {
        panic!("expected completed target response")
    };
    assert_eq!(completed.sender_id, target.id.to_string());
    let messages = SqliteStore::open(database.path())
        .unwrap()
        .list_messages(delivered.conversation_id)
        .unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].id, message_id);
    assert_eq!(messages[0].sender_type, MemberType::Agent);
    assert_eq!(messages[0].sender_id, source.id.to_string());
    assert_eq!(messages[1].sender_type, MemberType::Agent);
    assert_eq!(messages[1], completed);

    routed.shutdown(LATER.into()).await.unwrap();
    source_owner.shutdown(LATER.into()).await.unwrap();
    target_owner.shutdown(LATER.into()).await.unwrap();
    workspace.shutdown(LATER.into()).await.unwrap();
}

#[tokio::test]
async fn failed_agent_message_retry_is_claimed_once_under_concurrency() {
    let database = TestDatabase::new();
    let source = seed_agent(&database);
    let target = seed_named_agent(&database, "claude");
    let mut workspace =
        WorkspaceRuntime::<FakeTransport>::new(StorageWorker::open(database.path()).unwrap())
            .unwrap();
    let mut initial =
        DirectMessageService::new(workspace.direct_message_for_agent(target.id).unwrap());
    let message_id = MessageId::new();
    assert!(matches!(
        initial
            .send_agent_message(agent_send(message_id, source.id, target.id, "retry body"))
            .await
            .unwrap(),
        Some(AgentDirectMessageOutcome::PersistedFailed(_))
    ));

    let (target_transport, _target_events, target_observed) = FakeTransport::new();
    let mut target_owner =
        DirectMessageService::new(workspace.direct_message(target_transport).unwrap());
    target_owner
        .open("target-owner".into(), target.name, NOW.into())
        .await
        .unwrap();
    let mut retry_a =
        DirectMessageService::new(workspace.direct_message_for_agent(target.id).unwrap());
    let mut retry_b =
        DirectMessageService::new(workspace.direct_message_for_agent(target.id).unwrap());
    let command = RetryAgentDirectMessage {
        message_id,
        target_agent_id: target.id,
        retried_at: LATER.into(),
    };
    let (a, b) = tokio::join!(
        retry_a.retry_agent_message(command.clone()),
        retry_b.retry_agent_message(command.clone())
    );
    let outcomes = [a.unwrap(), b.unwrap()];
    assert_eq!(
        outcomes.iter().filter(|outcome| outcome.is_none()).count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, Some(AgentDirectMessageOutcome::Delivered(_))))
            .count(),
        1
    );
    assert_eq!(initial.retry_agent_message(command).await.unwrap(), None);
    assert_eq!(target_observed.lock().unwrap().messages.len(), 1);

    initial.shutdown(LATER.into()).await.unwrap();
    retry_a.shutdown(LATER.into()).await.unwrap();
    retry_b.shutdown(LATER.into()).await.unwrap();
    target_owner.shutdown(LATER.into()).await.unwrap();
    workspace.shutdown(LATER.into()).await.unwrap();
}

#[tokio::test]
async fn failed_agent_message_retry_revalidates_the_stored_dm_without_replacement() {
    let database = TestDatabase::new();
    let source = seed_agent(&database);
    let target = seed_named_agent(&database, "claude");
    let mut workspace =
        WorkspaceRuntime::<FakeTransport>::new(StorageWorker::open(database.path()).unwrap())
            .unwrap();
    let mut routed =
        DirectMessageService::new(workspace.direct_message_for_agent(target.id).unwrap());
    let message_id = MessageId::new();
    assert!(matches!(
        routed
            .send_agent_message(agent_send(message_id, source.id, target.id, "stored scope"))
            .await
            .unwrap(),
        Some(AgentDirectMessageOutcome::PersistedFailed(_))
    ));
    let conversation_id = SqliteStore::open(database.path())
        .unwrap()
        .get_message(message_id)
        .unwrap()
        .unwrap()
        .conversation_id;
    let connection = rusqlite::Connection::open(database.path()).unwrap();
    connection
        .execute(
            "UPDATE conversations SET status = 'closed' WHERE id = ?1",
            [conversation_id.to_string()],
        )
        .unwrap();

    assert!(matches!(
        routed
            .retry_agent_message(RetryAgentDirectMessage {
                message_id,
                target_agent_id: target.id,
                retried_at: LATER.into(),
            })
            .await
            .unwrap(),
        Some(AgentDirectMessageOutcome::PersistedFailed(DirectMessageError::Runtime(message)))
            if message.contains("message_delivery.agent_dm_scope")
    ));
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM conversations", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        SqliteStore::open(database.path())
            .unwrap()
            .get_message_delivery(message_id, target.id)
            .unwrap()
            .unwrap()
            .status,
        DeliveryStatus::Failed
    );

    routed.shutdown(LATER.into()).await.unwrap();
    workspace.shutdown(LATER.into()).await.unwrap();
}

#[tokio::test]
async fn agent_message_replay_rejects_message_and_delivery_conflicts() {
    let database = TestDatabase::new();
    let source = seed_agent(&database);
    let target = seed_named_agent(&database, "claude");
    let other = seed_named_agent(&database, "gemini");
    let mut workspace =
        WorkspaceRuntime::<FakeTransport>::new(StorageWorker::open(database.path()).unwrap())
            .unwrap();
    let mut target_route =
        DirectMessageService::new(workspace.direct_message_for_agent(target.id).unwrap());
    let message_id = MessageId::new();
    assert!(matches!(
        target_route
            .send_agent_message(agent_send(message_id, source.id, target.id, "original"))
            .await
            .unwrap(),
        Some(AgentDirectMessageOutcome::PersistedFailed(_))
    ));
    assert!(matches!(
        target_route
            .send_agent_message(agent_send(message_id, source.id, target.id, "changed"))
            .await,
        Err(DirectMessageError::Runtime(message)) if message.contains("different content")
    ));
    let mut other_route =
        DirectMessageService::new(workspace.direct_message_for_agent(other.id).unwrap());
    assert!(matches!(
        other_route
            .send_agent_message(agent_send(message_id, source.id, other.id, "original"))
            .await,
        Err(DirectMessageError::Runtime(message)) if message.contains("different content")
    ));
    assert_eq!(
        SqliteStore::open(database.path())
            .unwrap()
            .get_message_delivery(message_id, other.id)
            .unwrap(),
        None
    );

    let delivery_conflict_id = MessageId::new();
    let mut store = SqliteStore::open(database.path()).unwrap();
    let conversation = store
        .get_or_create_agent_dm(source.id, target.id, NOW)
        .unwrap();
    store
        .insert_message(&Message {
            id: delivery_conflict_id,
            conversation_id: conversation.id,
            sender_type: MemberType::Agent,
            sender_id: source.id.to_string(),
            body: "legacy exact body".into(),
            reply_to: None,
            metadata: json!({
                "july": {"schema": 1, "channel": "dm", "direction": "outbound"}
            }),
            created_at: NOW.into(),
        })
        .unwrap();
    drop(store);
    assert!(matches!(
        target_route
            .send_agent_message(agent_send(
                delivery_conflict_id,
                source.id,
                target.id,
                "legacy exact body",
            ))
            .await,
        Err(DirectMessageError::Runtime(message)) if message.contains("delivery for message")
    ));

    target_route.shutdown(LATER.into()).await.unwrap();
    other_route.shutdown(LATER.into()).await.unwrap();
    workspace.shutdown(LATER.into()).await.unwrap();
}

#[tokio::test]
async fn agent_message_open_and_send_failures_leave_failed_deliveries() {
    for failure in ["open", "send"] {
        let database = TestDatabase::new();
        let source = seed_agent(&database);
        let target = seed_named_agent(&database, "claude");
        let (mut transport, _events, observed) = FakeTransport::new();
        transport.create_fails_at = (failure == "open").then_some(2);
        transport.send_fails_at = (failure == "send").then_some(1);
        let mut workspace =
            WorkspaceRuntime::new(StorageWorker::open(database.path()).unwrap()).unwrap();
        let mut target_owner =
            DirectMessageService::new(workspace.direct_message(transport).unwrap());
        target_owner
            .open("target-owner".into(), target.name, NOW.into())
            .await
            .unwrap();
        let mut routed =
            DirectMessageService::new(workspace.direct_message_for_agent(target.id).unwrap());
        let message_id = MessageId::new();

        assert!(matches!(
            routed
                .send_agent_message(agent_send(message_id, source.id, target.id, "failure body"))
                .await
                .unwrap(),
            Some(AgentDirectMessageOutcome::PersistedFailed(DirectMessageError::Runtime(message)))
                if message.contains(&format!("fixture {failure} failure"))
        ));
        assert_eq!(
            SqliteStore::open(database.path())
                .unwrap()
                .get_message_delivery(message_id, target.id)
                .unwrap()
                .unwrap()
                .status,
            DeliveryStatus::Failed
        );
        assert_eq!(
            observed.lock().unwrap().messages.len(),
            usize::from(failure == "send")
        );

        routed.shutdown(LATER.into()).await.unwrap();
        target_owner.shutdown(LATER.into()).await.unwrap();
        workspace.shutdown(LATER.into()).await.unwrap();
    }
}

#[tokio::test]
async fn delivery_record_failure_immediately_recovers_agent_message_to_failed() {
    let database = TestDatabase::new();
    let source = seed_agent(&database);
    let target = seed_named_agent(&database, "claude");
    let (transport, _events, observed) = FakeTransport::new();
    let mut workspace =
        WorkspaceRuntime::new(StorageWorker::open(database.path()).unwrap()).unwrap();
    let mut target_owner = DirectMessageService::new(workspace.direct_message(transport).unwrap());
    target_owner
        .open("target-owner".into(), target.name, NOW.into())
        .await
        .unwrap();
    let connection = rusqlite::Connection::open(database.path()).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER fail_dm_delivered_record
             BEFORE UPDATE OF status ON message_deliveries
             WHEN NEW.status = 'delivered'
             BEGIN SELECT RAISE(ABORT, 'fixture delivered record failure'); END;",
        )
        .unwrap();
    let mut routed =
        DirectMessageService::new(workspace.direct_message_for_agent(target.id).unwrap());
    let message_id = MessageId::new();

    assert!(matches!(
        routed
            .send_agent_message(agent_send(message_id, source.id, target.id, "record body"))
            .await
            .unwrap(),
        Some(AgentDirectMessageOutcome::PersistedFailed(DirectMessageError::Runtime(message)))
            if message.contains("fixture delivered record failure")
    ));
    assert_eq!(observed.lock().unwrap().messages.len(), 1);
    assert_eq!(
        SqliteStore::open(database.path())
            .unwrap()
            .get_message_delivery(message_id, target.id)
            .unwrap()
            .unwrap()
            .status,
        DeliveryStatus::Failed
    );

    connection
        .execute_batch("DROP TRIGGER fail_dm_delivered_record")
        .unwrap();
    routed.shutdown(LATER.into()).await.unwrap();
    target_owner.shutdown(LATER.into()).await.unwrap();
    workspace.shutdown(LATER.into()).await.unwrap();
}

#[tokio::test]
async fn agent_message_record_and_failed_recovery_errors_are_combined() {
    let database = TestDatabase::new();
    let source = seed_agent(&database);
    let target = seed_named_agent(&database, "claude");
    let (transport, _events, observed) = FakeTransport::new();
    let mut workspace =
        WorkspaceRuntime::new(StorageWorker::open(database.path()).unwrap()).unwrap();
    let mut target_owner = DirectMessageService::new(workspace.direct_message(transport).unwrap());
    target_owner
        .open("target-owner".into(), target.name, NOW.into())
        .await
        .unwrap();
    let connection = rusqlite::Connection::open(database.path()).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER fail_all_dm_delivery_updates
             BEFORE UPDATE ON message_deliveries
             BEGIN SELECT RAISE(ABORT, 'fixture all delivery updates fail'); END;",
        )
        .unwrap();
    let mut routed =
        DirectMessageService::new(workspace.direct_message_for_agent(target.id).unwrap());
    let message_id = MessageId::new();

    let outcome = routed
        .send_agent_message(agent_send(
            message_id,
            source.id,
            target.id,
            "dual error body",
        ))
        .await
        .unwrap()
        .unwrap();
    let AgentDirectMessageOutcome::PersistedFailed(
        DirectMessageError::DeliveryStateRecoveryFailed { primary, recovery },
    ) = outcome
    else {
        panic!("expected combined delivery-state error, got {outcome:?}")
    };
    assert!(
        primary
            .to_string()
            .contains("fixture all delivery updates fail")
    );
    assert!(
        recovery
            .to_string()
            .contains("fixture all delivery updates fail")
    );
    assert_eq!(observed.lock().unwrap().messages.len(), 1);
    assert_eq!(
        SqliteStore::open(database.path())
            .unwrap()
            .get_message_delivery(message_id, target.id)
            .unwrap()
            .unwrap()
            .status,
        DeliveryStatus::Pending
    );

    connection
        .execute_batch("DROP TRIGGER fail_all_dm_delivery_updates")
        .unwrap();
    routed.shutdown(LATER.into()).await.unwrap();
    target_owner.shutdown(LATER.into()).await.unwrap();
    workspace.shutdown(LATER.into()).await.unwrap();
}
