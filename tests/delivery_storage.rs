use july_workspace::domain::{
    Agent, AgentId, DeliveryStatus, DomainError, MemberType, Message, MessageDelivery, MessageId,
};
use july_workspace::runtime::StorageWorker;
use july_workspace::storage::{SqliteStore, StoreError};
use serde_json::json;
use std::path::{Path, PathBuf};
use ulid::Ulid;

const NOW: &str = "2026-08-22T10:00:00Z";
const LATER: &str = "2026-08-22T10:01:00Z";

struct TestDatabase {
    directory: PathBuf,
    path: PathBuf,
}

impl TestDatabase {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("july-delivery-storage-test-{}", Ulid::generate()));
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

fn message(store: &mut SqliteStore, target: &Agent) -> Message {
    let conversation = store
        .get_or_create_dm("tony", target.id, NOW)
        .expect("create DM");
    Message {
        id: MessageId::new(),
        conversation_id: conversation.id,
        sender_type: MemberType::User,
        sender_id: "tony".into(),
        body: "check delivery".into(),
        reply_to: None,
        metadata: json!({}),
        created_at: NOW.into(),
    }
}

#[test]
fn message_and_pending_delivery_are_atomic_and_exact_idempotent() {
    let database = TestDatabase::new();
    let mut store = SqliteStore::open(database.path()).unwrap();
    let target = agent("codex");
    store.insert_agent(&target).unwrap();
    let message = message(&mut store, &target);

    assert!(
        store
            .insert_message_with_pending_delivery(&message, target.id, Some("thread capsule"))
            .unwrap()
    );
    assert_eq!(
        store.get_message_delivery(message.id, target.id).unwrap(),
        Some(MessageDelivery {
            message_id: message.id,
            target_agent_id: target.id,
            status: DeliveryStatus::Pending,
            capsule: Some("thread capsule".into()),
            capsule_delivered_at: None,
            created_at: NOW.into(),
            updated_at: NOW.into(),
            delivered_at: None,
        })
    );
    assert!(
        !store
            .insert_message_with_pending_delivery(&message, target.id, Some("thread capsule"))
            .unwrap()
    );

    assert!(matches!(
        store.insert_message_with_pending_delivery(&message, target.id, Some("changed capsule")),
        Err(StoreError::DeliveryConflict {
            message_id,
            target_agent_id,
        }) if message_id == message.id && target_agent_id == target.id
    ));
}

#[test]
fn exact_message_can_add_one_delivery_per_target() {
    let database = TestDatabase::new();
    let mut store = SqliteStore::open(database.path()).unwrap();
    let first_target = agent("codex");
    let second_target = agent("claude");
    store.insert_agent(&first_target).unwrap();
    store.insert_agent(&second_target).unwrap();
    let message = message(&mut store, &first_target);

    assert!(
        store
            .insert_message_with_pending_delivery(&message, first_target.id, None)
            .unwrap()
    );
    assert!(
        store
            .insert_message_with_pending_delivery(
                &message,
                second_target.id,
                Some("second capsule"),
            )
            .unwrap()
    );

    assert_eq!(
        store.get_message(message.id).unwrap(),
        Some(message.clone())
    );
    for target_id in [first_target.id, second_target.id] {
        assert_eq!(
            store
                .get_message_delivery(message.id, target_id)
                .unwrap()
                .unwrap()
                .status,
            DeliveryStatus::Pending
        );
    }
    assert!(matches!(
        store.insert_message_with_pending_delivery(
            &message,
            second_target.id,
            Some("changed capsule"),
        ),
        Err(StoreError::DeliveryConflict {
            message_id,
            target_agent_id,
        }) if message_id == message.id && target_agent_id == second_target.id
    ));
}

#[test]
fn delivery_validation_rejects_cross_field_progress_mismatches() {
    let valid = MessageDelivery {
        message_id: MessageId::new(),
        target_agent_id: AgentId::new(),
        status: DeliveryStatus::Pending,
        capsule: None,
        capsule_delivered_at: None,
        created_at: NOW.into(),
        updated_at: NOW.into(),
        delivered_at: None,
    };

    assert_eq!(
        MessageDelivery {
            capsule_delivered_at: Some(LATER.into()),
            ..valid.clone()
        }
        .validate(),
        Err(DomainError::CapsuleDeliveryWithoutCapsule)
    );
    for invalid in [
        MessageDelivery {
            status: DeliveryStatus::Delivered,
            ..valid.clone()
        },
        MessageDelivery {
            delivered_at: Some(LATER.into()),
            ..valid.clone()
        },
        MessageDelivery {
            status: DeliveryStatus::Failed,
            delivered_at: Some(LATER.into()),
            ..valid
        },
    ] {
        assert_eq!(
            invalid.validate(),
            Err(DomainError::DeliveryTimestampStatusMismatch)
        );
    }
}

#[test]
fn invalid_target_rolls_back_the_message() {
    let database = TestDatabase::new();
    let mut store = SqliteStore::open(database.path()).unwrap();
    let target = agent("codex");
    store.insert_agent(&target).unwrap();
    let message = message(&mut store, &target);
    let missing_target = AgentId::new();

    assert!(
        store
            .insert_message_with_pending_delivery(&message, missing_target, None)
            .is_err()
    );
    assert_eq!(store.get_message(message.id).unwrap(), None);
    assert_eq!(
        store
            .get_message_delivery(message.id, missing_target)
            .unwrap(),
        None
    );
}

#[test]
fn guarded_delivery_transitions_preserve_terminal_and_capsule_progress() {
    let database = TestDatabase::new();
    let mut store = SqliteStore::open(database.path()).unwrap();
    let target = agent("codex");
    store.insert_agent(&target).unwrap();
    let message = message(&mut store, &target);
    store
        .insert_message_with_pending_delivery(&message, target.id, Some("thread capsule"))
        .unwrap();

    assert!(
        store
            .mark_delivery_capsule_delivered(message.id, target.id, LATER)
            .unwrap()
    );
    assert!(
        !store
            .mark_delivery_capsule_delivered(message.id, target.id, LATER)
            .unwrap()
    );
    assert!(
        store
            .mark_delivery_failed(message.id, target.id, LATER)
            .unwrap()
    );
    assert!(
        !store
            .mark_delivery_delivered(message.id, target.id, LATER)
            .unwrap()
    );
    assert!(
        store
            .claim_failed_delivery(message.id, target.id, LATER)
            .unwrap()
    );
    assert!(
        !store
            .claim_failed_delivery(message.id, target.id, LATER)
            .unwrap()
    );
    assert!(
        store
            .mark_delivery_delivered(message.id, target.id, LATER)
            .unwrap()
    );
    assert!(
        !store
            .mark_delivery_failed(message.id, target.id, LATER)
            .unwrap()
    );
    assert!(
        !store
            .claim_failed_delivery(message.id, target.id, LATER)
            .unwrap()
    );

    assert_eq!(
        store.get_message_delivery(message.id, target.id).unwrap(),
        Some(MessageDelivery {
            message_id: message.id,
            target_agent_id: target.id,
            status: DeliveryStatus::Delivered,
            capsule: Some("thread capsule".into()),
            capsule_delivered_at: Some(LATER.into()),
            created_at: NOW.into(),
            updated_at: LATER.into(),
            delivered_at: Some(LATER.into()),
        })
    );
}

#[test]
fn capsule_transition_is_a_noop_without_a_capsule() {
    let database = TestDatabase::new();
    let mut store = SqliteStore::open(database.path()).unwrap();
    let target = agent("codex");
    store.insert_agent(&target).unwrap();
    let message = message(&mut store, &target);
    store
        .insert_message_with_pending_delivery(&message, target.id, None)
        .unwrap();

    assert!(
        !store
            .mark_delivery_capsule_delivered(message.id, target.id, LATER)
            .unwrap()
    );
}

#[tokio::test]
async fn storage_worker_startup_reconciles_pending_deliveries_before_ready() {
    let database = TestDatabase::new();
    let mut store = SqliteStore::open(database.path()).unwrap();
    let target = agent("codex");
    store.insert_agent(&target).unwrap();

    let stranded_with_capsule = message(&mut store, &target);
    store
        .insert_message_with_pending_delivery(&stranded_with_capsule, target.id, Some("capsule"))
        .unwrap();
    store
        .mark_delivery_capsule_delivered(stranded_with_capsule.id, target.id, LATER)
        .unwrap();

    let stranded_without_capsule = message(&mut store, &target);
    store
        .insert_message_with_pending_delivery(&stranded_without_capsule, target.id, None)
        .unwrap();

    let failed = message(&mut store, &target);
    store
        .insert_message_with_pending_delivery(&failed, target.id, None)
        .unwrap();
    store
        .mark_delivery_failed(failed.id, target.id, LATER)
        .unwrap();
    let failed_before = store
        .get_message_delivery(failed.id, target.id)
        .unwrap()
        .unwrap();

    let delivered = message(&mut store, &target);
    store
        .insert_message_with_pending_delivery(&delivered, target.id, Some("delivered capsule"))
        .unwrap();
    store
        .mark_delivery_capsule_delivered(delivered.id, target.id, LATER)
        .unwrap();
    store
        .mark_delivery_delivered(delivered.id, target.id, LATER)
        .unwrap();
    let delivered_before = store
        .get_message_delivery(delivered.id, target.id)
        .unwrap()
        .unwrap();
    drop(store);

    let mut first_worker = StorageWorker::open(database.path()).unwrap();
    let store = SqliteStore::open(database.path()).unwrap();
    let stranded_with_capsule_after = store
        .get_message_delivery(stranded_with_capsule.id, target.id)
        .unwrap()
        .unwrap();
    let stranded_without_capsule_after = store
        .get_message_delivery(stranded_without_capsule.id, target.id)
        .unwrap()
        .unwrap();
    assert_eq!(stranded_with_capsule_after.status, DeliveryStatus::Failed);
    assert_eq!(
        stranded_without_capsule_after.status,
        DeliveryStatus::Failed
    );
    assert_ne!(stranded_with_capsule_after.updated_at, LATER);
    assert_eq!(
        stranded_with_capsule_after.updated_at,
        stranded_without_capsule_after.updated_at
    );
    assert_eq!(
        stranded_with_capsule_after.capsule.as_deref(),
        Some("capsule")
    );
    assert_eq!(
        stranded_with_capsule_after.capsule_delivered_at.as_deref(),
        Some(LATER)
    );
    assert_eq!(
        store.get_message_delivery(failed.id, target.id).unwrap(),
        Some(failed_before)
    );
    assert_eq!(
        store.get_message_delivery(delivered.id, target.id).unwrap(),
        Some(delivered_before)
    );
    let reconciled = (
        stranded_with_capsule_after.clone(),
        stranded_without_capsule_after.clone(),
    );
    drop(store);
    first_worker.shutdown().await.unwrap();

    let mut second_worker = StorageWorker::open(database.path()).unwrap();
    let store = SqliteStore::open(database.path()).unwrap();
    assert_eq!(
        store
            .get_message_delivery(stranded_with_capsule.id, target.id)
            .unwrap()
            .unwrap(),
        reconciled.0
    );
    assert_eq!(
        store
            .get_message_delivery(stranded_without_capsule.id, target.id)
            .unwrap()
            .unwrap(),
        reconciled.1
    );
    assert!(
        store
            .claim_failed_delivery(stranded_without_capsule.id, target.id, LATER)
            .unwrap()
    );
    drop(store);
    second_worker.shutdown().await.unwrap();
}
