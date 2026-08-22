use july_workspace::domain::{
    Agent, AgentId, SessionBinding, SessionBindingId, SessionBindingStatus, SessionRecovery,
};
use july_workspace::runtime::{RuntimeError, StorageWorker};
use july_workspace::storage::{SqliteStore, StoreError};
use rusqlite::Connection;
use serde_json::json;
use std::path::{Path, PathBuf};
use ulid::Ulid;

const CREATED: &str = "2026-08-22T10:00:00Z";
const REPLACED: &str = "2026-08-22T11:00:00Z";
const ATTACHED: &str = "2026-08-22T11:01:00Z";
const DELIVERED: &str = "2026-08-22T11:02:00Z";
const CAPSULE: &str = "{\"schema\":1,\"messages\":[\"byte-identical\"]}";

struct TestDatabase {
    directory: PathBuf,
    path: PathBuf,
}

impl TestDatabase {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("july-session-recovery-{}", Ulid::generate()));
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

fn seed_source(
    store: &mut SqliteStore,
    status: SessionBindingStatus,
    generation: u64,
) -> SessionBinding {
    let target = agent("codex");
    store.insert_agent(&target).unwrap();
    let conversation = store.get_or_create_dm("tony", target.id, CREATED).unwrap();
    let source = SessionBinding {
        id: SessionBindingId::new(),
        conversation_id: conversation.id,
        agent_id: target.id,
        transport_type: "acp".into(),
        remote_session_id: Some("remote-source".into()),
        generation,
        status,
        created_at: CREATED.into(),
        last_used_at: CREATED.into(),
    };
    store.insert_session_binding(&source).unwrap();
    source
}

fn expected_recovery(
    source_binding_id: SessionBindingId,
    replacement_binding_id: SessionBindingId,
) -> SessionRecovery {
    SessionRecovery {
        session_binding_id: replacement_binding_id,
        source_binding_id,
        capsule: CAPSULE.into(),
        capsule_delivered_at: None,
        created_at: REPLACED.into(),
    }
}

#[test]
fn active_disconnected_and_lost_sources_are_replaced_atomically() {
    for status in [
        SessionBindingStatus::Active,
        SessionBindingStatus::Disconnected,
        SessionBindingStatus::Lost,
    ] {
        let database = TestDatabase::new();
        let mut store = SqliteStore::open(database.path()).unwrap();
        let source = seed_source(&mut store, status, 7);
        let replacement_id = SessionBindingId::new();

        let (replacement, recovery) = store
            .begin_session_replacement(source.id, replacement_id, CAPSULE, REPLACED)
            .unwrap();

        assert_eq!(
            store.get_session_binding(source.id).unwrap().unwrap(),
            SessionBinding {
                status: SessionBindingStatus::Lost,
                last_used_at: REPLACED.into(),
                ..source.clone()
            }
        );
        assert_eq!(
            replacement,
            SessionBinding {
                id: replacement_id,
                conversation_id: source.conversation_id,
                agent_id: source.agent_id,
                transport_type: source.transport_type.clone(),
                remote_session_id: None,
                generation: 8,
                status: SessionBindingStatus::Disconnected,
                created_at: REPLACED.into(),
                last_used_at: REPLACED.into(),
            }
        );
        assert_eq!(recovery, expected_recovery(source.id, replacement_id));
        assert_eq!(
            store
                .get_current_session_binding(source.conversation_id, source.agent_id)
                .unwrap(),
            Some(replacement.clone())
        );
        assert_eq!(
            store.get_session_recovery(replacement_id).unwrap(),
            Some(recovery)
        );
    }
}

#[test]
fn begin_retry_is_exact_and_conflicts_or_invalid_sources_do_not_mutate_state() {
    let database = TestDatabase::new();
    let mut store = SqliteStore::open(database.path()).unwrap();
    let source = seed_source(&mut store, SessionBindingStatus::Active, 1);
    let replacement_id = SessionBindingId::new();
    let first = store
        .begin_session_replacement(source.id, replacement_id, CAPSULE, REPLACED)
        .unwrap();

    assert_eq!(
        store
            .begin_session_replacement(source.id, replacement_id, CAPSULE, REPLACED)
            .unwrap(),
        first
    );
    assert!(matches!(
        store.begin_session_replacement(source.id, replacement_id, "different", REPLACED),
        Err(StoreError::SessionReplacementConflict {
            source_binding_id,
            replacement_binding_id,
        }) if source_binding_id == source.id && replacement_binding_id == replacement_id
    ));
    assert_eq!(
        store.get_session_binding(replacement_id).unwrap(),
        Some(first.0)
    );
    assert_eq!(
        store.get_session_recovery(replacement_id).unwrap(),
        Some(first.1)
    );

    let stale_database = TestDatabase::new();
    let mut stale_store = SqliteStore::open(stale_database.path()).unwrap();
    let stale = seed_source(&mut stale_store, SessionBindingStatus::Lost, 1);
    let latest = SessionBinding {
        id: SessionBindingId::new(),
        generation: 2,
        status: SessionBindingStatus::Lost,
        ..stale.clone()
    };
    stale_store.insert_session_binding(&latest).unwrap();
    assert!(matches!(
        stale_store.begin_session_replacement(
            stale.id,
            SessionBindingId::new(),
            CAPSULE,
            REPLACED,
        ),
        Err(StoreError::SessionReplacementSourceStale {
            source_binding_id,
            latest_binding_id,
        }) if source_binding_id == stale.id && latest_binding_id == latest.id
    ));
    assert_eq!(
        stale_store.get_session_binding(stale.id).unwrap(),
        Some(stale)
    );
    assert_eq!(
        stale_store.get_session_binding(latest.id).unwrap(),
        Some(latest)
    );

    let closed_database = TestDatabase::new();
    let mut closed_store = SqliteStore::open(closed_database.path()).unwrap();
    let closed = seed_source(&mut closed_store, SessionBindingStatus::Closed, 1);
    assert!(matches!(
        closed_store.begin_session_replacement(
            closed.id,
            SessionBindingId::new(),
            CAPSULE,
            REPLACED,
        ),
        Err(StoreError::SessionReplacementSourceUnavailable {
            source_binding_id,
            status: SessionBindingStatus::Closed,
        }) if source_binding_id == closed.id
    ));
    assert_eq!(
        closed_store.get_session_binding(closed.id).unwrap(),
        Some(closed)
    );

    let missing_id = SessionBindingId::new();
    assert!(matches!(
        closed_store.begin_session_replacement(
            missing_id,
            SessionBindingId::new(),
            CAPSULE,
            REPLACED,
        ),
        Err(StoreError::SessionReplacementSourceNotFound(id)) if id == missing_id
    ));
}

#[test]
fn generation_u32_max_is_typed_exhaustion_without_mutation() {
    let database = TestDatabase::new();
    let mut store = SqliteStore::open(database.path()).unwrap();
    let source = seed_source(
        &mut store,
        SessionBindingStatus::Active,
        u64::from(u32::MAX),
    );
    let replacement_id = SessionBindingId::new();

    assert!(matches!(
        store.begin_session_replacement(source.id, replacement_id, CAPSULE, REPLACED),
        Err(StoreError::SessionReplacementGenerationExhausted(id)) if id == source.id
    ));
    assert_eq!(store.get_session_binding(source.id).unwrap(), Some(source));
    assert_eq!(store.get_session_binding(replacement_id).unwrap(), None);
    assert_eq!(store.get_session_recovery(replacement_id).unwrap(), None);
}

#[test]
fn forced_replacement_or_recovery_insert_failure_rolls_back_every_change() {
    for fail_recovery in [false, true] {
        let database = TestDatabase::new();
        let mut store = SqliteStore::open(database.path()).unwrap();
        let source = seed_source(&mut store, SessionBindingStatus::Active, 1);
        let replacement_id = SessionBindingId::new();
        let connection = Connection::open(database.path()).unwrap();
        let sql = if fail_recovery {
            "CREATE TRIGGER fail_recovery_insert
             BEFORE INSERT ON session_recoveries BEGIN
                 SELECT RAISE(ABORT, 'forced recovery failure');
             END;"
                .into()
        } else {
            format!(
                "CREATE TRIGGER fail_replacement_insert
                 BEFORE INSERT ON session_bindings
                 WHEN NEW.id = '{}'
                 BEGIN SELECT RAISE(ABORT, 'forced replacement failure'); END;",
                replacement_id
            )
        };
        connection.execute_batch(&sql).unwrap();
        drop(connection);

        assert!(
            store
                .begin_session_replacement(source.id, replacement_id, CAPSULE, REPLACED)
                .is_err()
        );
        assert_eq!(store.get_session_binding(source.id).unwrap(), Some(source));
        assert_eq!(store.get_session_binding(replacement_id).unwrap(), None);
        assert_eq!(store.get_session_recovery(replacement_id).unwrap(), None);
    }
}

#[tokio::test]
async fn attach_and_delivery_are_bounded_idempotent_and_survive_restart() {
    let database = TestDatabase::new();
    let source = {
        let mut store = SqliteStore::open(database.path()).unwrap();
        seed_source(&mut store, SessionBindingStatus::Disconnected, 3)
    };
    let replacement_id = SessionBindingId::new();
    let mut worker = StorageWorker::open(database.path()).unwrap();
    let (replacement, recovery) = worker
        .begin_session_replacement(source.id, replacement_id, CAPSULE.into(), REPLACED.into())
        .await
        .unwrap();

    assert!(matches!(
        worker
            .mark_session_recovery_capsule_delivered(replacement_id, DELIVERED.into())
            .await,
        Err(RuntimeError::Storage(StoreError::SessionRecoveryNotAttached(id)))
            if id == replacement_id
    ));
    assert!(matches!(
        worker
            .attach_replacement_remote_session(source.id, "wrong".into(), ATTACHED.into())
            .await,
        Err(RuntimeError::Storage(StoreError::SessionRecoveryNotFound(id))) if id == source.id
    ));

    let attached = worker
        .attach_replacement_remote_session(
            replacement_id,
            "remote-replacement".into(),
            ATTACHED.into(),
        )
        .await
        .unwrap();
    assert_eq!(
        attached,
        SessionBinding {
            remote_session_id: Some("remote-replacement".into()),
            status: SessionBindingStatus::Active,
            last_used_at: ATTACHED.into(),
            ..replacement
        }
    );
    assert_eq!(
        worker
            .attach_replacement_remote_session(
                replacement_id,
                "remote-replacement".into(),
                "later-ignored".into(),
            )
            .await
            .unwrap(),
        attached
    );
    assert!(matches!(
        worker
            .attach_replacement_remote_session(
                replacement_id,
                "remote-conflict".into(),
                "later".into(),
            )
            .await,
        Err(RuntimeError::Storage(
            StoreError::SessionRecoveryRemoteAttachmentConflict(id)
        )) if id == replacement_id
    ));

    assert!(
        worker
            .mark_session_recovery_capsule_delivered(replacement_id, DELIVERED.into())
            .await
            .unwrap()
    );
    assert!(
        !worker
            .mark_session_recovery_capsule_delivered(replacement_id, "later-ignored".into())
            .await
            .unwrap()
    );
    let delivered = worker
        .get_session_recovery(replacement_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(delivered.capsule, recovery.capsule);
    assert_eq!(delivered.capsule_delivered_at.as_deref(), Some(DELIVERED));
    worker.shutdown().await.unwrap();

    let mut restarted = StorageWorker::open(database.path()).unwrap();
    assert_eq!(
        restarted
            .get_session_recovery(replacement_id)
            .await
            .unwrap(),
        Some(delivered)
    );
    restarted.shutdown().await.unwrap();
}
