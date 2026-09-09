use july_workspace::domain::WorkScope;
use july_workspace::domain::{
    Conversation, ConversationId, ConversationKind, MemberType, Memory, MemoryId, MemoryKind,
    MemoryScopeType, Message, MessageId, ResultId, WorkItem, WorkItemId, WorkResult, WorkStatus,
};
use july_workspace::runtime::StorageWorker;
use july_workspace::storage::SqliteStore;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use ulid::Ulid;

const CREATED: &str = "2026-08-22T10:00:00Z";
const LATER: &str = "2026-08-22T11:00:00Z";
const SHARED_SCOPE_ID: &str = "durable-scope";

struct TestDatabase {
    directory: PathBuf,
    path: PathBuf,
}

impl TestDatabase {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("july-memory-promotion-{}", Ulid::generate()));
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

fn conversation() -> Conversation {
    Conversation {
        id: ConversationId::new(),
        kind: ConversationKind::Dm,
        room_id: None,
        title: None,
        goal: None,
        parent_conversation_id: None,
        origin_conversation_id: None,
        status: "open".into(),
        created_at: CREATED.into(),
        updated_at: CREATED.into(),
    }
}

fn memory(
    id: &str,
    scope_type: MemoryScopeType,
    scope_id: &str,
    kind: MemoryKind,
    source_conversation_id: Option<ConversationId>,
    supersedes_memory_id: Option<MemoryId>,
    created_at: &str,
) -> Memory {
    Memory {
        id: MemoryId::from_str(id).unwrap(),
        scope_type,
        scope_id: scope_id.into(),
        kind,
        content: format!("memory-{id}"),
        source_conversation_id,
        evidence: vec!["verified test evidence".into(), "operator approval".into()],
        supersedes_memory_id,
        created_at: created_at.into(),
    }
}

#[tokio::test]
async fn explicit_promotion_lists_exact_scope_and_kind_in_stable_order_after_restart() {
    let database = TestDatabase::new();
    let source = conversation();
    let store = SqliteStore::open(database.path()).unwrap();
    store.insert_conversation(&source).unwrap();
    drop(store);

    let first = memory(
        "00000000000000000000000001",
        MemoryScopeType::Project,
        SHARED_SCOPE_ID,
        MemoryKind::Fact,
        Some(source.id),
        None,
        CREATED,
    );
    let same_time_second = memory(
        "00000000000000000000000002",
        MemoryScopeType::Project,
        SHARED_SCOPE_ID,
        MemoryKind::Fact,
        Some(source.id),
        None,
        CREATED,
    );
    let superseding_decision = memory(
        "00000000000000000000000003",
        MemoryScopeType::Project,
        SHARED_SCOPE_ID,
        MemoryKind::Decision,
        Some(source.id),
        Some(first.id),
        LATER,
    );
    let room_constraint = memory(
        "00000000000000000000000004",
        MemoryScopeType::Room,
        SHARED_SCOPE_ID,
        MemoryKind::Constraint,
        Some(source.id),
        None,
        CREATED,
    );
    let agent_result = memory(
        "00000000000000000000000005",
        MemoryScopeType::Agent,
        SHARED_SCOPE_ID,
        MemoryKind::Result,
        Some(source.id),
        None,
        CREATED,
    );

    let mut worker = StorageWorker::open(database.path()).unwrap();
    for memory in [
        same_time_second.clone(),
        first.clone(),
        superseding_decision.clone(),
        room_constraint.clone(),
        agent_result.clone(),
    ] {
        worker.promote_memory(memory).await.unwrap();
    }
    assert_eq!(
        worker
            .list_memories(MemoryScopeType::Project, SHARED_SCOPE_ID.into(), None)
            .await
            .unwrap(),
        vec![
            first.clone(),
            same_time_second.clone(),
            superseding_decision.clone()
        ]
    );
    assert_eq!(
        worker
            .list_memories(
                MemoryScopeType::Project,
                SHARED_SCOPE_ID.into(),
                Some(MemoryKind::Fact),
            )
            .await
            .unwrap(),
        vec![first.clone(), same_time_second.clone()]
    );
    assert_eq!(
        worker
            .list_memories(MemoryScopeType::Room, SHARED_SCOPE_ID.into(), None)
            .await
            .unwrap(),
        vec![room_constraint]
    );
    assert_eq!(
        worker
            .list_memories(MemoryScopeType::Agent, SHARED_SCOPE_ID.into(), None)
            .await
            .unwrap(),
        vec![agent_result]
    );
    worker.shutdown().await.unwrap();

    let mut restarted = StorageWorker::open(database.path()).unwrap();
    assert_eq!(
        restarted
            .list_memories(MemoryScopeType::Project, SHARED_SCOPE_ID.into(), None,)
            .await
            .unwrap(),
        vec![first, same_time_second, superseding_decision]
    );
    restarted.shutdown().await.unwrap();
}

#[tokio::test]
async fn raw_message_and_work_result_do_not_create_memory() {
    let database = TestDatabase::new();
    let source = conversation();
    let message = Message {
        id: MessageId::new(),
        conversation_id: source.id,
        sender_type: MemberType::User,
        sender_id: "tony".into(),
        body: "I think this may be a fact".into(),
        reply_to: None,
        metadata: serde_json::Value::Null,
        created_at: CREATED.into(),
    };
    let work = WorkItem {
        id: WorkItemId::new(),
        scope: WorkScope::Conversation(source.id),
        title: "Produce raw result".into(),
        goal: None,
        status: WorkStatus::Open,
        owner_agent_id: None,
        is_primary: false,
        created_at: CREATED.into(),
        updated_at: CREATED.into(),
        completed_at: None,
    };
    let result = WorkResult {
        id: ResultId::new(),
        work_id: work.id,
        status: "accepted".into(),
        summary: "A raw result is not promoted automatically".into(),
        outputs: vec![],
        evidence: vec!["worker output".into()],
        supersedes_result_id: None,
        created_at: LATER.into(),
    };

    let mut store = SqliteStore::open(database.path()).unwrap();
    store.insert_conversation(&source).unwrap();
    store.insert_message(&message).unwrap();
    store.insert_work_item(&work).unwrap();
    store
        .transition_work(work.id, WorkStatus::Working, CREATED)
        .unwrap();
    store.create_work_result(&result).unwrap();
    drop(store);

    let mut worker = StorageWorker::open(database.path()).unwrap();
    assert!(
        worker
            .list_memories(MemoryScopeType::Project, "cashpoint".into(), None)
            .await
            .unwrap()
            .is_empty()
    );
    worker.shutdown().await.unwrap();
}

#[tokio::test]
async fn invalid_or_orphaned_promotion_leaves_no_partial_memory() {
    let database = TestDatabase::new();
    let source = conversation();
    let store = SqliteStore::open(database.path()).unwrap();
    store.insert_conversation(&source).unwrap();
    drop(store);

    let blank_scope = Memory {
        scope_id: " ".into(),
        ..memory(
            "00000000000000000000000006",
            MemoryScopeType::Project,
            "cashpoint",
            MemoryKind::Fact,
            Some(source.id),
            None,
            CREATED,
        )
    };
    let missing_provenance = memory(
        "00000000000000000000000007",
        MemoryScopeType::Project,
        "cashpoint",
        MemoryKind::Fact,
        Some(ConversationId::new()),
        None,
        CREATED,
    );
    let missing_supersession = memory(
        "00000000000000000000000008",
        MemoryScopeType::Project,
        "cashpoint",
        MemoryKind::Fact,
        Some(source.id),
        Some(MemoryId::new()),
        CREATED,
    );
    let missing_source = memory(
        "00000000000000000000000009",
        MemoryScopeType::Project,
        "cashpoint",
        MemoryKind::Fact,
        None,
        None,
        CREATED,
    );
    let self_supersession = memory(
        "0000000000000000000000000A",
        MemoryScopeType::Project,
        "cashpoint",
        MemoryKind::Fact,
        Some(source.id),
        Some(MemoryId::from_str("0000000000000000000000000A").unwrap()),
        CREATED,
    );

    let predecessor = memory(
        "0000000000000000000000000B",
        MemoryScopeType::Project,
        "cashpoint",
        MemoryKind::Fact,
        Some(source.id),
        None,
        CREATED,
    );
    let cross_scope_supersession = memory(
        "0000000000000000000000000C",
        MemoryScopeType::Room,
        "cashpoint",
        MemoryKind::Decision,
        Some(source.id),
        Some(predecessor.id),
        LATER,
    );

    let mut worker = StorageWorker::open(database.path()).unwrap();
    worker.promote_memory(predecessor.clone()).await.unwrap();
    for memory in [
        blank_scope,
        missing_provenance,
        missing_supersession,
        missing_source,
        self_supersession,
        cross_scope_supersession,
    ] {
        assert!(worker.promote_memory(memory).await.is_err());
    }
    assert_eq!(
        worker
            .list_memories(MemoryScopeType::Project, "cashpoint".into(), None)
            .await
            .unwrap(),
        vec![predecessor]
    );
    assert!(
        worker
            .list_memories(MemoryScopeType::Room, "cashpoint".into(), None)
            .await
            .unwrap()
            .is_empty()
    );
    worker.shutdown().await.unwrap();
}
