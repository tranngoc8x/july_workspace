use july_workspace::domain::{
    Agent, Conversation, ConversationKind, PermissionDecision, PermissionOption, PermissionOutcome,
    SessionBinding, SessionBindingStatus,
};
use july_workspace::runtime::StorageWorker;
use july_workspace::storage::SqliteStore;
use serde_json::json;
use std::path::PathBuf;

const NOW: &str = "2026-08-11T00:00:00Z";

struct TestDatabase(PathBuf);

impl TestDatabase {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("july-runtime-{}.sqlite3", ulid::Ulid::generate())))
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
