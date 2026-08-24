use july_workspace::domain::{
    Agent, AgentId, Checkpoint, CheckpointId, Conversation, ConversationId, ConversationKind,
    MemberType, Message, MessageId,
};
use july_workspace::runtime::StorageWorker;
use july_workspace::storage::SqliteStore;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use ulid::Ulid;

const CREATED: &str = "2026-08-22T10:00:00Z";
const SAME_TIME: &str = "2026-08-22T11:00:00Z";
const LATER: &str = "2026-08-22T12:00:00Z";

struct TestDatabase {
    directory: PathBuf,
    path: PathBuf,
}

impl TestDatabase {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("july-checkpoint-storage-test-{}", Ulid::generate()));
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
        created_at: CREATED.into(),
        updated_at: CREATED.into(),
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

fn checkpoint(
    id: &str,
    conversation_id: ConversationId,
    agent_id: AgentId,
    last_message_id: Option<MessageId>,
    created_at: &str,
) -> Checkpoint {
    Checkpoint {
        id: CheckpointId::from_str(id).unwrap(),
        conversation_id,
        agent_id,
        goal: Some("Resume durable checkpoint".into()),
        current_state: Some("all optional fields round-trip".into()),
        decisions: vec!["keep scope exact".into()],
        open_items: vec!["run focused test".into()],
        references: vec!["docs/07-MEMORY-AND-SESSIONS.md".into()],
        last_message_id,
        created_at: created_at.into(),
    }
}

#[tokio::test]
async fn storage_worker_returns_scoped_latest_checkpoint_after_restart() {
    let database = TestDatabase::new();
    let primary_agent = agent("codex");
    let other_agent = agent("claude");
    let primary_conversation = conversation();
    let other_conversation = conversation();
    let message = Message {
        id: MessageId::new(),
        conversation_id: primary_conversation.id,
        sender_type: MemberType::User,
        sender_id: "tony".into(),
        body: "resume from checkpoint".into(),
        reply_to: None,
        metadata: json!({}),
        created_at: CREATED.into(),
    };

    let store = SqliteStore::open(database.path()).unwrap();
    for agent in [&primary_agent, &other_agent] {
        store.insert_agent(agent).unwrap();
    }
    for conversation in [&primary_conversation, &other_conversation] {
        store.insert_conversation(conversation).unwrap();
    }
    store.insert_message(&message).unwrap();
    drop(store);

    let older = checkpoint(
        "00000000000000000000000001",
        primary_conversation.id,
        primary_agent.id,
        Some(message.id),
        SAME_TIME,
    );
    let latest = checkpoint(
        "00000000000000000000000002",
        primary_conversation.id,
        primary_agent.id,
        Some(message.id),
        SAME_TIME,
    );
    let other_conversation_checkpoint = Checkpoint {
        goal: None,
        current_state: None,
        decisions: vec![],
        open_items: vec![],
        references: vec![],
        last_message_id: None,
        ..checkpoint(
            "00000000000000000000000003",
            other_conversation.id,
            primary_agent.id,
            None,
            LATER,
        )
    };
    let other_agent_checkpoint = checkpoint(
        "00000000000000000000000004",
        primary_conversation.id,
        other_agent.id,
        Some(message.id),
        LATER,
    );

    let mut worker = StorageWorker::open(database.path()).unwrap();
    for checkpoint in [
        older,
        latest.clone(),
        other_conversation_checkpoint.clone(),
        other_agent_checkpoint,
    ] {
        worker.insert_checkpoint(checkpoint).await.unwrap();
    }
    assert_eq!(
        worker
            .get_latest_checkpoint(primary_conversation.id, primary_agent.id)
            .await
            .unwrap(),
        Some(latest.clone())
    );
    worker.shutdown().await.unwrap();

    let mut restarted = StorageWorker::open(database.path()).unwrap();
    assert_eq!(
        restarted
            .get_latest_checkpoint(primary_conversation.id, primary_agent.id)
            .await
            .unwrap(),
        Some(latest)
    );
    assert_eq!(
        restarted
            .get_latest_checkpoint(other_conversation.id, primary_agent.id)
            .await
            .unwrap(),
        Some(other_conversation_checkpoint)
    );
    restarted.shutdown().await.unwrap();
}

#[tokio::test]
async fn storage_worker_rejects_invalid_or_orphaned_checkpoints_without_persisting_them() {
    let database = TestDatabase::new();
    let valid_agent = agent("codex");
    let valid_conversation = conversation();
    let other_conversation = conversation();
    let other_message = Message {
        id: MessageId::new(),
        conversation_id: other_conversation.id,
        sender_type: MemberType::User,
        sender_id: "tony".into(),
        body: "belongs to another conversation".into(),
        reply_to: None,
        metadata: json!({}),
        created_at: CREATED.into(),
    };
    let store = SqliteStore::open(database.path()).unwrap();
    store.insert_agent(&valid_agent).unwrap();
    store.insert_conversation(&valid_conversation).unwrap();
    store.insert_conversation(&other_conversation).unwrap();
    store.insert_message(&other_message).unwrap();
    drop(store);

    let invalid = Checkpoint {
        created_at: " ".into(),
        ..checkpoint(
            "00000000000000000000000005",
            valid_conversation.id,
            valid_agent.id,
            None,
            CREATED,
        )
    };
    let missing_conversation_id = ConversationId::new();
    let missing_conversation = checkpoint(
        "00000000000000000000000006",
        missing_conversation_id,
        valid_agent.id,
        None,
        CREATED,
    );
    let missing_agent_id = AgentId::new();
    let missing_agent = checkpoint(
        "00000000000000000000000007",
        valid_conversation.id,
        missing_agent_id,
        None,
        CREATED,
    );
    let cross_conversation_anchor = checkpoint(
        "00000000000000000000000008",
        valid_conversation.id,
        valid_agent.id,
        Some(other_message.id),
        CREATED,
    );

    let mut worker = StorageWorker::open(database.path()).unwrap();
    assert!(worker.insert_checkpoint(invalid).await.is_err());
    assert!(
        worker
            .insert_checkpoint(missing_conversation)
            .await
            .is_err()
    );
    assert!(worker.insert_checkpoint(missing_agent).await.is_err());
    assert!(
        worker
            .insert_checkpoint(cross_conversation_anchor)
            .await
            .is_err()
    );
    assert_eq!(
        worker
            .get_latest_checkpoint(valid_conversation.id, valid_agent.id)
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        worker
            .get_latest_checkpoint(missing_conversation_id, valid_agent.id)
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        worker
            .get_latest_checkpoint(valid_conversation.id, missing_agent_id)
            .await
            .unwrap(),
        None
    );
    worker.shutdown().await.unwrap();
}
