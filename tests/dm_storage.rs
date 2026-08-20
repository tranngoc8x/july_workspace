use july_workspace::domain::{
    Agent, AgentId, Conversation, ConversationId, ConversationKind, ConversationMember,
    DomainError, MemberType, Message, MessageId, SessionBinding, SessionBindingId,
    SessionBindingStatus,
};
use july_workspace::runtime::StorageWorker;
use july_workspace::storage::{SqliteStore, StoreError};
use rusqlite::Connection;
use serde_json::json;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use ulid::Ulid;

const NOW: &str = "2026-08-11T10:00:00Z";
const LATER: &str = "2026-08-11T11:00:00Z";

struct TestDatabase {
    directory: PathBuf,
    path: PathBuf,
}

impl TestDatabase {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("july-dm-storage-test-{}", Ulid::generate()));
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

fn agent(name: &str) -> Agent {
    Agent {
        id: AgentId::new(),
        name: name.into(),
        project_root: format!("/workspace/{name}"),
        transport_type: "acp".into(),
        transport_config: json!({"command": name}),
        status: "active".into(),
        metadata: json!({}),
        created_at: NOW.into(),
        updated_at: NOW.into(),
    }
}

fn dm() -> Conversation {
    Conversation {
        id: ConversationId::new(),
        kind: ConversationKind::Dm,
        room_id: None,
        title: None,
        goal: None,
        parent_conversation_id: None,
        origin_conversation_id: None,
        status: "open".into(),
        created_at: NOW.into(),
        updated_at: NOW.into(),
    }
}

#[test]
fn get_or_create_dm_reuses_only_the_exact_active_user_agent_pair() {
    let database = TestDatabase::new();
    let mut store = SqliteStore::open(database.path()).unwrap();
    let target = agent("codex");
    let other = agent("claude");
    store.insert_agent(&target).unwrap();
    store.insert_agent(&other).unwrap();

    assert_eq!(
        store.get_agent_by_name("codex").unwrap(),
        Some(target.clone())
    );
    assert_eq!(store.get_agent_by_name("Codex").unwrap(), None);

    let wrong_types = dm();
    store
        .insert_conversation_with_members(
            &wrong_types,
            &[
                ConversationMember {
                    conversation_id: wrong_types.id,
                    member_type: MemberType::Agent,
                    member_id: other.id.to_string(),
                    generation: 1,
                    joined_at: NOW.into(),
                    left_at: None,
                },
                ConversationMember {
                    conversation_id: wrong_types.id,
                    member_type: MemberType::Agent,
                    member_id: target.id.to_string(),
                    generation: 1,
                    joined_at: NOW.into(),
                    left_at: None,
                },
            ],
        )
        .unwrap();

    let extra_member = dm();
    store
        .insert_conversation_with_members(
            &extra_member,
            &[
                ConversationMember {
                    conversation_id: extra_member.id,
                    member_type: MemberType::User,
                    member_id: other.id.to_string(),
                    generation: 1,
                    joined_at: NOW.into(),
                    left_at: None,
                },
                ConversationMember {
                    conversation_id: extra_member.id,
                    member_type: MemberType::Agent,
                    member_id: target.id.to_string(),
                    generation: 1,
                    joined_at: NOW.into(),
                    left_at: None,
                },
                ConversationMember {
                    conversation_id: extra_member.id,
                    member_type: MemberType::User,
                    member_id: "another-user".into(),
                    generation: 1,
                    joined_at: NOW.into(),
                    left_at: None,
                },
            ],
        )
        .unwrap();

    let created = store
        .get_or_create_dm(&other.id.to_string(), target.id, NOW)
        .unwrap();
    assert_ne!(created.id, wrong_types.id);
    assert_ne!(created.id, extra_member.id);
    assert_eq!(
        store
            .get_or_create_dm(&other.id.to_string(), target.id, LATER)
            .unwrap(),
        created
    );
    assert_eq!(
        store.list_conversation_members(created.id).unwrap(),
        vec![
            ConversationMember {
                conversation_id: created.id,
                member_type: MemberType::Agent,
                member_id: target.id.to_string(),
                generation: 1,
                joined_at: NOW.into(),
                left_at: None,
            },
            ConversationMember {
                conversation_id: created.id,
                member_type: MemberType::User,
                member_id: other.id.to_string(),
                generation: 1,
                joined_at: NOW.into(),
                left_at: None,
            },
        ]
    );
}

#[test]
fn get_or_create_dm_rejects_blank_user_and_timestamp() {
    let database = TestDatabase::new();
    let mut store = SqliteStore::open(database.path()).unwrap();
    let target = agent("codex");
    store.insert_agent(&target).unwrap();

    assert!(matches!(
        store.get_or_create_dm(" ", target.id, NOW),
        Err(StoreError::Domain(DomainError::EmptyField(
            "conversation_member.member_id"
        )))
    ));
    assert!(matches!(
        store.get_or_create_dm("tony", target.id, " "),
        Err(StoreError::Domain(DomainError::EmptyField(
            "conversation.created_at"
        )))
    ));
}

#[test]
fn get_or_create_agent_dm_reuses_only_the_exact_active_agent_pair() {
    let database = TestDatabase::new();
    let mut store = SqliteStore::open(database.path()).unwrap();
    let source = agent("codex");
    let target = agent("claude");
    let other = agent("gemini");
    for agent in [&source, &target, &other] {
        store.insert_agent(agent).unwrap();
    }

    let user_dm = store.get_or_create_dm("tony", source.id, NOW).unwrap();
    let wrong_pair = dm();
    store
        .insert_conversation_with_members(
            &wrong_pair,
            &[
                ConversationMember {
                    conversation_id: wrong_pair.id,
                    member_type: MemberType::Agent,
                    member_id: source.id.to_string(),
                    generation: 1,
                    joined_at: NOW.into(),
                    left_at: None,
                },
                ConversationMember {
                    conversation_id: wrong_pair.id,
                    member_type: MemberType::Agent,
                    member_id: other.id.to_string(),
                    generation: 1,
                    joined_at: NOW.into(),
                    left_at: None,
                },
            ],
        )
        .unwrap();
    let extra_member = dm();
    store
        .insert_conversation_with_members(
            &extra_member,
            &[
                ConversationMember {
                    conversation_id: extra_member.id,
                    member_type: MemberType::Agent,
                    member_id: source.id.to_string(),
                    generation: 1,
                    joined_at: NOW.into(),
                    left_at: None,
                },
                ConversationMember {
                    conversation_id: extra_member.id,
                    member_type: MemberType::Agent,
                    member_id: target.id.to_string(),
                    generation: 1,
                    joined_at: NOW.into(),
                    left_at: None,
                },
                ConversationMember {
                    conversation_id: extra_member.id,
                    member_type: MemberType::Agent,
                    member_id: other.id.to_string(),
                    generation: 1,
                    joined_at: NOW.into(),
                    left_at: None,
                },
            ],
        )
        .unwrap();
    let closed = Conversation {
        status: "closed".into(),
        ..dm()
    };
    store
        .insert_conversation_with_members(
            &closed,
            &[
                ConversationMember {
                    conversation_id: closed.id,
                    member_type: MemberType::Agent,
                    member_id: source.id.to_string(),
                    generation: 1,
                    joined_at: NOW.into(),
                    left_at: None,
                },
                ConversationMember {
                    conversation_id: closed.id,
                    member_type: MemberType::Agent,
                    member_id: target.id.to_string(),
                    generation: 1,
                    joined_at: NOW.into(),
                    left_at: None,
                },
            ],
        )
        .unwrap();

    let created = store
        .get_or_create_agent_dm(source.id, target.id, NOW)
        .unwrap();
    assert_ne!(created.id, user_dm.id);
    assert_ne!(created.id, wrong_pair.id);
    assert_ne!(created.id, extra_member.id);
    assert_ne!(created.id, closed.id);
    assert_eq!(
        store
            .get_or_create_agent_dm(target.id, source.id, LATER)
            .unwrap(),
        created
    );
    let members = store.list_conversation_members(created.id).unwrap();
    assert_eq!(members.len(), 2);
    assert!(members.iter().all(|member| {
        member.member_type == MemberType::Agent
            && member.generation == 1
            && member.joined_at == NOW
            && member.left_at.is_none()
    }));
    assert_eq!(
        members
            .into_iter()
            .map(|member| member.member_id)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([source.id.to_string(), target.id.to_string()])
    );
}

#[test]
fn get_or_create_agent_dm_rejects_self_missing_and_inactive_agents() {
    let database = TestDatabase::new();
    let mut store = SqliteStore::open(database.path()).unwrap();
    let source = agent("codex");
    let mut inactive = agent("claude");
    inactive.status = "inactive".into();
    store.insert_agent(&source).unwrap();
    store.insert_agent(&inactive).unwrap();

    assert!(matches!(
        store.get_or_create_agent_dm(source.id, source.id, NOW),
        Err(StoreError::InvalidStoredValue(
            "agent DM requires distinct agent IDs"
        ))
    ));
    assert!(matches!(
        store.get_or_create_agent_dm(source.id, AgentId::new(), NOW),
        Err(StoreError::AgentNotFound(_))
    ));
    assert!(matches!(
        store.get_or_create_agent_dm(source.id, inactive.id, NOW),
        Err(StoreError::AgentInactive(id)) if id == inactive.id
    ));
}

#[test]
fn concurrent_get_or_create_dm_returns_one_conversation() {
    let database = TestDatabase::new();
    let target = agent("codex");
    let store = SqliteStore::open(database.path()).unwrap();
    store.insert_agent(&target).unwrap();
    drop(store);

    let mut first_store = SqliteStore::open(database.path()).unwrap();
    let mut second_store = SqliteStore::open(database.path()).unwrap();
    let blocker = Connection::open(database.path()).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();

    let barrier = Arc::new(Barrier::new(3));
    let first_barrier = barrier.clone();
    let first = std::thread::spawn(move || {
        first_barrier.wait();
        first_store.get_or_create_dm("tony", target.id, NOW)
    });
    let second_barrier = barrier.clone();
    let second = std::thread::spawn(move || {
        second_barrier.wait();
        second_store.get_or_create_dm("tony", target.id, LATER)
    });

    barrier.wait();
    std::thread::sleep(std::time::Duration::from_millis(50));
    blocker.execute_batch("COMMIT").unwrap();

    let first = first.join().unwrap().unwrap();
    let second = second.join().unwrap().unwrap();
    assert_eq!(first.id, second.id);
}

#[test]
fn concurrent_get_or_create_agent_dm_returns_one_conversation() {
    let database = TestDatabase::new();
    let source = agent("codex");
    let target = agent("claude");
    let store = SqliteStore::open(database.path()).unwrap();
    store.insert_agent(&source).unwrap();
    store.insert_agent(&target).unwrap();
    drop(store);

    let mut first_store = SqliteStore::open(database.path()).unwrap();
    let mut second_store = SqliteStore::open(database.path()).unwrap();
    let blocker = Connection::open(database.path()).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();

    let barrier = Arc::new(Barrier::new(3));
    let first_barrier = barrier.clone();
    let first = std::thread::spawn(move || {
        first_barrier.wait();
        first_store.get_or_create_agent_dm(source.id, target.id, NOW)
    });
    let second_barrier = barrier.clone();
    let second = std::thread::spawn(move || {
        second_barrier.wait();
        second_store.get_or_create_agent_dm(target.id, source.id, LATER)
    });

    barrier.wait();
    std::thread::sleep(std::time::Duration::from_millis(50));
    blocker.execute_batch("COMMIT").unwrap();

    assert_eq!(
        first.join().unwrap().unwrap().id,
        second.join().unwrap().unwrap().id
    );
}

#[test]
fn message_insert_is_idempotent_only_for_the_exact_record() {
    let database = TestDatabase::new();
    let mut store = SqliteStore::open(database.path()).unwrap();
    let target = agent("codex");
    store.insert_agent(&target).unwrap();
    let conversation = store.get_or_create_dm("tony", target.id, NOW).unwrap();
    let message = Message {
        id: MessageId::new(),
        conversation_id: conversation.id,
        sender_type: MemberType::Agent,
        sender_id: target.id.to_string(),
        body: "answer".into(),
        reply_to: None,
        metadata: json!({"july": {"schema": 1}}),
        created_at: LATER.into(),
    };

    store.insert_message(&message).unwrap();
    store.insert_message(&message).unwrap();

    let conflicting = Message {
        body: "different answer".into(),
        ..message.clone()
    };
    assert!(matches!(
        store.insert_message(&conflicting),
        Err(StoreError::MessageConflict { id }) if id == message.id
    ));
    assert_eq!(store.get_message(message.id).unwrap(), Some(message));
}

#[tokio::test]
async fn storage_worker_persists_dm_messages_and_returns_latest_binding() {
    let database = TestDatabase::new();
    let target = agent("codex");
    let store = SqliteStore::open(database.path()).unwrap();
    store.insert_agent(&target).unwrap();
    drop(store);

    let mut worker = StorageWorker::open(database.path()).unwrap();
    assert_eq!(
        worker.get_agent_by_name("codex".into()).await.unwrap(),
        Some(target.clone())
    );
    let conversation = worker
        .get_or_create_dm("tony".into(), target.id, NOW.into())
        .await
        .unwrap();
    let old_binding = SessionBinding {
        id: Default::default(),
        conversation_id: conversation.id,
        agent_id: target.id,
        transport_type: "acp".into(),
        remote_session_id: Some("remote-old".into()),
        generation: 1,
        status: SessionBindingStatus::Closed,
        created_at: NOW.into(),
        last_used_at: NOW.into(),
    };
    let latest_binding = SessionBinding {
        id: SessionBindingId::new(),
        remote_session_id: Some("remote-lost".into()),
        generation: 2,
        status: SessionBindingStatus::Lost,
        created_at: LATER.into(),
        last_used_at: LATER.into(),
        ..old_binding.clone()
    };
    worker.insert_session_binding(old_binding).await.unwrap();
    worker
        .insert_session_binding(latest_binding.clone())
        .await
        .unwrap();

    let message = Message {
        id: MessageId::new(),
        conversation_id: conversation.id,
        sender_type: MemberType::User,
        sender_id: "tony".into(),
        body: "Continue Phase 3".into(),
        reply_to: None,
        metadata: json!({"source": "cli", "kind": "prompt"}),
        created_at: LATER.into(),
    };
    worker.insert_message(message.clone()).await.unwrap();
    assert_eq!(
        worker.list_messages(conversation.id).await.unwrap(),
        vec![message]
    );
    assert_eq!(
        worker
            .get_latest_session_binding(conversation.id, target.id)
            .await
            .unwrap(),
        Some(latest_binding)
    );
    worker.shutdown().await.unwrap();
}
