use july_workspace::domain::{
    Agent, AgentId, Checkpoint, Conversation, ConversationId, ConversationKind, ConversationMember,
    DependencyType, DomainError, MemberType, Memory, MemoryKind, MemoryScopeType, Message,
    MessageId, PermissionDecision, PermissionOption, PermissionOutcome, Publish, Room, RoomMember,
    SessionBinding, SessionBindingStatus, WorkDependency, WorkItem, WorkResult, WorkStatus,
};
use july_workspace::storage::{SqliteStore, StoreError};
use serde_json::{Value, json};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use ulid::Ulid;

const CREATED: &str = "2026-08-09T10:00:00Z";
const LATER: &str = "2026-08-09T11:00:00Z";

struct TestDatabase {
    directory: PathBuf,
    path: PathBuf,
}

impl TestDatabase {
    fn new() -> Self {
        let directory = env::temp_dir().join(format!("july-core-sqlite-test-{}", Ulid::generate()));
        fs::create_dir(&directory).expect("create test database directory");
        let path = directory.join("workspace.db");
        assert!(!path.starts_with(env!("CARGO_MANIFEST_DIR")));
        Self { directory, path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.directory)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("failed to clean up {}: {error}", self.directory.display());
        }
    }
}

fn agent(name: &str) -> Agent {
    Agent {
        id: AgentId::new(),
        name: name.into(),
        project_root: format!("/workspace/{name}"),
        transport_type: "local".into(),
        transport_config: json!({"command": ["codex", "exec"], "retries": 2}),
        status: "active".into(),
        metadata: json!({"labels": ["rust", "storage"], "enabled": true}),
        created_at: CREATED.into(),
        updated_at: CREATED.into(),
    }
}

fn room(name: &str) -> Room {
    Room {
        id: Default::default(),
        name: name.into(),
        description: Some(format!("{name} room")),
        status: "active".into(),
        created_at: CREATED.into(),
        updated_at: CREATED.into(),
    }
}

fn dm_conversation() -> Conversation {
    Conversation {
        id: ConversationId::new(),
        kind: ConversationKind::Dm,
        room_id: None,
        title: None,
        goal: Some("Direct coordination".into()),
        parent_conversation_id: None,
        origin_conversation_id: None,
        status: "open".into(),
        created_at: CREATED.into(),
        updated_at: CREATED.into(),
    }
}

#[test]
fn full_graph_round_trips_after_reopen() {
    let database = TestDatabase::new();
    let worker = agent("worker-one");
    let operations = room("operations");
    let room_member = RoomMember {
        room_id: operations.id,
        agent_id: worker.id,
        role: Some("owner".into()),
        generation: 1,
        joined_at: CREATED.into(),
        left_at: None,
    };
    let direct = dm_conversation();
    let thread = Conversation {
        id: ConversationId::new(),
        kind: ConversationKind::Thread,
        room_id: Some(operations.id),
        title: Some("Storage rollout".into()),
        goal: Some("Persist the whole graph".into()),
        parent_conversation_id: Some(direct.id),
        origin_conversation_id: Some(direct.id),
        status: "open".into(),
        created_at: CREATED.into(),
        updated_at: CREATED.into(),
    };
    let conversation_member = ConversationMember {
        conversation_id: thread.id,
        member_type: MemberType::Agent,
        member_id: worker.id.to_string(),
        generation: 1,
        joined_at: CREATED.into(),
        left_at: Some(LATER.into()),
    };
    let null_message = Message {
        id: MessageId::from(Ulid::from(10_u128)),
        conversation_id: thread.id,
        sender_type: MemberType::User,
        sender_id: "tony".into(),
        body: "Start the rollout".into(),
        reply_to: None,
        metadata: Value::Null,
        created_at: CREATED.into(),
    };
    let json_message = Message {
        id: MessageId::from(Ulid::from(11_u128)),
        conversation_id: thread.id,
        sender_type: MemberType::Agent,
        sender_id: worker.id.to_string(),
        body: "Persistence is ready".into(),
        reply_to: Some(null_message.id),
        metadata: json!({"nested": {"ok": true}, "items": [1, "two", null]}),
        created_at: LATER.into(),
    };
    let upstream = WorkItem {
        id: Default::default(),
        conversation_id: thread.id,
        title: "Storage rollout".into(),
        goal: Some("Persist the whole graph".into()),
        status: WorkStatus::Open,
        owner_agent_id: None,
        is_primary: true,
        created_at: CREATED.into(),
        updated_at: CREATED.into(),
        completed_at: None,
    };
    let downstream = WorkItem {
        id: Default::default(),
        conversation_id: thread.id,
        title: "Review storage".into(),
        goal: None,
        status: WorkStatus::Done,
        owner_agent_id: None,
        is_primary: false,
        created_at: CREATED.into(),
        updated_at: LATER.into(),
        completed_at: Some(LATER.into()),
    };
    let dependency = WorkDependency {
        upstream_work_id: upstream.id,
        downstream_work_id: downstream.id,
        dependency_type: DependencyType::Requires,
        created_at: CREATED.into(),
    };
    let first_result = WorkResult {
        id: Default::default(),
        work_id: upstream.id,
        status: "reviewed-custom".into(),
        summary: "Initial result".into(),
        outputs: vec!["artifact://storage".into(), "line\nbreak".into()],
        evidence: vec!["cargo test".into(), "clippy".into()],
        supersedes_result_id: None,
        created_at: CREATED.into(),
    };
    let final_result = WorkResult {
        id: Default::default(),
        work_id: upstream.id,
        status: "accepted-custom".into(),
        summary: "Final result".into(),
        outputs: vec!["artifact://final".into()],
        evidence: vec!["focused test: pass".into()],
        supersedes_result_id: Some(first_result.id),
        created_at: LATER.into(),
    };
    let publish = Publish {
        id: Default::default(),
        result_id: final_result.id,
        source_conversation_id: thread.id,
        target_conversation_id: direct.id,
        created_at: LATER.into(),
    };
    let binding = SessionBinding {
        id: Default::default(),
        conversation_id: thread.id,
        agent_id: worker.id,
        transport_type: "local".into(),
        remote_session_id: Some("remote-123".into()),
        generation: 2,
        status: SessionBindingStatus::Active,
        created_at: CREATED.into(),
        last_used_at: LATER.into(),
    };
    let checkpoint = Checkpoint {
        id: Default::default(),
        conversation_id: thread.id,
        agent_id: worker.id,
        goal: Some("Finish persistence".into()),
        current_state: Some("green".into()),
        decisions: vec!["Use rusqlite directly".into(), "Keep API typed".into()],
        open_items: vec!["Run clippy".into()],
        references: vec!["src/storage/sqlite.rs".into()],
        last_message_id: Some(json_message.id),
        created_at: LATER.into(),
    };
    let first_memory = Memory {
        id: Default::default(),
        scope_type: MemoryScopeType::Project,
        scope_id: "july-workspace".into(),
        kind: MemoryKind::Fact,
        content: "SQLite is the durable store".into(),
        source_conversation_id: Some(thread.id),
        evidence: vec!["schema version 1".into()],
        supersedes_memory_id: None,
        created_at: CREATED.into(),
    };
    let final_memory = Memory {
        id: Default::default(),
        scope_type: MemoryScopeType::Agent,
        scope_id: worker.id.to_string(),
        kind: MemoryKind::Decision,
        content: "Use concrete persistence methods".into(),
        source_conversation_id: Some(thread.id),
        evidence: vec!["focused integration test".into(), "reopen check".into()],
        supersedes_memory_id: Some(first_memory.id),
        created_at: LATER.into(),
    };

    {
        let mut store = SqliteStore::open(database.path()).unwrap();
        store.insert_agent(&worker).unwrap();
        store.insert_room(&operations).unwrap();
        store
            .add_room_member(operations.id, worker.id, Some("owner"), CREATED)
            .unwrap();
        store.insert_conversation(&direct).unwrap();
        store
            .create_thread_with_primary_work(&thread, upstream.id, "tony", &[worker.id])
            .unwrap();
        store
            .remove_thread_member(thread.id, worker.id, LATER)
            .unwrap();
        store.insert_message(&null_message).unwrap();
        store.insert_message(&json_message).unwrap();
        store.insert_work_item(&downstream).unwrap();
        store.insert_work_dependency(&dependency).unwrap();
        store.insert_work_result(&first_result).unwrap();
        store.insert_work_result(&final_result).unwrap();
        store.insert_publish(&publish).unwrap();
        store.insert_session_binding(&binding).unwrap();
        store.insert_checkpoint(&checkpoint).unwrap();
        store.insert_memory(&first_memory).unwrap();
        store.insert_memory(&final_memory).unwrap();
    }

    let store = SqliteStore::open(database.path()).unwrap();
    assert_eq!(store.get_agent(worker.id).unwrap(), Some(worker));
    assert_eq!(store.get_room(operations.id).unwrap(), Some(operations));
    assert_eq!(
        store.list_room_members(room_member.room_id).unwrap(),
        vec![room_member]
    );
    assert_eq!(store.get_conversation(direct.id).unwrap(), Some(direct));
    assert_eq!(
        store.get_conversation(thread.id).unwrap(),
        Some(thread.clone())
    );
    assert_eq!(
        store
            .list_conversation_members(conversation_member.conversation_id)
            .unwrap()
            .into_iter()
            .filter(|member| member.member_type == MemberType::Agent)
            .collect::<Vec<_>>(),
        vec![conversation_member]
    );
    assert_eq!(
        store.get_message(null_message.id).unwrap(),
        Some(null_message.clone())
    );
    assert_eq!(
        store.get_message(json_message.id).unwrap(),
        Some(json_message.clone())
    );
    assert_eq!(
        store.list_messages(thread.id).unwrap(),
        vec![null_message, json_message]
    );
    assert_eq!(store.get_work_item(upstream.id).unwrap(), Some(upstream));
    assert_eq!(
        store.get_work_item(downstream.id).unwrap(),
        Some(downstream)
    );
    assert_eq!(
        store
            .get_work_dependency(dependency.upstream_work_id, dependency.downstream_work_id)
            .unwrap(),
        Some(dependency)
    );
    assert_eq!(
        store.get_work_result(first_result.id).unwrap(),
        Some(first_result)
    );
    assert_eq!(
        store.get_work_result(final_result.id).unwrap(),
        Some(final_result)
    );
    assert_eq!(store.get_publish(publish.id).unwrap(), Some(publish));
    assert_eq!(
        store.get_session_binding(binding.id).unwrap(),
        Some(binding)
    );
    assert_eq!(
        store.get_checkpoint(checkpoint.id).unwrap(),
        Some(checkpoint)
    );
    assert_eq!(
        store.get_memory(first_memory.id).unwrap(),
        Some(first_memory)
    );
    assert_eq!(
        store.get_memory(final_memory.id).unwrap(),
        Some(final_memory)
    );
}

#[test]
fn agent_update_persists_and_duplicate_name_is_rejected() {
    let database = TestDatabase::new();
    let mut original = agent("unique-name");
    let mut duplicate = agent("unique-name");
    let store = SqliteStore::open(database.path()).unwrap();

    store.insert_agent(&original).unwrap();
    assert!(store.insert_agent(&duplicate).is_err());

    original.name = "renamed-agent".into();
    original.project_root = "/workspace/renamed".into();
    original.transport_type = "stdio".into();
    original.transport_config = json!({"program": "codex", "args": ["exec"]});
    original.status = "paused".into();
    original.metadata = json!({"reason": "maintenance"});
    original.updated_at = LATER.into();
    assert!(store.update_agent(&original).unwrap());

    duplicate.name = " ".into();
    assert!(matches!(
        store.update_agent(&duplicate),
        Err(StoreError::Domain(DomainError::EmptyField("agent.name")))
    ));
    drop(store);

    let store = SqliteStore::open(database.path()).unwrap();
    assert_eq!(store.get_agent(original.id).unwrap(), Some(original));
}

#[test]
fn same_agent_can_join_multiple_rooms() {
    let database = TestDatabase::new();
    let mut store = SqliteStore::open(database.path()).unwrap();
    let worker = agent("multi-room-worker");
    let first_room = room("first-room");
    let second_room = room("second-room");
    let first_membership = RoomMember {
        room_id: first_room.id,
        agent_id: worker.id,
        role: None,
        generation: 1,
        joined_at: CREATED.into(),
        left_at: None,
    };
    let second_membership = RoomMember {
        room_id: second_room.id,
        agent_id: worker.id,
        role: Some("reviewer".into()),
        generation: 1,
        joined_at: LATER.into(),
        left_at: None,
    };

    store.insert_agent(&worker).unwrap();
    store.insert_room(&first_room).unwrap();
    store.insert_room(&second_room).unwrap();
    store
        .add_room_member(first_room.id, worker.id, None, CREATED)
        .unwrap();
    store
        .add_room_member(second_room.id, worker.id, Some("reviewer"), LATER)
        .unwrap();

    assert_eq!(
        store.list_room_members(first_room.id).unwrap(),
        vec![first_membership]
    );
    assert_eq!(
        store.list_room_members(second_room.id).unwrap(),
        vec![second_membership]
    );
}

#[test]
fn messages_are_ordered_by_created_at_then_id() {
    let database = TestDatabase::new();
    let store = SqliteStore::open(database.path()).unwrap();
    let conversation = dm_conversation();
    let first_id = MessageId::from(Ulid::from(1_u128));
    let second_id = MessageId::from(Ulid::from(2_u128));
    let later_id = MessageId::from(Ulid::from(3_u128));
    let first = Message {
        id: first_id,
        conversation_id: conversation.id,
        sender_type: MemberType::User,
        sender_id: "tony".into(),
        body: "first".into(),
        reply_to: None,
        metadata: Value::Null,
        created_at: CREATED.into(),
    };
    let second = Message {
        id: second_id,
        body: "second".into(),
        ..first.clone()
    };
    let later = Message {
        id: later_id,
        body: "later".into(),
        created_at: LATER.into(),
        ..first.clone()
    };

    store.insert_conversation(&conversation).unwrap();
    store.insert_message(&later).unwrap();
    store.insert_message(&second).unwrap();
    store.insert_message(&first).unwrap();

    assert_eq!(
        store.list_messages(conversation.id).unwrap(),
        vec![first, second, later]
    );
}

#[test]
fn foreign_keys_reject_missing_parent_records() {
    let database = TestDatabase::new();
    let store = SqliteStore::open(database.path()).unwrap();
    let message = Message {
        id: Default::default(),
        conversation_id: ConversationId::new(),
        sender_type: MemberType::User,
        sender_id: "tony".into(),
        body: "orphan".into(),
        reply_to: None,
        metadata: Value::Null,
        created_at: CREATED.into(),
    };

    assert!(store.insert_message(&message).is_err());
    assert_eq!(store.get_message(message.id).unwrap(), None);
}

#[test]
fn room_batch_rolls_back_parent_when_a_member_insert_fails() {
    let database = TestDatabase::new();
    let worker = agent("room-batch-worker");
    let parent = room("atomic-room");
    let members = [
        RoomMember {
            room_id: parent.id,
            agent_id: worker.id,
            role: None,
            generation: 1,
            joined_at: CREATED.into(),
            left_at: None,
        },
        RoomMember {
            room_id: parent.id,
            agent_id: AgentId::new(),
            role: None,
            generation: 1,
            joined_at: LATER.into(),
            left_at: None,
        },
    ];
    let mut store = SqliteStore::open(database.path()).unwrap();
    store.insert_agent(&worker).unwrap();

    assert!(store.insert_room_with_members(&parent, &members).is_err());
    drop(store);

    let store = SqliteStore::open(database.path()).unwrap();
    assert_eq!(store.get_room(parent.id).unwrap(), None);
}

#[test]
fn room_batch_rejects_member_for_a_different_room_before_inserting_parent() {
    let database = TestDatabase::new();
    let worker = agent("room-mismatch-worker");
    let existing_room = room("existing-room");
    let parent = room("new-room");
    let mismatched_member = RoomMember {
        room_id: existing_room.id,
        agent_id: worker.id,
        role: None,
        generation: 1,
        joined_at: CREATED.into(),
        left_at: None,
    };
    let mut store = SqliteStore::open(database.path()).unwrap();
    store.insert_agent(&worker).unwrap();
    store.insert_room(&existing_room).unwrap();

    assert!(matches!(
        store.insert_room_with_members(&parent, &[mismatched_member]),
        Err(StoreError::RoomMemberParentMismatch { expected, found })
            if expected == parent.id && found == existing_room.id
    ));
    drop(store);

    let valid_member = RoomMember {
        room_id: parent.id,
        agent_id: worker.id,
        role: Some("owner".into()),
        generation: 1,
        joined_at: LATER.into(),
        left_at: None,
    };
    let mut store = SqliteStore::open(database.path()).unwrap();
    assert_eq!(store.get_room(parent.id).unwrap(), None);
    store
        .insert_room_with_members(&parent, std::slice::from_ref(&valid_member))
        .unwrap();
    assert_eq!(store.get_room(parent.id).unwrap(), Some(parent));
    assert_eq!(
        store.list_room_members(valid_member.room_id).unwrap(),
        vec![valid_member]
    );
}

#[test]
fn conversation_batch_rolls_back_parent_when_a_member_insert_fails() {
    let database = TestDatabase::new();
    let parent = dm_conversation();
    let duplicate = ConversationMember {
        conversation_id: parent.id,
        member_type: MemberType::User,
        member_id: "tony".into(),
        generation: 1,
        joined_at: CREATED.into(),
        left_at: None,
    };
    let members = [duplicate.clone(), duplicate];
    let mut store = SqliteStore::open(database.path()).unwrap();

    assert!(
        store
            .insert_conversation_with_members(&parent, &members)
            .is_err()
    );
    drop(store);

    let store = SqliteStore::open(database.path()).unwrap();
    assert_eq!(store.get_conversation(parent.id).unwrap(), None);
}

#[test]
fn conversation_batch_rejects_member_for_a_different_conversation_before_inserting_parent() {
    let database = TestDatabase::new();
    let parent_room = room("conversation-mismatch-room");
    let existing_conversation = dm_conversation();
    let parent = Conversation {
        id: ConversationId::new(),
        kind: ConversationKind::Thread,
        room_id: Some(parent_room.id),
        title: Some("New conversation".into()),
        goal: None,
        parent_conversation_id: None,
        origin_conversation_id: None,
        status: "open".into(),
        created_at: CREATED.into(),
        updated_at: CREATED.into(),
    };
    let mismatched_member = ConversationMember {
        conversation_id: existing_conversation.id,
        member_type: MemberType::User,
        member_id: "tony".into(),
        generation: 1,
        joined_at: CREATED.into(),
        left_at: None,
    };
    let mut store = SqliteStore::open(database.path()).unwrap();
    store.insert_room(&parent_room).unwrap();
    store.insert_conversation(&existing_conversation).unwrap();

    assert!(matches!(
        store.insert_conversation_with_members(&parent, &[mismatched_member]),
        Err(StoreError::ConversationMemberParentMismatch { expected, found })
            if expected == parent.id && found == existing_conversation.id
    ));
    drop(store);

    let valid_member = ConversationMember {
        conversation_id: parent.id,
        member_type: MemberType::User,
        member_id: "tony".into(),
        generation: 1,
        joined_at: LATER.into(),
        left_at: None,
    };
    let mut store = SqliteStore::open(database.path()).unwrap();
    assert_eq!(store.get_conversation(parent.id).unwrap(), None);
    assert!(matches!(
        store.insert_conversation_with_members(&parent, std::slice::from_ref(&valid_member)),
        Err(StoreError::ThreadAggregateRequired(id)) if id == parent.id
    ));
    assert_eq!(store.get_conversation(parent.id).unwrap(), None);
}

#[test]
fn writes_validate_domain_records_before_sql() {
    let database = TestDatabase::new();
    let store = SqliteStore::open(database.path()).unwrap();
    let mut invalid_room = room("invalid-room");
    invalid_room.name = " ".into();

    assert!(matches!(
        store.insert_room(&invalid_room),
        Err(StoreError::Domain(DomainError::EmptyField("room.name")))
    ));

    let valid_room = room("valid-room");
    store.insert_room(&valid_room).unwrap();
    let mut invalid_dm = dm_conversation();
    invalid_dm.room_id = Some(valid_room.id);
    assert!(matches!(
        store.insert_conversation(&invalid_dm),
        Err(StoreError::Domain(DomainError::DmHasRoom))
    ));
}

#[test]
fn missing_typed_id_returns_none() {
    let database = TestDatabase::new();
    let store = SqliteStore::open(database.path()).unwrap();

    assert_eq!(store.get_agent(AgentId::new()).unwrap(), None);
}

#[test]
fn session_lifecycle_and_permission_decision_round_trip() {
    let database = TestDatabase::new();
    let store = SqliteStore::open(database.path()).unwrap();
    let worker = agent("runtime-worker");
    let conversation = dm_conversation();
    let binding = SessionBinding {
        id: Default::default(),
        conversation_id: conversation.id,
        agent_id: worker.id,
        transport_type: "acp".into(),
        remote_session_id: Some("remote-1".into()),
        generation: 1,
        status: SessionBindingStatus::Active,
        created_at: CREATED.into(),
        last_used_at: CREATED.into(),
    };
    let decision = PermissionDecision {
        id: "decision-1".into(),
        session_binding_id: binding.id,
        correlation_id: "request-1".into(),
        options: vec![
            PermissionOption {
                id: "allow-once".into(),
                label: "Allow once".into(),
            },
            PermissionOption {
                id: "reject-once".into(),
                label: "Reject".into(),
            },
        ],
        outcome: PermissionOutcome::Selected("allow-once".into()),
        decided_at: LATER.into(),
    };

    store.insert_agent(&worker).unwrap();
    store.insert_conversation(&conversation).unwrap();
    store.insert_session_binding(&binding).unwrap();
    assert_eq!(
        store
            .get_current_session_binding(conversation.id, worker.id)
            .unwrap(),
        Some(binding.clone())
    );
    assert!(
        store
            .update_session_binding_status(binding.id, SessionBindingStatus::Disconnected, LATER,)
            .unwrap()
    );
    assert_eq!(
        store
            .list_current_session_bindings_for_agent(worker.id)
            .unwrap()[0]
            .status,
        SessionBindingStatus::Disconnected
    );
    store.insert_permission_decision(&decision).unwrap();
    assert_eq!(
        store.get_permission_decision(&decision.id).unwrap(),
        Some(decision)
    );
    let cancelled = PermissionDecision {
        id: "decision-2".into(),
        session_binding_id: binding.id,
        correlation_id: "request-2".into(),
        options: vec![],
        outcome: PermissionOutcome::Cancelled,
        decided_at: LATER.into(),
    };
    store.insert_permission_decision(&cancelled).unwrap();
    assert_eq!(
        store.get_permission_decision(&cancelled.id).unwrap(),
        Some(cancelled)
    );
}

#[test]
fn disconnect_marks_only_the_selected_current_binding() {
    let database = TestDatabase::new();
    let store = SqliteStore::open(database.path()).unwrap();
    let worker = agent("disconnect-worker");
    let first_conversation = dm_conversation();
    let second_conversation = dm_conversation();
    let first = SessionBinding {
        id: Default::default(),
        conversation_id: first_conversation.id,
        agent_id: worker.id,
        transport_type: "acp".into(),
        remote_session_id: Some("remote-1".into()),
        generation: 1,
        status: SessionBindingStatus::Active,
        created_at: CREATED.into(),
        last_used_at: CREATED.into(),
    };
    let second = SessionBinding {
        id: Default::default(),
        conversation_id: second_conversation.id,
        remote_session_id: Some("remote-2".into()),
        ..first.clone()
    };

    store.insert_agent(&worker).unwrap();
    store.insert_conversation(&first_conversation).unwrap();
    store.insert_conversation(&second_conversation).unwrap();
    store.insert_session_binding(&first).unwrap();
    store.insert_session_binding(&second).unwrap();

    assert!(store.mark_binding_disconnected(first.id, LATER).unwrap());
    assert_eq!(
        store.get_session_binding(first.id).unwrap().unwrap().status,
        SessionBindingStatus::Disconnected
    );
    assert_eq!(
        store
            .get_session_binding(second.id)
            .unwrap()
            .unwrap()
            .status,
        SessionBindingStatus::Active
    );
}
