use july_workspace::application::{
    AddRoomMember, AddThreadMember, CollaborationError, CollaborationService, CreateRoom,
    CreateThread, MembershipState, RemoveRoomMember, RemoveThreadMember, RoomRef,
};
use july_workspace::domain::{Agent, AgentId, ConversationId, MemberType, RoomId, WorkItemId};
use july_workspace::runtime::StorageWorker;
use july_workspace::storage::SqliteStore;
use serde_json::json;
use std::path::{Path, PathBuf};

const CREATED: &str = "2026-08-13T09:00:00Z";
const LEFT: &str = "2026-08-13T10:00:00Z";

struct TestDatabase {
    directory: PathBuf,
    path: PathBuf,
}

impl TestDatabase {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("july-phase4-app-{}", ulid::Ulid::generate()));
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

fn seed_agent(path: &Path, name: &str) -> Agent {
    let agent = Agent {
        id: AgentId::new(),
        name: name.into(),
        project_root: format!("/workspace/{name}"),
        transport_type: "acp".into(),
        transport_config: json!({}),
        status: "active".into(),
        metadata: json!({}),
        created_at: CREATED.into(),
        updated_at: CREATED.into(),
    };
    SqliteStore::open(path)
        .unwrap()
        .insert_agent(&agent)
        .unwrap();
    agent
}

fn service(path: &Path) -> CollaborationService<StorageWorker> {
    CollaborationService::new(StorageWorker::open(path).unwrap())
}

#[tokio::test]
async fn creates_and_lists_rooms_with_exact_reference_resolution() {
    let database = TestDatabase::new();
    let mut service = service(database.path());
    let room_id = RoomId::new();

    let created = service
        .create_room(CreateRoom {
            room_id,
            name: "Payments".into(),
            description: Some("Payment work".into()),
            created_at: CREATED.into(),
        })
        .await
        .unwrap();

    assert_eq!(created, room_id);
    assert_eq!(service.list_rooms().await.unwrap()[0].id, room_id);
    assert!(
        service
            .list_room_members(RoomRef::Name("Payments".into()))
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        service
            .list_room_members(RoomRef::Name("payments".into()))
            .await,
        Err(CollaborationError::RoomNotFound("payments".into()))
    );
}

#[tokio::test]
async fn room_membership_commands_return_current_state_and_changed() {
    let database = TestDatabase::new();
    let agent = seed_agent(database.path(), "Codex");
    let mut service = service(database.path());
    let room_id = service
        .create_room(CreateRoom {
            room_id: RoomId::new(),
            name: "Payments".into(),
            description: None,
            created_at: CREATED.into(),
        })
        .await
        .unwrap();

    let added = service
        .add_room_member(AddRoomMember {
            room: RoomRef::Name("Payments".into()),
            agent: july_workspace::application::AgentRef::Name("Codex".into()),
            role: Some("reviewer".into()),
            changed_at: CREATED.into(),
        })
        .await
        .unwrap();
    assert!(added.changed);
    assert_eq!(added.state, MembershipState::Active);
    let members = service
        .list_room_members(RoomRef::Id(room_id))
        .await
        .unwrap();
    let added_member = &members[0];
    assert_eq!(added_member.generation, 1);
    assert_eq!(added_member.joined_at, CREATED);

    let repeated = service
        .add_room_member(AddRoomMember {
            room: RoomRef::Id(room_id),
            agent: july_workspace::application::AgentRef::Id(agent.id),
            role: Some("owner".into()),
            changed_at: LEFT.into(),
        })
        .await
        .unwrap();
    assert!(!repeated.changed);
    assert_eq!(repeated.state, added.state);

    let removed = service
        .remove_room_member(RemoveRoomMember {
            room: RoomRef::Id(room_id),
            agent: july_workspace::application::AgentRef::Id(agent.id),
            changed_at: LEFT.into(),
        })
        .await
        .unwrap();
    assert!(removed.changed);
    assert_eq!(removed.state, MembershipState::Left);
    assert_eq!(
        service
            .list_room_members(RoomRef::Id(room_id))
            .await
            .unwrap()[0]
            .left_at
            .as_deref(),
        Some(LEFT)
    );

    let repeated = service
        .remove_room_member(RemoveRoomMember {
            room: RoomRef::Id(room_id),
            agent: july_workspace::application::AgentRef::Id(agent.id),
            changed_at: "2026-08-13T11:00:00Z".into(),
        })
        .await
        .unwrap();
    assert!(!repeated.changed);
    assert_eq!(repeated.state, removed.state);
    assert_eq!(
        service
            .list_room_members(RoomRef::Id(room_id))
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn creates_threads_atomically_and_mutates_members_by_typed_id() {
    let database = TestDatabase::new();
    let agent = seed_agent(database.path(), "Codex");
    let mut service = service(database.path());
    let room_id = service
        .create_room(CreateRoom {
            room_id: RoomId::new(),
            name: "Payments".into(),
            description: None,
            created_at: CREATED.into(),
        })
        .await
        .unwrap();
    service
        .add_room_member(AddRoomMember {
            room: RoomRef::Id(room_id),
            agent: july_workspace::application::AgentRef::Id(agent.id),
            role: None,
            changed_at: CREATED.into(),
        })
        .await
        .unwrap();
    let thread_id = ConversationId::new();
    let primary_work_id = WorkItemId::new();

    let created = service
        .create_thread(CreateThread {
            thread_id,
            primary_work_id,
            room: RoomRef::Name("Payments".into()),
            title: "Review settlement".into(),
            goal: Some("Validate settlement".into()),
            user_id: "tony".into(),
            initial_agents: vec![july_workspace::application::AgentRef::Name("Codex".into())],
            created_at: CREATED.into(),
        })
        .await
        .unwrap();

    assert_eq!(created.thread_id, thread_id);
    assert_eq!(created.primary_work_id, primary_work_id);
    assert_eq!(
        service.list_threads(RoomRef::Id(room_id)).await.unwrap()[0].id,
        thread_id
    );
    assert_eq!(
        service
            .list_thread_members(thread_id)
            .await
            .unwrap()
            .iter()
            .filter(|member| member.member_type == MemberType::Agent)
            .count(),
        1
    );

    let repeated = service
        .add_thread_member(AddThreadMember {
            thread_id,
            agent: july_workspace::application::AgentRef::Id(agent.id),
            changed_at: LEFT.into(),
        })
        .await
        .unwrap();
    assert!(!repeated.changed);
    assert_eq!(repeated.state, MembershipState::Active);
    assert!(
        service
            .list_thread_members(thread_id)
            .await
            .unwrap()
            .iter()
            .any(|member| member.member_id == agent.id.to_string() && member.joined_at == CREATED)
    );

    let removed = service
        .remove_thread_member(RemoveThreadMember {
            thread_id,
            agent: july_workspace::application::AgentRef::Id(agent.id),
            changed_at: LEFT.into(),
        })
        .await
        .unwrap();
    assert!(removed.changed);
    assert_eq!(removed.state, MembershipState::Left);
    assert!(
        service
            .list_thread_members(thread_id)
            .await
            .unwrap()
            .iter()
            .any(|member| {
                member.member_id == agent.id.to_string() && member.left_at.as_deref() == Some(LEFT)
            })
    );
}

#[tokio::test]
async fn reports_typed_reference_and_invariant_errors() {
    let database = TestDatabase::new();
    let agent = seed_agent(database.path(), "Codex");
    let mut service = service(database.path());
    let room_id = service
        .create_room(CreateRoom {
            room_id: RoomId::new(),
            name: "Payments".into(),
            description: None,
            created_at: CREATED.into(),
        })
        .await
        .unwrap();

    assert_eq!(
        service
            .add_room_member(AddRoomMember {
                room: RoomRef::Id(room_id),
                agent: july_workspace::application::AgentRef::Name("codex".into()),
                role: None,
                changed_at: CREATED.into(),
            })
            .await,
        Err(CollaborationError::AgentNotFound("codex".into()))
    );
    assert!(matches!(
        service
            .add_thread_member(AddThreadMember {
                thread_id: ConversationId::new(),
                agent: july_workspace::application::AgentRef::Id(agent.id),
                changed_at: CREATED.into(),
            })
            .await,
        Err(CollaborationError::ThreadNotFound(_))
    ));
}

#[tokio::test]
async fn removing_an_absent_member_returns_left_without_creating_history() {
    let database = TestDatabase::new();
    let agent = seed_agent(database.path(), "Codex");
    let mut service = service(database.path());
    let room_id = service
        .create_room(CreateRoom {
            room_id: RoomId::new(),
            name: "Payments".into(),
            description: None,
            created_at: CREATED.into(),
        })
        .await
        .unwrap();

    let removed = service
        .remove_room_member(RemoveRoomMember {
            room: RoomRef::Id(room_id),
            agent: july_workspace::application::AgentRef::Id(agent.id),
            changed_at: LEFT.into(),
        })
        .await
        .unwrap();

    assert_eq!(removed.state, MembershipState::Left);
    assert!(!removed.changed);
    assert!(
        service
            .list_room_members(RoomRef::Id(room_id))
            .await
            .unwrap()
            .is_empty()
    );
}

#[test]
fn collaboration_application_boundary_is_storage_and_transport_neutral() {
    let source = include_str!("../src/application/collaboration.rs");
    assert!(!source.contains("StoreError"));
    assert!(!source.contains("rusqlite"));
    assert!(!source.contains("crate::storage"));
    assert!(!source.contains("crate::transport"));
    assert!(!source.contains("agent_client_protocol"));
}
