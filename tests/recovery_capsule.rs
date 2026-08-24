use july_workspace::application::{
    BuildRecoveryCapsule, RECENT_MESSAGE_LIMIT, RecoveryError, RecoveryService,
};
use july_workspace::domain::{
    Agent, AgentId, Checkpoint, CheckpointId, Conversation, ConversationId, ConversationKind,
    MemberType, Memory, MemoryId, MemoryKind, MemoryScopeType, Message, MessageId, PublishId, Room,
    RoomId, RoomMember, WorkItem, WorkItemId, WorkResult, WorkStatus,
};
use july_workspace::runtime::StorageWorker;
use july_workspace::storage::SqliteStore;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use ulid::Ulid;

const CREATED: &str = "2026-08-22T10:00:00Z";
const LATER: &str = "2026-08-22T11:00:00Z";

struct TestDatabase {
    directory: PathBuf,
    path: PathBuf,
}

impl TestDatabase {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("july-recovery-capsule-{}", Ulid::generate()));
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

fn id<T: From<Ulid>>(value: u128) -> T {
    Ulid::from(value).into()
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
        created_at: CREATED.into(),
        updated_at: CREATED.into(),
    }
}

fn dm(store: &mut SqliteStore, agent: &Agent, user: &str) -> Conversation {
    store.insert_agent(agent).unwrap();
    store.get_or_create_dm(user, agent.id, CREATED).unwrap()
}

fn bare_dm() -> Conversation {
    Conversation {
        id: ConversationId::new(),
        kind: ConversationKind::Dm,
        room_id: None,
        title: None,
        goal: Some("Continue durable work".into()),
        parent_conversation_id: None,
        origin_conversation_id: None,
        status: "open".into(),
        created_at: CREATED.into(),
        updated_at: CREATED.into(),
    }
}

fn message(
    id: u128,
    conversation_id: ConversationId,
    body: impl Into<String>,
    created_at: &str,
) -> Message {
    Message {
        id: self::id(id),
        conversation_id,
        sender_type: MemberType::User,
        sender_id: "tony".into(),
        body: body.into(),
        reply_to: None,
        metadata: Value::Null,
        created_at: created_at.into(),
    }
}

fn command(conversation_id: ConversationId, agent_id: AgentId) -> BuildRecoveryCapsule {
    BuildRecoveryCapsule {
        conversation_id,
        agent_id,
    }
}

fn parse(content: &str) -> Value {
    serde_json::from_str(content).unwrap()
}

#[tokio::test]
async fn no_checkpoint_emits_newest_twenty_chronologically_with_stable_id_ties() {
    let database = TestDatabase::new();
    let target = agent("codex");
    let conversation = {
        let mut store = SqliteStore::open(database.path()).unwrap();
        let conversation = dm(&mut store, &target, "tony");
        for number in 1..=25 {
            store
                .insert_message(&message(
                    number,
                    conversation.id,
                    format!("message-{number:02}"),
                    CREATED,
                ))
                .unwrap();
        }
        conversation
    };
    let mut service = RecoveryService::new(StorageWorker::open(database.path()).unwrap());

    let capsule = service
        .build(command(conversation.id, target.id))
        .await
        .unwrap();
    let document = parse(&capsule.content);
    let bodies: Vec<_> = document["recent_messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|message| message["body"].as_str().unwrap())
        .collect();

    assert_eq!(RECENT_MESSAGE_LIMIT, 20);
    assert_eq!(capsule.recent_message_count, 20);
    assert!(capsule.messages_truncated);
    assert_eq!(document["recent_messages_meta"]["included_count"], 20);
    assert_eq!(document["recent_messages_meta"]["truncated"], true);
    assert_eq!(bodies.first(), Some(&"message-06"));
    assert_eq!(bodies.last(), Some(&"message-25"));
}

#[tokio::test]
async fn checkpoint_anchor_emits_only_strictly_later_messages() {
    let database = TestDatabase::new();
    let target = agent("codex");
    let (conversation, anchor) = {
        let mut store = SqliteStore::open(database.path()).unwrap();
        let conversation = dm(&mut store, &target, "tony");
        let anchor = message(10, conversation.id, "anchor", CREATED);
        for message in [
            message(9, conversation.id, "before", CREATED),
            anchor.clone(),
            message(11, conversation.id, "after-same-time", CREATED),
            message(1, conversation.id, "after-later-time", LATER),
        ] {
            store.insert_message(&message).unwrap();
        }
        store
            .insert_checkpoint(&Checkpoint {
                id: CheckpointId::new(),
                conversation_id: conversation.id,
                agent_id: target.id,
                goal: Some("Resume".into()),
                current_state: Some("Anchored".into()),
                decisions: vec![],
                open_items: vec![],
                references: vec![],
                last_message_id: Some(anchor.id),
                created_at: LATER.into(),
            })
            .unwrap();
        (conversation, anchor)
    };
    let mut service = RecoveryService::new(StorageWorker::open(database.path()).unwrap());

    let capsule = service
        .build(command(conversation.id, target.id))
        .await
        .unwrap();
    let messages = parse(&capsule.content)["recent_messages"]
        .as_array()
        .unwrap()
        .clone();

    assert_eq!(capsule.recent_message_count, 2);
    assert!(!capsule.messages_truncated);
    let document = parse(&capsule.content);
    assert_eq!(document["recent_messages_meta"]["included_count"], 2);
    assert_eq!(document["recent_messages_meta"]["truncated"], false);
    assert_eq!(messages[0]["body"], "after-same-time");
    assert_eq!(messages[1]["body"], "after-later-time");
    assert_ne!(messages[0]["id"], anchor.id.to_string());
}

#[tokio::test]
async fn missing_and_wrong_conversation_checkpoint_anchors_never_fall_back() {
    let database = TestDatabase::new();
    let target = agent("codex");
    let (conversation, wrong_anchor, checkpoint_id) = {
        let mut store = SqliteStore::open(database.path()).unwrap();
        let conversation = dm(&mut store, &target, "tony");
        let other = bare_dm();
        store.insert_conversation(&other).unwrap();
        let wrong_anchor = message(30, other.id, "foreign", CREATED);
        store.insert_message(&wrong_anchor).unwrap();
        store
            .insert_message(&message(31, conversation.id, "must not replay", LATER))
            .unwrap();
        let checkpoint_id = CheckpointId::new();
        store
            .insert_checkpoint(&Checkpoint {
                id: checkpoint_id,
                conversation_id: conversation.id,
                agent_id: target.id,
                goal: None,
                current_state: None,
                decisions: vec![],
                open_items: vec![],
                references: vec![],
                last_message_id: Some(wrong_anchor.id),
                created_at: LATER.into(),
            })
            .unwrap();
        (conversation, wrong_anchor, checkpoint_id)
    };
    let mut service = RecoveryService::new(StorageWorker::open(database.path()).unwrap());
    assert_eq!(
        service.build(command(conversation.id, target.id)).await,
        Err(RecoveryError::InvalidCheckpointAnchor {
            checkpoint_id,
            message_id: wrong_anchor.id,
        })
    );
    drop(service);

    let missing: MessageId = id(99);
    let connection = rusqlite::Connection::open(database.path()).unwrap();
    connection
        .pragma_update(None, "foreign_keys", "OFF")
        .unwrap();
    connection
        .execute(
            "UPDATE checkpoints SET last_message_id = ?1 WHERE id = ?2",
            [missing.to_string(), checkpoint_id.to_string()],
        )
        .unwrap();
    drop(connection);
    let mut service = RecoveryService::new(StorageWorker::open(database.path()).unwrap());
    assert_eq!(
        service.build(command(conversation.id, target.id)).await,
        Err(RecoveryError::InvalidCheckpointAnchor {
            checkpoint_id,
            message_id: missing,
        })
    );
}

fn memory(
    id: u128,
    scope_type: MemoryScopeType,
    scope_id: String,
    kind: MemoryKind,
    content: &str,
    supersedes_memory_id: Option<MemoryId>,
) -> Memory {
    Memory {
        id: self::id(id),
        scope_type,
        scope_id,
        kind,
        content: content.into(),
        source_conversation_id: None,
        evidence: vec!["verified\nevidence".into()],
        supersedes_memory_id,
        created_at: CREATED.into(),
    }
}

#[tokio::test]
async fn thread_includes_only_current_project_and_own_room_memories() {
    let database = TestDatabase::new();
    let target = agent("codex");
    let (thread, old_project) = {
        let mut store = SqliteStore::open(database.path()).unwrap();
        store.insert_agent(&target).unwrap();
        let room = Room {
            id: RoomId::new(),
            name: "payments".into(),
            description: None,
            status: "active".into(),
            created_at: CREATED.into(),
            updated_at: CREATED.into(),
        };
        store
            .insert_room_with_members(
                &room,
                &[RoomMember {
                    room_id: room.id,
                    agent_id: target.id,
                    role: None,
                    generation: 1,
                    joined_at: CREATED.into(),
                    left_at: None,
                }],
            )
            .unwrap();
        let thread = Conversation {
            id: ConversationId::new(),
            kind: ConversationKind::Thread,
            room_id: Some(room.id),
            title: Some("Payment recovery".into()),
            goal: Some("Khôi phục ✅".into()),
            parent_conversation_id: None,
            origin_conversation_id: None,
            status: "open".into(),
            created_at: CREATED.into(),
            updated_at: CREATED.into(),
        };
        store
            .create_thread_with_primary_work(&thread, WorkItemId::new(), "tony", &[target.id])
            .unwrap();
        let old_project = memory(
            1,
            MemoryScopeType::Project,
            target.project_root.clone(),
            MemoryKind::Fact,
            "obsolete",
            None,
        );
        for memory in [
            old_project.clone(),
            memory(
                2,
                MemoryScopeType::Project,
                target.project_root.clone(),
                MemoryKind::Decision,
                "current project\nquyết định",
                Some(old_project.id),
            ),
            memory(
                3,
                MemoryScopeType::Project,
                "/workspace/sibling".into(),
                MemoryKind::Fact,
                "sibling project secret",
                None,
            ),
            memory(
                4,
                MemoryScopeType::Room,
                room.id.to_string(),
                MemoryKind::Constraint,
                "own room contract",
                None,
            ),
            memory(
                5,
                MemoryScopeType::Room,
                RoomId::new().to_string(),
                MemoryKind::Constraint,
                "sibling room secret",
                None,
            ),
        ] {
            store.insert_memory(&memory).unwrap();
        }
        store
            .insert_checkpoint(&Checkpoint {
                id: CheckpointId::new(),
                conversation_id: thread.id,
                agent_id: target.id,
                goal: Some("Resume deterministic state".into()),
                current_state: Some("line one\ntrạng thái ✅".into()),
                decisions: vec!["preserve exact text".into()],
                open_items: vec!["continue".into()],
                references: vec!["src/application/recovery.rs".into()],
                last_message_id: None,
                created_at: LATER.into(),
            })
            .unwrap();
        (thread, old_project)
    };
    let mut service = RecoveryService::new(StorageWorker::open(database.path()).unwrap());

    let first = service.build(command(thread.id, target.id)).await.unwrap();
    let second = service.build(command(thread.id, target.id)).await.unwrap();
    let document = parse(&first.content);

    assert_eq!(first, second);
    assert_eq!(document["version"], "JULY_RECOVERY_V1");
    assert_eq!(document["conversation"]["goal"], "Khôi phục ✅");
    assert_eq!(
        document["checkpoint"]["current_state"],
        "line one\ntrạng thái ✅"
    );
    assert_eq!(document["project_memories"].as_array().unwrap().len(), 1);
    assert_eq!(
        document["project_memories"][0]["content"],
        "current project\nquyết định"
    );
    assert_eq!(
        document["project_memories"][0]["supersedes_memory_id"],
        old_project.id.to_string()
    );
    assert_eq!(document["room_memories"].as_array().unwrap().len(), 1);
    assert_eq!(document["room_memories"][0]["content"], "own room contract");
    assert!(!first.content.contains("sibling"));
    assert!(!first.content.contains("obsolete"));
}

fn seed_result(store: &mut SqliteStore, source: &Conversation) -> WorkResult {
    let work = WorkItem {
        id: WorkItemId::new(),
        conversation_id: source.id,
        title: "Publish compact result".into(),
        goal: None,
        status: WorkStatus::Open,
        owner_agent_id: None,
        is_primary: false,
        created_at: CREATED.into(),
        updated_at: CREATED.into(),
        completed_at: None,
    };
    store.insert_work_item(&work).unwrap();
    store
        .transition_work(work.id, WorkStatus::Working, CREATED)
        .unwrap();
    store
        .create_work_result(&WorkResult {
            id: july_workspace::domain::ResultId::new(),
            work_id: work.id,
            status: "accepted".into(),
            summary: "Kết quả đã xuất bản".into(),
            outputs: vec![],
            evidence: vec![],
            supersedes_result_id: None,
            created_at: LATER.into(),
        })
        .unwrap()
}

#[tokio::test]
async fn dm_omits_room_memory_and_includes_links_without_source_transcript() {
    let database = TestDatabase::new();
    let target_agent = agent("codex");
    let source_agent = agent("claude");
    let (target, source, result, publish_id) = {
        let mut store = SqliteStore::open(database.path()).unwrap();
        let target = dm(&mut store, &target_agent, "tony");
        let source = dm(&mut store, &source_agent, "tony");
        store
            .insert_message(&message(
                70,
                source.id,
                "private source transcript",
                CREATED,
            ))
            .unwrap();
        store
            .insert_memory(&memory(
                71,
                MemoryScopeType::Room,
                RoomId::new().to_string(),
                MemoryKind::Fact,
                "unrelated room memory",
                None,
            ))
            .unwrap();
        let result = seed_result(&mut store, &source);
        let publish_id = PublishId::new();
        store
            .publish_result(publish_id, result.id, target.id, LATER)
            .unwrap();
        (target, source, result, publish_id)
    };
    let mut service = RecoveryService::new(StorageWorker::open(database.path()).unwrap());

    let capsule = service
        .build(command(target.id, target_agent.id))
        .await
        .unwrap();
    let document = parse(&capsule.content);

    assert!(document.get("room_memories").is_none());
    assert_eq!(
        document["published_results"][0]["publish_id"],
        publish_id.to_string()
    );
    assert_eq!(
        document["published_results"][0]["result_id"],
        result.id.to_string()
    );
    assert_eq!(
        document["published_results"][0]["source_conversation_id"],
        source.id.to_string()
    );
    assert_eq!(document["published_results"][0]["status"], "accepted");
    assert_eq!(
        document["published_results"][0]["summary"],
        "Kết quả đã xuất bản"
    );
    assert!(!capsule.content.contains("private source transcript"));
    assert!(!capsule.content.contains("unrelated room memory"));
}

#[tokio::test]
async fn missing_inactive_and_non_member_agents_are_rejected_before_exposure() {
    let database = TestDatabase::new();
    let member = agent("codex");
    let outsider = agent("claude");
    let (conversation, unscoped) = {
        let mut store = SqliteStore::open(database.path()).unwrap();
        let conversation = dm(&mut store, &member, "tony");
        store.insert_agent(&outsider).unwrap();
        let unscoped = bare_dm();
        store.insert_conversation(&unscoped).unwrap();
        (conversation, unscoped)
    };
    let mut service = RecoveryService::new(StorageWorker::open(database.path()).unwrap());

    let missing_agent = AgentId::new();
    assert_eq!(
        service.build(command(conversation.id, missing_agent)).await,
        Err(RecoveryError::AgentNotFound(missing_agent))
    );
    let missing_conversation = ConversationId::new();
    assert_eq!(
        service
            .build(command(missing_conversation, member.id))
            .await,
        Err(RecoveryError::ConversationNotFound(missing_conversation))
    );
    assert_eq!(
        service.build(command(conversation.id, outsider.id)).await,
        Err(RecoveryError::AgentNotMember {
            conversation_id: conversation.id,
            agent_id: outsider.id,
        })
    );
    assert_eq!(
        service.build(command(unscoped.id, member.id)).await,
        Err(RecoveryError::AgentNotMember {
            conversation_id: unscoped.id,
            agent_id: member.id,
        })
    );
    drop(service);

    let store = SqliteStore::open(database.path()).unwrap();
    let mut inactive = member.clone();
    inactive.status = "inactive".into();
    inactive.updated_at = LATER.into();
    store.update_agent(&inactive).unwrap();
    drop(store);
    let mut service = RecoveryService::new(StorageWorker::open(database.path()).unwrap());
    assert_eq!(
        service.build(command(conversation.id, member.id)).await,
        Err(RecoveryError::AgentInactive(member.id))
    );
}
