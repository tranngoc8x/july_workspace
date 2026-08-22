use july_workspace::domain::{
    Agent, AgentId, DeliveryStatus, DomainError, MemberType, Message, MessageDelivery, MessageId,
};
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
