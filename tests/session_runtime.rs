use july_workspace::domain::{
    Agent, AgentId, Conversation, ConversationKind, PermissionDecision, PermissionOption,
    PermissionOutcome, SessionBinding, SessionBindingStatus,
};
use july_workspace::runtime::{SessionManager, StorageWorker};
use july_workspace::storage::SqliteStore;
use july_workspace::transport::{
    AgentConnection, AgentTransport, CreateSession, PermissionResponse, ResumeSession, SendMessage,
    SessionCreated, SessionRef, SessionResumed, TransportError, TransportEvent, TransportEvents,
};
use serde_json::json;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const NOW: &str = "2026-08-11T00:00:00Z";

struct TestDatabase(PathBuf);

impl TestDatabase {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("july-runtime-{}.sqlite3", ulid::Ulid::generate())))
    }
}

struct FakeTransport {
    events: Option<tokio::sync::mpsc::Receiver<TransportEvent>>,
    permission_responses: Arc<Mutex<Vec<PermissionResponse>>>,
    resumed_sessions: Arc<Mutex<Vec<SessionRef>>>,
}

type FakeTransportParts = (
    FakeTransport,
    tokio::sync::mpsc::Sender<TransportEvent>,
    Arc<Mutex<Vec<PermissionResponse>>>,
    Arc<Mutex<Vec<SessionRef>>>,
);

impl FakeTransport {
    fn new() -> FakeTransportParts {
        let (sender, events) = tokio::sync::mpsc::channel(8);
        let permission_responses = Arc::new(Mutex::new(Vec::new()));
        let resumed_sessions = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                events: Some(events),
                permission_responses: permission_responses.clone(),
                resumed_sessions: resumed_sessions.clone(),
            },
            sender,
            permission_responses,
            resumed_sessions,
        )
    }
}

impl AgentTransport for FakeTransport {
    async fn connect(&mut self, _agent: &AgentConnection) -> Result<(), TransportError> {
        Ok(())
    }

    async fn create_session(
        &mut self,
        request: CreateSession,
    ) -> Result<SessionCreated, TransportError> {
        Ok(SessionCreated {
            session: SessionRef {
                binding_id: request.binding_id,
                remote_session_id: "remote-created".into(),
            },
        })
    }

    async fn resume_session(
        &mut self,
        request: ResumeSession,
    ) -> Result<SessionResumed, TransportError> {
        self.resumed_sessions
            .lock()
            .unwrap()
            .push(request.session.clone());
        Ok(SessionResumed {
            session: request.session,
        })
    }

    async fn send_message(&mut self, _request: SendMessage) -> Result<(), TransportError> {
        Ok(())
    }

    async fn cancel_turn(&mut self, _session: SessionRef) -> Result<(), TransportError> {
        Ok(())
    }

    async fn respond_permission(
        &mut self,
        response: PermissionResponse,
    ) -> Result<(), TransportError> {
        self.permission_responses.lock().unwrap().push(response);
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

impl Drop for TestDatabase {
    fn drop(&mut self) {
        for suffix in ["", "-shm", "-wal"] {
            let _ = std::fs::remove_file(format!("{}{}", self.0.display(), suffix));
        }
    }
}

#[tokio::test]
async fn storage_worker_owns_binding_and_permission_writes() {
    let database = TestDatabase::new();
    let store = SqliteStore::open(&database.0).unwrap();
    let agent = Agent {
        id: Default::default(),
        name: "worker".into(),
        project_root: "/tmp".into(),
        transport_type: "acp".into(),
        transport_config: json!({}),
        status: "active".into(),
        metadata: json!({}),
        created_at: NOW.into(),
        updated_at: NOW.into(),
    };
    let conversation = Conversation {
        id: Default::default(),
        kind: ConversationKind::Dm,
        room_id: None,
        title: None,
        goal: None,
        parent_conversation_id: None,
        origin_conversation_id: None,
        status: "active".into(),
        created_at: NOW.into(),
        updated_at: NOW.into(),
    };
    store.insert_agent(&agent).unwrap();
    store.insert_conversation(&conversation).unwrap();
    drop(store);

    let binding = SessionBinding {
        id: Default::default(),
        conversation_id: conversation.id,
        agent_id: agent.id,
        transport_type: "acp".into(),
        remote_session_id: Some("remote-1".into()),
        generation: 1,
        status: SessionBindingStatus::Active,
        created_at: NOW.into(),
        last_used_at: NOW.into(),
    };
    let decision = PermissionDecision {
        id: "decision-1".into(),
        session_binding_id: binding.id,
        correlation_id: "permission-1".into(),
        options: vec![PermissionOption {
            id: "allow-once".into(),
            label: "Allow once".into(),
        }],
        outcome: PermissionOutcome::Selected("allow-once".into()),
        decided_at: NOW.into(),
    };

    let mut worker = StorageWorker::open(&database.0).unwrap();
    worker
        .insert_session_binding(binding.clone())
        .await
        .unwrap();
    assert_eq!(
        worker
            .get_current_session_binding(conversation.id, agent.id)
            .await
            .unwrap(),
        Some(binding)
    );
    worker
        .insert_permission_decision(decision.clone())
        .await
        .unwrap();
    assert_eq!(
        worker
            .get_permission_decision("decision-1".into())
            .await
            .unwrap(),
        Some(decision)
    );
    worker.shutdown().await.unwrap();
}

#[tokio::test]
async fn session_manager_persists_create_and_agent_disconnect() {
    let database = TestDatabase::new();
    let (agent, conversation) = seed_workspace(&database);
    let storage = StorageWorker::open(&database.0).unwrap();
    let (transport, event_sender, permission_responses, resumed_sessions) = FakeTransport::new();
    let mut manager = SessionManager::connect(
        transport,
        storage,
        AgentConnection {
            agent_id: agent.id,
            project_root: std::env::temp_dir(),
        },
    )
    .await
    .unwrap();
    let binding = SessionBinding {
        id: Default::default(),
        conversation_id: conversation.id,
        agent_id: agent.id,
        transport_type: "acp".into(),
        remote_session_id: None,
        generation: 1,
        status: SessionBindingStatus::Active,
        created_at: NOW.into(),
        last_used_at: NOW.into(),
    };

    let created = manager
        .create_session(binding.clone(), std::env::temp_dir())
        .await
        .unwrap();
    let forged_binding = SessionBinding {
        remote_session_id: Some("forged-remote".into()),
        ..binding
    };
    manager
        .resume_session(&forged_binding, std::env::temp_dir(), NOW.into())
        .await
        .unwrap();
    assert_eq!(
        resumed_sessions.lock().unwrap()[0].remote_session_id,
        "remote-created"
    );
    let request_id = july_workspace::transport::PermissionRequestId::from("permission-unknown");
    event_sender
        .send(TransportEvent::PermissionRequested(
            july_workspace::transport::PermissionRequest {
                session: created.clone(),
                request_id: request_id.clone(),
                options: vec![PermissionOption {
                    id: "allow-once".into(),
                    label: "Allow once".into(),
                }],
            },
        ))
        .await
        .unwrap();
    manager.next_event(NOW).await.unwrap();
    assert!(matches!(
        manager
            .respond_permission(
                PermissionResponse {
                    session: created.clone(),
                    request_id,
                    outcome: PermissionOutcome::Selected("not-advertised".into()),
                },
                NOW.into(),
            )
            .await,
        Err(july_workspace::runtime::RuntimeError::Transport(
            TransportError::PermissionOptionNotAdvertised(_)
        ))
    ));
    assert_eq!(
        permission_responses.lock().unwrap()[0].outcome,
        PermissionOutcome::Cancelled
    );
    let missing_binding_request =
        july_workspace::transport::PermissionRequestId::from("permission-storage-failure");
    let missing_binding_session = SessionRef {
        binding_id: Default::default(),
        remote_session_id: created.remote_session_id.clone(),
    };
    event_sender
        .send(TransportEvent::PermissionRequested(
            july_workspace::transport::PermissionRequest {
                session: missing_binding_session.clone(),
                request_id: missing_binding_request.clone(),
                options: vec![PermissionOption {
                    id: "allow-once".into(),
                    label: "Allow once".into(),
                }],
            },
        ))
        .await
        .unwrap();
    manager.next_event(NOW).await.unwrap();
    assert!(matches!(
        manager
            .respond_permission(
                PermissionResponse {
                    session: missing_binding_session,
                    request_id: missing_binding_request,
                    outcome: PermissionOutcome::Selected("allow-once".into()),
                },
                NOW.into(),
            )
            .await,
        Err(july_workspace::runtime::RuntimeError::Storage(_))
    ));
    assert_eq!(
        permission_responses.lock().unwrap()[1].outcome,
        PermissionOutcome::Cancelled
    );
    let cancel_request = july_workspace::transport::PermissionRequestId::from("permission-cancel");
    event_sender
        .send(TransportEvent::PermissionRequested(
            july_workspace::transport::PermissionRequest {
                session: created.clone(),
                request_id: cancel_request,
                options: vec![],
            },
        ))
        .await
        .unwrap();
    manager.next_event(NOW).await.unwrap();
    manager
        .cancel_turn(created.clone(), NOW.into())
        .await
        .unwrap();
    let disconnect_request =
        july_workspace::transport::PermissionRequestId::from("permission-disconnect");
    event_sender
        .send(TransportEvent::PermissionRequested(
            july_workspace::transport::PermissionRequest {
                session: created.clone(),
                request_id: disconnect_request,
                options: vec![],
            },
        ))
        .await
        .unwrap();
    manager.next_event(NOW).await.unwrap();
    event_sender
        .send(TransportEvent::TransportDisconnected {
            agent_id: agent.id,
            reason: "fixture EOF".into(),
        })
        .await
        .unwrap();
    assert!(matches!(
        manager.next_event(NOW).await.unwrap(),
        Some(TransportEvent::TransportDisconnected { .. })
    ));
    let shutdown_request =
        july_workspace::transport::PermissionRequestId::from("permission-shutdown");
    event_sender
        .send(TransportEvent::PermissionRequested(
            july_workspace::transport::PermissionRequest {
                session: created.clone(),
                request_id: shutdown_request,
                options: vec![],
            },
        ))
        .await
        .unwrap();
    manager.next_event(NOW).await.unwrap();
    let queued_shutdown_request =
        july_workspace::transport::PermissionRequestId::from("permission-queued-shutdown");
    event_sender
        .send(TransportEvent::PermissionRequested(
            july_workspace::transport::PermissionRequest {
                session: created.clone(),
                request_id: queued_shutdown_request,
                options: vec![],
            },
        ))
        .await
        .unwrap();
    manager.shutdown(NOW.into()).await.unwrap();

    let store = SqliteStore::open(&database.0).unwrap();
    let stored = store
        .get_session_binding(created.binding_id)
        .unwrap()
        .unwrap();
    assert_eq!(stored.remote_session_id.as_deref(), Some("remote-created"));
    assert_eq!(stored.status, SessionBindingStatus::Disconnected);
    let connection = rusqlite::Connection::open(&database.0).unwrap();
    let outcome: String = connection
        .query_row(
            "SELECT outcome FROM permission_decisions WHERE correlation_id = ?1",
            ["permission-unknown"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(outcome, "cancelled");
    let cancelled_audits: i64 = connection
        .query_row(
            "SELECT count(*) FROM permission_decisions
             WHERE correlation_id IN (
                 'permission-cancel', 'permission-disconnect', 'permission-shutdown',
                 'permission-queued-shutdown'
             )
               AND outcome = 'cancelled'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(cancelled_audits, 4);
}

fn seed_workspace(database: &TestDatabase) -> (Agent, Conversation) {
    let store = SqliteStore::open(&database.0).unwrap();
    let agent = Agent {
        id: AgentId::new(),
        name: "manager-worker".into(),
        project_root: "/tmp".into(),
        transport_type: "acp".into(),
        transport_config: json!({}),
        status: "active".into(),
        metadata: json!({}),
        created_at: NOW.into(),
        updated_at: NOW.into(),
    };
    let conversation = Conversation {
        id: Default::default(),
        kind: ConversationKind::Dm,
        room_id: None,
        title: None,
        goal: None,
        parent_conversation_id: None,
        origin_conversation_id: None,
        status: "active".into(),
        created_at: NOW.into(),
        updated_at: NOW.into(),
    };
    store.insert_agent(&agent).unwrap();
    store.insert_conversation(&conversation).unwrap();
    (agent, conversation)
}
