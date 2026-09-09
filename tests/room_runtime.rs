use july_workspace::domain::{
    Agent, AgentId, MemberType, PermissionOption, PermissionOutcome, Room, RoomId, RoomMessage,
    RoomMessageId, SessionBindingStatus,
};
use july_workspace::runtime::{RoomRuntimeEvent, StorageWorker, WorkspaceRuntime};
use july_workspace::storage::SqliteStore;
use july_workspace::transport::{
    AgentConnection, AgentTransport, CreateSession, PermissionRequest, PermissionRequestId,
    PermissionResponse, ResumeSession, SendMessage, SessionCreated, SessionRef, SessionResumed,
    TransportError, TransportEvent, TransportEvents,
};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
const NOW: &str = "2026-09-07T00:00:00Z";
struct TestDatabase {
    directory: PathBuf,
    path: PathBuf,
}

impl TestDatabase {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("july-room-runtime-{}", ulid::Ulid::generate()));
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
    cancels: usize,
    fail_send: bool,
    lose_resume: bool,
    fail_resume: bool,
    close_on_resume: Option<PathBuf>,
    create_gate: Option<Arc<tokio::sync::Notify>>,
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
        let gate = self.observed.lock().unwrap().create_gate.clone();
        if let Some(gate) = gate {
            gate.notified().await;
        }
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
        let close_path = self.observed.lock().unwrap().close_on_resume.clone();
        if let Some(path) = close_path {
            SqliteStore::open(path)
                .unwrap()
                .update_session_binding_status(
                    request.session.binding_id,
                    SessionBindingStatus::Closed,
                    NOW,
                )
                .unwrap();
            return Err(TransportError::SessionLost(
                request.session.remote_session_id,
            ));
        }
        if self.observed.lock().unwrap().fail_resume {
            return Err(TransportError::Protocol("uncertain resume failure".into()));
        }
        if self.observed.lock().unwrap().lose_resume {
            return Err(TransportError::SessionLost(
                request.session.remote_session_id,
            ));
        }
        Ok(SessionResumed {
            session: request.session,
        })
    }

    async fn send_message(&mut self, request: SendMessage) -> Result<(), TransportError> {
        self.observed.lock().unwrap().messages.push(request);
        if self.observed.lock().unwrap().fail_send {
            return Err(TransportError::SessionLost("ambiguous send".into()));
        }
        Ok(())
    }

    async fn cancel_turn(&mut self, _session: SessionRef) -> Result<(), TransportError> {
        self.observed.lock().unwrap().cancels += 1;
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

fn seed(database: &TestDatabase) -> (Agent, Agent, Room, RoomMessage) {
    let agent = |name: &str| Agent {
        id: AgentId::new(),
        name: name.into(),
        project_root: "/workspace/project".into(),
        transport_type: "acp".into(),
        transport_config: json!({}),
        status: "active".into(),
        metadata: json!({}),
        created_at: NOW.into(),
        updated_at: NOW.into(),
    };
    let pay = agent("pay");
    let infra = agent("infra");
    let room = Room {
        id: RoomId::new(),
        name: "VNA".into(),
        description: None,
        status: "active".into(),
        created_at: NOW.into(),
        updated_at: NOW.into(),
    };
    let mut store = SqliteStore::open(database.path()).unwrap();
    store.insert_agent(&pay).unwrap();
    store.insert_agent(&infra).unwrap();
    store.create_room(&room).unwrap();
    store.add_room_member(room.id, pay.id, None, NOW).unwrap();
    store.add_room_member(room.id, infra.id, None, NOW).unwrap();
    let message = RoomMessage {
        id: RoomMessageId::new(),
        room_id: room.id,
        sender_type: MemberType::User,
        sender_id: "local-user".into(),
        body: "@pay check refund".into(),
        mentions: vec![pay.id],
        reply_to: None,
        created_at: NOW.into(),
    };
    store.append_room_message(&message).unwrap();
    (pay, infra, room, message)
}

#[tokio::test]
async fn room_activation_is_selective_durable_and_does_not_create_conversations() {
    let database = TestDatabase::new();
    let (pay, infra, room, message) = seed(&database);
    let (transport, events, observed) = FakeTransport::new();
    let mut workspace =
        WorkspaceRuntime::new(StorageWorker::open(database.path()).unwrap()).unwrap();
    workspace
        .register_agent(
            AgentConnection {
                agent_id: pay.id,
                project_root: pay.project_root.clone().into(),
            },
            transport,
        )
        .await
        .unwrap();
    assert!(
        workspace
            .activate_room_message(message.id, infra.id, NOW.into())
            .await
            .is_err()
    );
    let (infra_transport, _infra_events, infra_observed) = FakeTransport::new();
    workspace
        .register_agent(
            AgentConnection {
                agent_id: infra.id,
                project_root: infra.project_root.clone().into(),
            },
            infra_transport,
        )
        .await
        .unwrap();
    assert!(
        workspace
            .activate_room_message(message.id, infra.id, NOW.into())
            .await
            .is_err()
    );
    assert!(infra_observed.lock().unwrap().creates.is_empty());
    assert!(infra_observed.lock().unwrap().messages.is_empty());
    let mut active = workspace
        .activate_room_message(message.id, pay.id, NOW.into())
        .await
        .unwrap()
        .unwrap();
    assert!(
        workspace
            .activate_room_message(message.id, pay.id, NOW.into())
            .await
            .unwrap()
            .is_none()
    );
    let session = active.session().clone();
    assert_eq!(observed.lock().unwrap().creates.len(), 1);
    assert_eq!(observed.lock().unwrap().messages.len(), 1);
    assert!(
        observed.lock().unwrap().messages[0]
            .content
            .contains(&message.body)
    );
    for event in [
        TransportEvent::AgentTextDelta {
            session: session.clone(),
            text: "PRIVATE reasoning".into(),
        },
        TransportEvent::ToolCallStarted {
            session: session.clone(),
            tool_call_id: "tool".into(),
            title: "PRIVATE file".into(),
        },
        TransportEvent::TurnCompleted { session },
    ] {
        events.send(event).await.unwrap();
    }
    assert_eq!(
        active.next_event(NOW.into()).await.unwrap(),
        Some(RoomRuntimeEvent::Completed)
    );
    assert_eq!(active.next_event(NOW.into()).await.unwrap(), None);
    let store = SqliteStore::open(database.path()).unwrap();
    assert_eq!(
        store.list_recent_room_messages(room.id, 50).unwrap().0,
        vec![message.clone()]
    );
    let binding = store
        .get_room_session_binding(room.id, pay.id)
        .unwrap()
        .unwrap();
    assert_eq!(binding.status, SessionBindingStatus::Disconnected);
    let connection = rusqlite::Connection::open(database.path()).unwrap();
    for table in ["conversations", "messages", "work_items"] {
        assert_eq!(
            connection
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    assert_eq!(
        connection
            .query_row("SELECT status FROM room_message_activations", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        "completed"
    );
    workspace.shutdown(NOW.into()).await.unwrap();
    let (transport, _events, observed) = FakeTransport::new();
    let mut restarted =
        WorkspaceRuntime::new(StorageWorker::open(database.path()).unwrap()).unwrap();
    restarted
        .register_agent(
            AgentConnection {
                agent_id: pay.id,
                project_root: pay.project_root.into(),
            },
            transport,
        )
        .await
        .unwrap();
    assert!(
        restarted
            .activate_room_message(message.id, pay.id, NOW.into())
            .await
            .unwrap()
            .is_none()
    );
    assert!(observed.lock().unwrap().messages.is_empty());
    restarted.shutdown(NOW.into()).await.unwrap();
}

#[tokio::test]
async fn room_publication_scope_is_revoked_on_cancel_drop_and_unconsumed_completion() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    async fn publish(config: &july_workspace::transport::RoomMessagingConfig) -> bool {
        let Ok(mut stream) = tokio::net::UnixStream::connect(&config.socket).await else {
            return false;
        };
        let request = json!({"token":config.token,"arguments":{"targets":["infra"],"body":"shared","request_id":"once"}});
        if stream
            .write_all(format!("{request}\n").as_bytes())
            .await
            .is_err()
        {
            return false;
        }
        let mut response = String::new();
        if BufReader::new(stream)
            .read_line(&mut response)
            .await
            .is_err()
        {
            return false;
        }
        serde_json::from_str::<serde_json::Value>(&response)
            .ok()
            .is_some_and(|v| v.get("result").is_some())
    }
    for ending in ["cancel", "drop", "complete"] {
        let database = TestDatabase::new();
        let (pay, _, room, message) = seed(&database);
        let (transport, events, observed) = FakeTransport::new();
        let mut workspace =
            WorkspaceRuntime::new(StorageWorker::open(database.path()).unwrap()).unwrap();
        workspace
            .register_agent(
                AgentConnection {
                    agent_id: pay.id,
                    project_root: pay.project_root.into(),
                },
                transport,
            )
            .await
            .unwrap();
        let mut active = workspace
            .activate_room_message(message.id, pay.id, NOW.into())
            .await
            .unwrap()
            .unwrap();
        let config = observed.lock().unwrap().creates[0]
            .room_messaging
            .clone()
            .unwrap();
        assert!(publish(&config).await);
        if ending != "complete" {
            let shared = active.next_event(NOW.into()).await.unwrap().unwrap();
            assert!(
                matches!(shared, RoomRuntimeEvent::SharedMessage(ref saved) if saved.body == "shared")
            );
        }
        match ending {
            "cancel" => {
                active.cancel(NOW.into()).await.unwrap();
                drop(active);
            }
            "drop" => drop(active),
            _ => {
                events
                    .send(TransportEvent::TurnCompleted {
                        session: active.session().clone(),
                    })
                    .await
                    .unwrap();
                // The owner must revoke before the UI consumes completion.
                tokio::time::timeout(std::time::Duration::from_secs(2), async {
                    while config.socket.exists() {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
                assert!(!publish(&config).await);
                // Scope is gone and terminal is queued, but the committed shared
                // message must still be delivered first.
                let shared = active.next_event(NOW.into()).await.unwrap().unwrap();
                assert!(
                    matches!(shared, RoomRuntimeEvent::SharedMessage(ref saved) if saved.body == "shared")
                );
                assert_eq!(
                    active.next_event(NOW.into()).await.unwrap(),
                    Some(RoomRuntimeEvent::Completed)
                );
            }
        }
        assert!(!publish(&config).await, "{ending}");
        workspace.shutdown(NOW.into()).await.unwrap();
        let store = SqliteStore::open(database.path()).unwrap();
        assert_eq!(
            store
                .list_recent_room_messages(room.id, 10)
                .unwrap()
                .0
                .len(),
            2
        );
    }
}

#[tokio::test]
async fn room_sessions_resume_without_history_replay_and_keep_permissions_private() {
    let database = TestDatabase::new();
    let (pay, _, room, message) = seed(&database);
    let (transport, events, observed) = FakeTransport::new();
    let mut workspace =
        WorkspaceRuntime::new(StorageWorker::open(database.path()).unwrap()).unwrap();
    workspace
        .register_agent(
            AgentConnection {
                agent_id: pay.id,
                project_root: pay.project_root.clone().into(),
            },
            transport,
        )
        .await
        .unwrap();
    let mut active = workspace
        .activate_room_message(message.id, pay.id, NOW.into())
        .await
        .unwrap()
        .unwrap();
    let session = active.session().clone();
    let request = PermissionRequest {
        session: session.clone(),
        request_id: PermissionRequestId::from("request"),
        prompt: "Write file?".into(),
        options: vec![PermissionOption {
            id: "allow".into(),
            label: "Allow once".into(),
        }],
    };
    events
        .send(TransportEvent::PermissionRequested(request.clone()))
        .await
        .unwrap();
    assert_eq!(
        active.next_event(NOW.into()).await.unwrap(),
        Some(RoomRuntimeEvent::PermissionRequested(request))
    );
    active
        .respond_permission(
            PermissionRequestId::from("request"),
            PermissionOutcome::Selected("allow".into()),
            NOW.into(),
        )
        .await
        .unwrap();
    active.cancel(NOW.into()).await.unwrap();
    active.cancel(NOW.into()).await.unwrap();
    events
        .send(TransportEvent::TurnCompleted {
            session: session.clone(),
        })
        .await
        .unwrap();
    assert_eq!(
        active.next_event(NOW.into()).await.unwrap(),
        Some(RoomRuntimeEvent::Cancelled)
    );
    assert_eq!(observed.lock().unwrap().cancels, 1);
    assert_eq!(observed.lock().unwrap().permissions.len(), 1);
    let mut next = message.clone();
    next.id = RoomMessageId::new();
    next.body = "@pay fresh question".into();
    let mut store = SqliteStore::open(database.path()).unwrap();
    store.append_room_message(&next).unwrap();
    let mut active = workspace
        .activate_room_message(next.id, pay.id, NOW.into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(active.session(), &session);
    assert_eq!(observed.lock().unwrap().creates.len(), 1);
    assert_eq!(observed.lock().unwrap().resumes.len(), 1);
    assert_eq!(
        observed.lock().unwrap().messages.len(),
        2,
        "no conversation recovery capsule was sent"
    );
    events
        .send(TransportEvent::TurnCompleted { session })
        .await
        .unwrap();
    active.next_event(NOW.into()).await.unwrap();
    assert_eq!(
        store
            .list_recent_room_messages(room.id, 50)
            .unwrap()
            .0
            .len(),
        2
    );
    let connection = rusqlite::Connection::open(database.path()).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM permission_decisions", [], |row| row
                .get::<_, i64>(
                0
            ))
            .unwrap(),
        1
    );
    workspace.shutdown(NOW.into()).await.unwrap();
}

#[tokio::test]
async fn room_activation_rechecks_departed_targets_and_senders_without_runtime_side_effects() {
    let database = TestDatabase::new();
    let (pay, infra, room, message) = seed(&database);
    let (transport, _events, observed) = FakeTransport::new();
    let mut workspace =
        WorkspaceRuntime::new(StorageWorker::open(database.path()).unwrap()).unwrap();
    workspace
        .register_agent(
            AgentConnection {
                agent_id: pay.id,
                project_root: pay.project_root.clone().into(),
            },
            transport,
        )
        .await
        .unwrap();
    let mut store = SqliteStore::open(database.path()).unwrap();
    let mut from_agent = message.clone();
    from_agent.id = RoomMessageId::new();
    from_agent.sender_type = MemberType::Agent;
    from_agent.sender_id = infra.id.to_string();
    store.append_room_message(&from_agent).unwrap();
    store.remove_room_member(room.id, infra.id, NOW).unwrap();
    assert!(
        workspace
            .activate_room_message(from_agent.id, pay.id, NOW.into())
            .await
            .is_err()
    );
    store.remove_room_member(room.id, pay.id, NOW).unwrap();
    assert!(
        workspace
            .activate_room_message(message.id, pay.id, NOW.into())
            .await
            .is_err()
    );
    assert!(observed.lock().unwrap().creates.is_empty());
    assert!(observed.lock().unwrap().messages.is_empty());
    assert!(
        store
            .get_room_session_binding(room.id, pay.id)
            .unwrap()
            .is_none()
    );
    workspace.shutdown(NOW.into()).await.unwrap();
}

#[tokio::test]
async fn ambiguous_send_is_recorded_and_never_automatically_retried() {
    let database = TestDatabase::new();
    let (pay, _, room, message) = seed(&database);
    let (transport, _events, observed) = FakeTransport::new();
    observed.lock().unwrap().fail_send = true;
    let mut workspace =
        WorkspaceRuntime::new(StorageWorker::open(database.path()).unwrap()).unwrap();
    workspace
        .register_agent(
            AgentConnection {
                agent_id: pay.id,
                project_root: pay.project_root.into(),
            },
            transport,
        )
        .await
        .unwrap();
    assert!(
        workspace
            .activate_room_message(message.id, pay.id, NOW.into())
            .await
            .is_err()
    );
    observed.lock().unwrap().fail_send = false;
    assert!(
        workspace
            .activate_room_message(message.id, pay.id, NOW.into())
            .await
            .unwrap()
            .is_none()
    );
    let store = SqliteStore::open(database.path()).unwrap();
    assert_eq!(
        store
            .get_room_session_binding(room.id, pay.id)
            .unwrap()
            .unwrap()
            .status,
        SessionBindingStatus::Lost
    );
    assert_eq!(observed.lock().unwrap().messages.len(), 1);
    let connection = rusqlite::Connection::open(database.path()).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM agent_room_cursors", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        connection
            .query_row("SELECT status FROM room_message_activations", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        "failed"
    );
    workspace.shutdown(NOW.into()).await.unwrap();
}

#[tokio::test]
async fn missing_room_session_recreates_for_new_turn_without_private_capsule() {
    let database = TestDatabase::new();
    let (pay, _, room, message) = seed(&database);
    let (transport, events, observed) = FakeTransport::new();
    let mut workspace =
        WorkspaceRuntime::new(StorageWorker::open(database.path()).unwrap()).unwrap();
    workspace
        .register_agent(
            AgentConnection {
                agent_id: pay.id,
                project_root: pay.project_root.into(),
            },
            transport,
        )
        .await
        .unwrap();
    let mut active = workspace
        .activate_room_message(message.id, pay.id, NOW.into())
        .await
        .unwrap()
        .unwrap();
    events
        .send(TransportEvent::TurnCompleted {
            session: active.session().clone(),
        })
        .await
        .unwrap();
    active.next_event(NOW.into()).await.unwrap();
    observed.lock().unwrap().lose_resume = true;
    let mut next = message;
    next.id = RoomMessageId::new();
    let mut store = SqliteStore::open(database.path()).unwrap();
    store.append_room_message(&next).unwrap();
    let recovered = workspace
        .activate_room_message(next.id, pay.id, NOW.into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(observed.lock().unwrap().creates.len(), 2);
    assert_eq!(observed.lock().unwrap().messages.len(), 2);
    let binding = store
        .get_room_session_binding(room.id, pay.id)
        .unwrap()
        .unwrap();
    assert_eq!(binding.generation, 2);
    assert_eq!(binding.status, SessionBindingStatus::Active);
    assert_eq!(binding.id, recovered.session().binding_id);
    assert!(
        observed.lock().unwrap().messages[1]
            .content
            .contains("Recovered shared Room context")
    );
    assert!(
        workspace
            .activate_room_message(next.id, pay.id, NOW.into())
            .await
            .unwrap()
            .is_none()
    );
    workspace.shutdown(NOW.into()).await.unwrap();
}

#[tokio::test]
async fn two_rooms_share_one_owner_but_private_traffic_cannot_block_or_cross_sessions() {
    let database = TestDatabase::new();
    let (pay, _, room, first) = seed(&database);
    let mut store = SqliteStore::open(database.path()).unwrap();
    let other = Room {
        id: RoomId::new(),
        name: "other".into(),
        ..room
    };
    store.create_room(&other).unwrap();
    store.add_room_member(other.id, pay.id, None, NOW).unwrap();
    let second = RoomMessage {
        id: RoomMessageId::new(),
        room_id: other.id,
        ..first.clone()
    };
    store.append_room_message(&second).unwrap();
    let (transport, events, observed) = FakeTransport::new();
    let mut workspace =
        WorkspaceRuntime::new(StorageWorker::open(database.path()).unwrap()).unwrap();
    workspace
        .register_agent(
            AgentConnection {
                agent_id: pay.id,
                project_root: pay.project_root.into(),
            },
            transport,
        )
        .await
        .unwrap();
    let mut a = workspace
        .activate_room_message(first.id, pay.id, NOW.into())
        .await
        .unwrap()
        .unwrap();
    let mut b = workspace
        .activate_room_message(second.id, pay.id, NOW.into())
        .await
        .unwrap()
        .unwrap();
    assert_ne!(a.session(), b.session());
    for _ in 0..100 {
        events
            .send(TransportEvent::AgentTextDelta {
                session: a.session().clone(),
                text: "private".into(),
            })
            .await
            .unwrap();
        events
            .send(TransportEvent::ToolCallFinished {
                session: a.session().clone(),
                tool_call_id: "private".into(),
            })
            .await
            .unwrap();
    }
    events
        .send(TransportEvent::TurnCompleted {
            session: b.session().clone(),
        })
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(2), b.next_event(NOW.into()))
            .await
            .unwrap()
            .unwrap(),
        Some(RoomRuntimeEvent::Completed)
    );
    events
        .send(TransportEvent::TurnCompleted {
            session: a.session().clone(),
        })
        .await
        .unwrap();
    assert_eq!(
        a.next_event(NOW.into()).await.unwrap(),
        Some(RoomRuntimeEvent::Completed)
    );
    assert_eq!(observed.lock().unwrap().connects, 1);
    assert_eq!(observed.lock().unwrap().subscribes, 1);
    assert_eq!(observed.lock().unwrap().creates.len(), 2);
    workspace.shutdown(NOW.into()).await.unwrap();
}

#[tokio::test]
async fn interrupted_room_activation_is_reconciled_without_resending_old_message() {
    let database = TestDatabase::new();
    let (pay, _, _, first) = seed(&database);
    let (transport, _events, _) = FakeTransport::new();
    let mut workspace =
        WorkspaceRuntime::new(StorageWorker::open(database.path()).unwrap()).unwrap();
    workspace
        .register_agent(
            AgentConnection {
                agent_id: pay.id,
                project_root: pay.project_root.clone().into(),
            },
            transport,
        )
        .await
        .unwrap();
    let _active = workspace
        .activate_room_message(first.id, pay.id, NOW.into())
        .await
        .unwrap()
        .unwrap();
    workspace.shutdown(NOW.into()).await.unwrap();
    let mut second = first.clone();
    second.id = RoomMessageId::new();
    SqliteStore::open(database.path())
        .unwrap()
        .append_room_message(&second)
        .unwrap();
    let (transport, _events2, observed) = FakeTransport::new();
    let mut restarted =
        WorkspaceRuntime::new(StorageWorker::open(database.path()).unwrap()).unwrap();
    restarted
        .register_agent(
            AgentConnection {
                agent_id: pay.id,
                project_root: pay.project_root.into(),
            },
            transport,
        )
        .await
        .unwrap();
    let recovered = restarted
        .activate_room_message(second.id, pay.id, NOW.into())
        .await
        .unwrap()
        .unwrap();
    assert!(observed.lock().unwrap().resumes.is_empty());
    assert_eq!(observed.lock().unwrap().creates.len(), 1);
    assert_eq!(observed.lock().unwrap().messages.len(), 1);
    assert!(
        restarted
            .activate_room_message(first.id, pay.id, NOW.into())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        SqliteStore::open(database.path())
            .unwrap()
            .get_room_session_binding(second.room_id, pay.id)
            .unwrap()
            .unwrap()
            .generation,
        2
    );
    drop(recovered);
    restarted.shutdown(NOW.into()).await.unwrap();
}

#[tokio::test]
async fn cancelled_activation_caller_cannot_orphan_an_accepted_turn() {
    let database = TestDatabase::new();
    let (pay, _, room, message) = seed(&database);
    let (transport, _events, observed) = FakeTransport::new();
    let gate = Arc::new(tokio::sync::Notify::new());
    observed.lock().unwrap().create_gate = Some(gate.clone());
    let workspace =
        Arc::new(WorkspaceRuntime::new(StorageWorker::open(database.path()).unwrap()).unwrap());
    workspace
        .register_agent(
            AgentConnection {
                agent_id: pay.id,
                project_root: pay.project_root.into(),
            },
            transport,
        )
        .await
        .unwrap();
    let activation = {
        let workspace = workspace.clone();
        tokio::spawn(async move {
            workspace
                .activate_room_message(message.id, pay.id, NOW.into())
                .await
        })
    };
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while observed.lock().unwrap().creates.is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    activation.abort();
    assert!(activation.await.is_err());
    gate.notify_one();
    let store = SqliteStore::open(database.path()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if observed.lock().unwrap().cancels > 0
                && store
                    .get_room_session_binding(room.id, pay.id)
                    .unwrap()
                    .is_some_and(|binding| binding.status == SessionBindingStatus::Lost)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("abandoned activation must cancel and quarantine the session");
    let mut workspace = match Arc::try_unwrap(workspace) {
        Ok(workspace) => workspace,
        Err(_) => panic!("activation caller still owns workspace"),
    };
    workspace.shutdown(NOW.into()).await.unwrap();
}

#[tokio::test]
async fn room_terminal_failures_and_disconnects_close_activation_without_shared_output() {
    for mode in ["failure", "lost", "disconnect"] {
        let database = TestDatabase::new();
        let (pay, _, room, message) = seed(&database);
        let (transport, events, _) = FakeTransport::new();
        let mut workspace =
            WorkspaceRuntime::new(StorageWorker::open(database.path()).unwrap()).unwrap();
        workspace
            .register_agent(
                AgentConnection {
                    agent_id: pay.id,
                    project_root: pay.project_root.into(),
                },
                transport,
            )
            .await
            .unwrap();
        let mut active = workspace
            .activate_room_message(message.id, pay.id, NOW.into())
            .await
            .unwrap()
            .unwrap();
        let event = match mode {
            "failure" => TransportEvent::TurnFailed {
                session: active.session().clone(),
                failure: july_workspace::transport::TransportFailureKind::Protocol,
            },
            "lost" => TransportEvent::SessionLost {
                session: active.session().clone(),
            },
            _ => TransportEvent::TransportDisconnected {
                agent_id: pay.id,
                reason: "closed".into(),
            },
        };
        events.send(event).await.unwrap();
        let result = active.next_event(NOW.into()).await;
        if mode == "failure" {
            assert!(matches!(result, Ok(Some(RoomRuntimeEvent::Failed(_)))));
        } else {
            assert!(result.is_err());
        }
        let store = SqliteStore::open(database.path()).unwrap();
        assert_eq!(
            store
                .get_room_session_binding(room.id, pay.id)
                .unwrap()
                .unwrap()
                .status,
            if mode == "failure" {
                SessionBindingStatus::Disconnected
            } else {
                SessionBindingStatus::Lost
            }
        );
        assert_eq!(
            store.list_recent_room_messages(room.id, 50).unwrap().0,
            vec![message]
        );
        let connection = rusqlite::Connection::open(database.path()).unwrap();
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM agent_room_cursors", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row("SELECT status FROM room_message_activations", [], |row| row
                    .get::<_, String>(0))
                .unwrap(),
            "failed"
        );
        workspace.shutdown(NOW.into()).await.unwrap();
    }
}

#[tokio::test]
async fn real_acp_room_activation_handles_permission_and_keeps_reply_private() {
    use july_workspace::transport::{AcpAgentConfig, AcpTransport};
    let database = TestDatabase::new();
    let (mut pay, _, room, message) = seed(&database);
    pay.project_root = database.directory.to_string_lossy().into_owned();
    SqliteStore::open(database.path())
        .unwrap()
        .update_agent(&pay)
        .unwrap();
    let config = AcpAgentConfig {
        executable: "/usr/bin/python3".into(),
        arguments: vec![
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/acp_agent.py")
                .to_string_lossy()
                .into_owned(),
        ],
        environment: Default::default(),
        state_directory: database.directory.clone(),
        expected_agent_name: "test-acp-agent".into(),
        expected_agent_version: "1.0.0".into(),
    };
    let mut workspace =
        WorkspaceRuntime::new(StorageWorker::open(database.path()).unwrap()).unwrap();
    workspace
        .register_agent(
            AgentConnection {
                agent_id: pay.id,
                project_root: pay.project_root.into(),
            },
            AcpTransport::new(config),
        )
        .await
        .unwrap();
    let mut active = workspace
        .activate_room_message(message.id, pay.id, NOW.into())
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match active.next_event(NOW.into()).await.unwrap().unwrap() {
                RoomRuntimeEvent::PermissionRequested(request) => active
                    .respond_permission(
                        request.request_id,
                        PermissionOutcome::Selected(request.options[0].id.clone()),
                        NOW.into(),
                    )
                    .await
                    .unwrap(),
                RoomRuntimeEvent::Completed => break,
                event => panic!("unexpected Room event: {event:?}"),
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(
        SqliteStore::open(database.path())
            .unwrap()
            .list_recent_room_messages(room.id, 50)
            .unwrap()
            .0,
        vec![message]
    );
    workspace.shutdown(NOW.into()).await.unwrap();
}

#[tokio::test]
async fn terminal_poll_cancellation_preserves_completion_and_finishes_detach() {
    let database = TestDatabase::new();
    let (pay, _, room, message) = seed(&database);
    let (transport, events, _) = FakeTransport::new();
    let mut workspace =
        WorkspaceRuntime::new(StorageWorker::open(database.path()).unwrap()).unwrap();
    workspace
        .register_agent(
            AgentConnection {
                agent_id: pay.id,
                project_root: pay.project_root.into(),
            },
            transport,
        )
        .await
        .unwrap();
    let mut active = workspace
        .activate_room_message(message.id, pay.id, NOW.into())
        .await
        .unwrap()
        .unwrap();
    let blocker = rusqlite::Connection::open(database.path()).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    events
        .send(TransportEvent::TurnCompleted {
            session: active.session().clone(),
        })
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            active.next_event(NOW.into())
        )
        .await
        .is_err()
    );
    blocker.execute_batch("ROLLBACK").unwrap();
    assert_eq!(
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            active.next_event(NOW.into())
        )
        .await
        .expect("terminal event survives cancelled polling")
        .unwrap(),
        Some(RoomRuntimeEvent::Completed)
    );
    assert_eq!(
        SqliteStore::open(database.path())
            .unwrap()
            .get_room_session_binding(room.id, pay.id)
            .unwrap()
            .unwrap()
            .status,
        SessionBindingStatus::Disconnected
    );
    workspace.shutdown(NOW.into()).await.unwrap();
}

#[tokio::test]
async fn room_cursor_delivers_bounded_context_and_advances_only_after_completion() {
    let database = TestDatabase::new();
    let (pay, _, room, mut message) = seed(&database);
    let mut store = SqliteStore::open(database.path()).unwrap();
    for index in 0..55 {
        message.id = RoomMessageId::new();
        message.body = format!("shared-context-{index:02}");
        message.created_at = format!("2000-{index:02}"); // Deliberately unrelated to insertion order.
        store.append_room_message(&message).unwrap();
    }
    message.id = RoomMessageId::new();
    message.body = "current-trigger".into();
    store.append_room_message(&message).unwrap();
    let (transport, events, observed) = FakeTransport::new();
    let mut workspace =
        WorkspaceRuntime::new(StorageWorker::open(database.path()).unwrap()).unwrap();
    workspace
        .register_agent(
            AgentConnection {
                agent_id: pay.id,
                project_root: pay.project_root.clone().into(),
            },
            transport,
        )
        .await
        .unwrap();
    let mut active = workspace
        .activate_room_message(message.id, pay.id, NOW.into())
        .await
        .unwrap()
        .unwrap();
    let content = observed.lock().unwrap().messages[0].content.clone();
    assert!(content.contains("shared-context-05"), "{content}");
    assert!(content.contains("send_room_message"), "{content}");
    assert!(content.contains("targets=[]"), "{content}");
    assert!(
        content.contains("Private runtime output is not published"),
        "{content}"
    );
    assert!(!content.contains("shared-context-04"));
    assert!(content.contains("omitted"));
    assert_eq!(content.matches("current-trigger").count(), 1);
    let connection = rusqlite::Connection::open(database.path()).unwrap();
    let cursor_count = || {
        connection
            .query_row("SELECT count(*) FROM agent_room_cursors", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap()
    };
    assert_eq!(cursor_count(), 0);
    events
        .send(TransportEvent::TurnCompleted {
            session: active.session().clone(),
        })
        .await
        .unwrap();
    assert_eq!(
        active.next_event(NOW.into()).await.unwrap(),
        Some(RoomRuntimeEvent::Completed)
    );
    assert_eq!(cursor_count(), 1);
    workspace.shutdown(NOW.into()).await.unwrap();
    let (transport, events, observed) = FakeTransport::new();
    let mut workspace =
        WorkspaceRuntime::new(StorageWorker::open(database.path()).unwrap()).unwrap();
    workspace
        .register_agent(
            AgentConnection {
                agent_id: pay.id,
                project_root: pay.project_root.clone().into(),
            },
            transport,
        )
        .await
        .unwrap();
    message.id = RoomMessageId::new();
    message.body = "incremental-only".into();
    store.append_room_message(&message).unwrap();
    let mut active = workspace
        .activate_room_message(message.id, pay.id, NOW.into())
        .await
        .unwrap()
        .unwrap();
    let content = observed.lock().unwrap().messages[0].content.clone();
    assert!(content.contains("incremental-only"));
    assert!(!content.contains("shared-context"));
    assert!(!content.contains("current-trigger"));
    active.cancel(NOW.into()).await.unwrap();
    events
        .send(TransportEvent::TurnCompleted {
            session: active.session().clone(),
        })
        .await
        .unwrap();
    assert_eq!(
        active.next_event(NOW.into()).await.unwrap(),
        Some(RoomRuntimeEvent::Cancelled)
    );
    let seen: String = connection
        .query_row(
            "SELECT last_seen_message_id FROM agent_room_cursors WHERE room_id = ?1",
            [room.id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_ne!(seen, message.id.to_string());
    workspace.shutdown(NOW.into()).await.unwrap();
}

#[tokio::test]
async fn room_cursor_is_scoped_and_cannot_regress_or_include_future_messages() {
    let database = TestDatabase::new();
    let (pay, infra, room, first) = seed(&database);
    let mut store = SqliteStore::open(database.path()).unwrap();
    let mut newer = first.clone();
    newer.id = RoomMessageId::new();
    newer.body = "newer-trigger".into();
    newer.mentions = vec![pay.id, infra.id];
    store.append_room_message(&newer).unwrap();
    let mut future = newer.clone();
    future.id = RoomMessageId::new();
    future.body = "future-must-not-leak".into();
    store.append_room_message(&future).unwrap();
    let (transport, events, observed) = FakeTransport::new();
    let mut workspace =
        WorkspaceRuntime::new(StorageWorker::open(database.path()).unwrap()).unwrap();
    workspace
        .register_agent(
            AgentConnection {
                agent_id: pay.id,
                project_root: pay.project_root.clone().into(),
            },
            transport,
        )
        .await
        .unwrap();
    let mut active = workspace
        .activate_room_message(newer.id, pay.id, NOW.into())
        .await
        .unwrap()
        .unwrap();
    let content = observed.lock().unwrap().messages[0].content.clone();
    assert!(content.contains(&first.body));
    assert!(!content.contains(&future.body));
    events
        .send(TransportEvent::TurnCompleted {
            session: active.session().clone(),
        })
        .await
        .unwrap();
    active.next_event(NOW.into()).await.unwrap();
    // A legitimate older activation still carries its current message, but cannot rewind.
    let mut active = workspace
        .activate_room_message(first.id, pay.id, NOW.into())
        .await
        .unwrap()
        .unwrap();
    events
        .send(TransportEvent::TurnCompleted {
            session: active.session().clone(),
        })
        .await
        .unwrap();
    active.next_event(NOW.into()).await.unwrap();
    let connection = rusqlite::Connection::open(database.path()).unwrap();
    connection.execute_batch("VACUUM").unwrap();
    let seen: String = connection.query_row("SELECT last_seen_message_id FROM agent_room_cursors WHERE agent_id = ?1 AND room_id = ?2", [pay.id.to_string(), room.id.to_string()], |row| row.get(0)).unwrap();
    assert_eq!(seen, newer.id.to_string());
    let (transport, infra_events, infra_observed) = FakeTransport::new();
    workspace
        .register_agent(
            AgentConnection {
                agent_id: infra.id,
                project_root: infra.project_root.clone().into(),
            },
            transport,
        )
        .await
        .unwrap();
    let mut active = workspace
        .activate_room_message(newer.id, infra.id, NOW.into())
        .await
        .unwrap()
        .unwrap();
    assert!(
        infra_observed.lock().unwrap().messages[0]
            .content
            .contains(&first.body)
    );
    infra_events
        .send(TransportEvent::TurnCompleted {
            session: active.session().clone(),
        })
        .await
        .unwrap();
    active.next_event(NOW.into()).await.unwrap();
    // The same agent in another Room starts independently.
    let mut other = room.clone();
    other.id = RoomId::new();
    other.name = "Other".into();
    store.create_room(&other).unwrap();
    store.add_room_member(other.id, pay.id, None, NOW).unwrap();
    let mut other_message = first.clone();
    other_message.room_id = other.id;
    other_message.id = RoomMessageId::new();
    other_message.body = "other-room-history".into();
    store.append_room_message(&other_message).unwrap();
    other_message.id = RoomMessageId::new();
    other_message.body = "other-room-current".into();
    store.append_room_message(&other_message).unwrap();
    let mut active = workspace
        .activate_room_message(other_message.id, pay.id, NOW.into())
        .await
        .unwrap()
        .unwrap();
    let content = observed
        .lock()
        .unwrap()
        .messages
        .last()
        .unwrap()
        .content
        .clone();
    assert!(content.contains("other-room-history"));
    assert!(!content.contains(&newer.body));
    events
        .send(TransportEvent::TurnCompleted {
            session: active.session().clone(),
        })
        .await
        .unwrap();
    active.next_event(NOW.into()).await.unwrap();
    workspace.shutdown(NOW.into()).await.unwrap();
}

#[test]
fn room_cursor_migration_preserves_existing_messages_and_stable_append_order() {
    let database = TestDatabase::new();
    let (pay, _, room, first) = seed(&database);
    let connection = rusqlite::Connection::open(database.path()).unwrap();
    let binding = july_workspace::domain::SessionBindingId::new().to_string();
    connection.execute("INSERT INTO session_bindings(id, room_id, agent_id, transport_type, status, created_at, last_used_at) VALUES (?1, ?2, ?3, 'acp', 'disconnected', ?4, ?4)", rusqlite::params![binding, room.id.to_string(), pay.id.to_string(), NOW]).unwrap();
    connection.execute("INSERT INTO room_message_activations(message_id, agent_id, session_binding_id, status, updated_at) VALUES (?1, ?2, ?3, 'failed', ?4)", rusqlite::params![first.id.to_string(), pay.id.to_string(), binding, NOW]).unwrap();
    // Reconstruct the previous schema with its existing canonical message intact.
    connection.execute_batch("DROP TABLE room_message_work; DROP TABLE room_a2a_task_bindings; DROP TRIGGER room_task_work_scope; DROP TABLE room_message_publications; DROP TRIGGER room_message_append_order; DROP TABLE agent_room_cursors; DROP TABLE room_message_order; DELETE FROM schema_migrations WHERE version >= 18;").unwrap();
    drop(connection);
    let mut store = SqliteStore::open(database.path()).unwrap();
    assert_eq!(store.schema_version().unwrap(), 20);
    assert_eq!(
        store.list_recent_room_messages(room.id, 10).unwrap().0,
        vec![first.clone()]
    );
    let mut second = first.clone();
    second.id = RoomMessageId::new();
    second.created_at = "1900-01-01".into();
    second.body = "new but backdated".into();
    store.append_room_message(&second).unwrap();
    store.append_room_message(&second).unwrap(); // Replay must not allocate another ordinal.
    let connection = rusqlite::Connection::open(database.path()).unwrap();
    connection.execute_batch("VACUUM").unwrap();
    let ordered = connection
        .prepare("SELECT message_id FROM room_message_order ORDER BY sequence")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(ordered, vec![first.id.to_string(), second.id.to_string()]);
    assert!(store.get_agent(pay.id).unwrap().is_some());
    assert_eq!(
        store
            .get_room_session_binding(room.id, pay.id)
            .unwrap()
            .unwrap()
            .id
            .to_string(),
        binding
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT status FROM room_message_activations WHERE message_id = ?1",
                [first.id.to_string()],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "failed"
    );
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn recreated_session_receives_bounded_shared_context_and_relevant_work_without_moving_cursor()
{
    use july_workspace::domain::{WorkItem, WorkItemId, WorkScope, WorkStatus};
    let database = TestDatabase::new();
    let (pay, infra, room, first) = seed(&database);
    let mut store = SqliteStore::open(database.path()).unwrap();
    let mut last = first.clone();
    for index in 0..60 {
        last = RoomMessage {
            id: RoomMessageId::new(),
            body: format!("shared-history-{index:02}"),
            ..first.clone()
        };
        store.append_room_message(&last).unwrap();
    }
    for index in 0_u128..26 {
        let work = WorkItem {
            id: WorkItemId::from(ulid::Ulid::from(index + 1)),
            scope: WorkScope::Room(room.id),
            title: format!("pending-work-{index:02}"),
            goal: Some("preserve existing progress".into()),
            status: WorkStatus::Open,
            owner_agent_id: None,
            is_primary: false,
            created_at: NOW.into(),
            updated_at: NOW.into(),
            completed_at: None,
        };
        store.insert_work_item(&work).unwrap();
        store
            .assign_work_owner(work.id, if index == 25 { infra.id } else { pay.id }, NOW)
            .unwrap();
    }
    let (transport, events, observed) = FakeTransport::new();
    let mut workspace =
        WorkspaceRuntime::new(StorageWorker::open(database.path()).unwrap()).unwrap();
    workspace
        .register_agent(
            AgentConnection {
                agent_id: pay.id,
                project_root: pay.project_root.into(),
            },
            transport,
        )
        .await
        .unwrap();
    let mut active = workspace
        .activate_room_message(last.id, pay.id, NOW.into())
        .await
        .unwrap()
        .unwrap();
    events
        .send(TransportEvent::TurnCompleted {
            session: active.session().clone(),
        })
        .await
        .unwrap();
    active.next_event(NOW.into()).await.unwrap();
    let connection = rusqlite::Connection::open(database.path()).unwrap();
    let cursor: String = connection
        .query_row(
            "SELECT last_seen_message_id FROM agent_room_cursors WHERE agent_id=?1",
            [pay.id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    let next = RoomMessage {
        id: RoomMessageId::new(),
        body: "new explicit reconciliation request".into(),
        ..first.clone()
    };
    store.append_room_message(&next).unwrap();
    let future = RoomMessage {
        id: RoomMessageId::new(),
        body: "future-shared-message".into(),
        ..first
    };
    store.append_room_message(&future).unwrap();
    observed.lock().unwrap().lose_resume = true;
    let recovered = workspace
        .activate_room_message(next.id, pay.id, NOW.into())
        .await
        .unwrap()
        .unwrap();
    let prompt = observed
        .lock()
        .unwrap()
        .messages
        .last()
        .unwrap()
        .content
        .clone();
    assert_eq!(prompt.matches("shared-history-").count(), 50);
    assert!(prompt.contains("shared-history-10"));
    assert!(prompt.contains("shared-history-59"));
    assert!(!prompt.contains("shared-history-09"));
    assert!(!prompt.contains("future-shared-message"));
    assert_eq!(prompt.matches("Unfinished Work:").count(), 20);
    assert!(prompt.contains("pending-work-24"));
    assert!(!prompt.contains("pending-work-04"));
    assert!(!prompt.contains("pending-work-25"));
    assert!(prompt.contains("do not repeat interrupted actions"));
    assert_eq!(
        connection
            .query_row(
                "SELECT last_seen_message_id FROM agent_room_cursors WHERE agent_id=?1",
                [pay.id.to_string()],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        cursor
    );
    assert_eq!(observed.lock().unwrap().messages.len(), 2);
    drop(recovered);
    workspace.shutdown(NOW.into()).await.unwrap();
}

#[tokio::test]
async fn uncertain_resume_error_does_not_create_replacement_or_send_new_prompt() {
    for close_during_resume in [false, true] {
        let database = TestDatabase::new();
        let (pay, _, room, first) = seed(&database);
        let (transport, events, observed) = FakeTransport::new();
        let mut workspace =
            WorkspaceRuntime::new(StorageWorker::open(database.path()).unwrap()).unwrap();
        workspace
            .register_agent(
                AgentConnection {
                    agent_id: pay.id,
                    project_root: pay.project_root.into(),
                },
                transport,
            )
            .await
            .unwrap();
        let mut active = workspace
            .activate_room_message(first.id, pay.id, NOW.into())
            .await
            .unwrap()
            .unwrap();
        events
            .send(TransportEvent::TurnCompleted {
                session: active.session().clone(),
            })
            .await
            .unwrap();
        active.next_event(NOW.into()).await.unwrap();
        observed.lock().unwrap().fail_resume = !close_during_resume;
        if close_during_resume {
            observed.lock().unwrap().close_on_resume = Some(database.path().to_path_buf());
        }
        let next = RoomMessage {
            id: RoomMessageId::new(),
            ..first
        };
        let mut store = SqliteStore::open(database.path()).unwrap();
        store.append_room_message(&next).unwrap();
        assert!(
            workspace
                .activate_room_message(next.id, pay.id, NOW.into())
                .await
                .is_err()
        );
        assert_eq!(observed.lock().unwrap().creates.len(), 1);
        assert_eq!(observed.lock().unwrap().messages.len(), 1);
        assert_eq!(
            store
                .get_room_session_binding(room.id, pay.id)
                .unwrap()
                .unwrap()
                .generation,
            1
        );
        assert!(
            workspace
                .activate_room_message(next.id, pay.id, NOW.into())
                .await
                .unwrap()
                .is_none()
        );
        if close_during_resume {
            assert_eq!(
                store
                    .get_room_session_binding(room.id, pay.id)
                    .unwrap()
                    .unwrap()
                    .status,
                SessionBindingStatus::Closed
            );
        }
        workspace.shutdown(NOW.into()).await.unwrap();
    }
}
