use july_workspace::domain::{
    Agent, AgentId, MemberType, Room, RoomId, RoomMessage, RoomMessageId,
};
use july_workspace::runtime::{RoomRuntimeEvent, StorageWorker, WorkspaceRuntime};
use july_workspace::storage::SqliteStore;
use july_workspace::transport::{
    AgentConnection, AgentTransport, CreateSession, PermissionResponse, ResumeSession, SendMessage,
    SessionCreated, SessionRef, SessionResumed, TransportError, TransportEvent, TransportEvents,
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
    creates: Vec<CreateSession>,
    messages: Vec<SendMessage>,
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
        sender_type: MemberType::Agent,
        sender_id: infra.id.to_string(),
        body: "@pay check refund".into(),
        mentions: vec![pay.id],
        reply_to: None,
        created_at: NOW.into(),
    };
    store.append_room_message(&message).unwrap();
    (pay, infra, room, message)
}

#[tokio::test]
async fn a2a_delivery_validates_before_acp_and_uses_existing_once_only_activation() {
    let database = TestDatabase::new();
    let (pay, _, room, message) = seed(&database);
    let storage = StorageWorker::open(database.path()).unwrap();
    let wire = storage
        .prepare_room_a2a_message(message.id, pay.id)
        .await
        .unwrap();
    let (transport, events, observed) = FakeTransport::new();
    let mut workspace = WorkspaceRuntime::new(storage).unwrap();
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
    let mut forged = wire.clone();
    forged["parts"][0]["text"] = json!("forged body");
    assert!(
        workspace
            .receive_room_a2a_message(pay.id, &forged, NOW.into())
            .await
            .is_err()
    );
    assert!(observed.lock().unwrap().creates.is_empty());
    assert!(observed.lock().unwrap().messages.is_empty());
    assert!(
        SqliteStore::open(database.path())
            .unwrap()
            .get_room_session_binding(room.id, pay.id)
            .unwrap()
            .is_none()
    );
    let mut active = workspace
        .receive_room_a2a_message(pay.id, &wire, NOW.into())
        .await
        .unwrap()
        .unwrap();
    assert!(
        workspace
            .receive_room_a2a_message(pay.id, &wire, NOW.into())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(observed.lock().unwrap().creates.len(), 1);
    assert_eq!(observed.lock().unwrap().messages.len(), 1);
    assert!(
        observed.lock().unwrap().messages[0]
            .content
            .contains(&message.body)
    );
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
    assert!(
        workspace
            .receive_room_a2a_message(pay.id, &wire, NOW.into())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(observed.lock().unwrap().messages.len(), 1);
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
async fn a2a_delivery_rechecks_sender_and_recipient_after_preparation() {
    for remove_sender in [false, true] {
        let database = TestDatabase::new();
        let (pay, infra, room, message) = seed(&database);
        let storage = StorageWorker::open(database.path()).unwrap();
        let wire = storage
            .prepare_room_a2a_message(message.id, pay.id)
            .await
            .unwrap();
        let (transport, _events, observed) = FakeTransport::new();
        let mut workspace = WorkspaceRuntime::new(storage).unwrap();
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
        store
            .remove_room_member(room.id, if remove_sender { infra.id } else { pay.id }, NOW)
            .unwrap();
        assert!(
            workspace
                .receive_room_a2a_message(pay.id, &wire, NOW.into())
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
}
