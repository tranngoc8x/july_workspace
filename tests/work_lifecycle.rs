use july_workspace::application::{AssignWorkOwner, TransitionWork, WorkError, WorkService};
use july_workspace::domain::{
    Agent, AgentId, Conversation, ConversationId, ConversationKind, DomainError, Room, RoomId,
    WorkItem, WorkItemId, WorkStatus,
};
use july_workspace::runtime::StorageWorker;
use july_workspace::storage::SqliteStore;
use serde_json::json;
use std::path::{Path, PathBuf};

const CREATED: &str = "2026-08-22T08:00:00Z";
const CHANGED: &str = "2026-08-22T09:00:00Z";
const RETRIED: &str = "2026-08-22T10:00:00Z";

struct TestDatabase {
    directory: PathBuf,
    path: PathBuf,
}

impl TestDatabase {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("july-work-lifecycle-{}", ulid::Ulid::generate()));
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
        transport_config: json!({}),
        status: "active".into(),
        metadata: json!({}),
        created_at: CREATED.into(),
        updated_at: CREATED.into(),
    }
}

fn room() -> Room {
    Room {
        id: RoomId::new(),
        name: "Phase 6".into(),
        description: None,
        status: "active".into(),
        created_at: CREATED.into(),
        updated_at: CREATED.into(),
    }
}

fn thread(room_id: RoomId) -> Conversation {
    Conversation {
        id: ConversationId::new(),
        kind: ConversationKind::Thread,
        room_id: Some(room_id),
        title: Some("Work lifecycle".into()),
        goal: Some("Lock deterministic transitions".into()),
        parent_conversation_id: None,
        origin_conversation_id: None,
        status: "open".into(),
        created_at: CREATED.into(),
        updated_at: CREATED.into(),
    }
}

struct Seeded {
    primary_work_id: WorkItemId,
    secondary_work_id: WorkItemId,
    member_id: AgentId,
    replacement_id: AgentId,
    inactive_id: AgentId,
    outsider_id: AgentId,
}

fn seed(path: &Path) -> Seeded {
    let mut store = SqliteStore::open(path).unwrap();
    let room = room();
    let conversation = thread(room.id);
    let member = agent("member");
    let replacement = agent("replacement");
    let mut inactive = agent("inactive");
    let outsider = agent("outsider");
    store.insert_room(&room).unwrap();
    for agent in [&member, &replacement, &inactive, &outsider] {
        store.insert_agent(agent).unwrap();
        store
            .add_room_member(room.id, agent.id, None, CREATED)
            .unwrap();
    }
    let primary_work_id = WorkItemId::new();
    store
        .create_thread_with_primary_work(
            &conversation,
            primary_work_id,
            "tony",
            &[member.id, replacement.id, inactive.id],
        )
        .unwrap();
    inactive.status = "inactive".into();
    inactive.updated_at = CHANGED.into();
    store.update_agent(&inactive).unwrap();

    let secondary_work_id = WorkItemId::new();
    store
        .insert_work_item(&WorkItem {
            id: secondary_work_id,
            conversation_id: conversation.id,
            title: "Secondary work".into(),
            goal: None,
            status: WorkStatus::Open,
            owner_agent_id: None,
            is_primary: false,
            created_at: CREATED.into(),
            updated_at: CREATED.into(),
            completed_at: None,
        })
        .unwrap();

    Seeded {
        primary_work_id,
        secondary_work_id,
        member_id: member.id,
        replacement_id: replacement.id,
        inactive_id: inactive.id,
        outsider_id: outsider.id,
    }
}

fn insert_work(path: &Path, conversation_id: ConversationId, status: WorkStatus) -> WorkItem {
    let work = WorkItem {
        id: WorkItemId::new(),
        conversation_id,
        title: format!("{status} work"),
        goal: None,
        status,
        owner_agent_id: None,
        is_primary: false,
        created_at: CREATED.into(),
        updated_at: CREATED.into(),
        completed_at: status.is_terminal().then(|| CREATED.to_owned()),
    };
    SqliteStore::open(path)
        .unwrap()
        .insert_work_item(&work)
        .unwrap();
    work
}

fn read_work(path: &Path, id: WorkItemId) -> WorkItem {
    SqliteStore::open(path)
        .unwrap()
        .get_work_item(id)
        .unwrap()
        .unwrap()
}

#[test]
fn phase4_primary_work_remains_open_and_unowned() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());

    let work = read_work(database.path(), seeded.primary_work_id);
    assert!(work.is_primary);
    assert_eq!(work.status, WorkStatus::Open);
    assert_eq!(work.owner_agent_id, None);
    assert_eq!(work.updated_at, CREATED);
    assert_eq!(work.completed_at, None);
}

#[test]
fn work_validation_requires_completed_at_exactly_for_terminal_status() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut work = read_work(database.path(), seeded.primary_work_id);

    work.status = WorkStatus::Done;
    assert_eq!(
        work.validate(),
        Err(DomainError::WorkCompletionTimestampMismatch)
    );

    work.status = WorkStatus::Working;
    work.completed_at = Some(CHANGED.into());
    assert_eq!(
        work.validate(),
        Err(DomainError::WorkCompletionTimestampMismatch)
    );
}

#[tokio::test]
async fn assigns_and_replaces_owner_for_primary_and_non_primary_work_with_exact_retry() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut service = WorkService::new(StorageWorker::open(database.path()).unwrap());

    for work_id in [seeded.primary_work_id, seeded.secondary_work_id] {
        let assigned = service
            .assign_owner(AssignWorkOwner {
                work_id,
                owner_agent_id: seeded.member_id,
                assigned_at: CHANGED.into(),
            })
            .await
            .unwrap();
        assert_eq!(assigned.owner_agent_id, Some(seeded.member_id));
        assert_eq!(assigned.updated_at, CHANGED);

        let retried = service
            .assign_owner(AssignWorkOwner {
                work_id,
                owner_agent_id: seeded.member_id,
                assigned_at: RETRIED.into(),
            })
            .await
            .unwrap();
        assert_eq!(retried, assigned);

        let replaced = service
            .assign_owner(AssignWorkOwner {
                work_id,
                owner_agent_id: seeded.replacement_id,
                assigned_at: RETRIED.into(),
            })
            .await
            .unwrap();
        assert_eq!(replaced.owner_agent_id, Some(seeded.replacement_id));
        assert_eq!(replaced.updated_at, RETRIED);
    }
}

#[tokio::test]
async fn owner_validation_and_terminal_immutability_are_side_effect_free() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let work_id = seeded.primary_work_id;
    let missing_id = AgentId::new();
    let mut service = WorkService::new(StorageWorker::open(database.path()).unwrap());
    let assigned = service
        .assign_owner(AssignWorkOwner {
            work_id,
            owner_agent_id: seeded.member_id,
            assigned_at: CHANGED.into(),
        })
        .await
        .unwrap();

    let rejected = [
        (
            seeded.outsider_id,
            WorkError::OwnerOutOfScope {
                work_id,
                owner_agent_id: seeded.outsider_id,
            },
        ),
        (
            seeded.inactive_id,
            WorkError::OwnerInactive(seeded.inactive_id),
        ),
        (missing_id, WorkError::OwnerNotFound(missing_id)),
    ];
    for (owner_agent_id, expected) in rejected {
        assert_eq!(
            service
                .assign_owner(AssignWorkOwner {
                    work_id,
                    owner_agent_id,
                    assigned_at: RETRIED.into(),
                })
                .await,
            Err(expected)
        );
        assert_eq!(read_work(database.path(), work_id), assigned);
    }

    assert_eq!(
        service
            .assign_owner(AssignWorkOwner {
                work_id,
                owner_agent_id: seeded.replacement_id,
                assigned_at: "  ".into(),
            })
            .await,
        Err(WorkError::InvalidTimestamp)
    );
    assert_eq!(read_work(database.path(), work_id), assigned);

    let terminal = service
        .transition(TransitionWork {
            work_id,
            target: WorkStatus::Cancelled,
            transitioned_at: RETRIED.into(),
        })
        .await
        .unwrap();
    let exact_owner_retry = service
        .assign_owner(AssignWorkOwner {
            work_id,
            owner_agent_id: seeded.member_id,
            assigned_at: "2026-08-22T11:00:00Z".into(),
        })
        .await
        .unwrap();
    assert_eq!(exact_owner_retry, terminal);
    assert_eq!(
        service
            .assign_owner(AssignWorkOwner {
                work_id,
                owner_agent_id: seeded.replacement_id,
                assigned_at: "2026-08-22T11:00:00Z".into(),
            })
            .await,
        Err(WorkError::TerminalOwnershipImmutable(work_id))
    );
    assert_eq!(read_work(database.path(), work_id), terminal);
}

#[tokio::test]
async fn every_directly_allowed_transition_persists_terminal_semantics_and_exact_retry_is_unchanged()
 {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let conversation_id = read_work(database.path(), seeded.primary_work_id).conversation_id;
    let allowed = [
        (WorkStatus::Open, WorkStatus::Working),
        (WorkStatus::Open, WorkStatus::Blocked),
        (WorkStatus::Open, WorkStatus::Cancelled),
        (WorkStatus::Working, WorkStatus::Blocked),
        (WorkStatus::Working, WorkStatus::Failed),
        (WorkStatus::Working, WorkStatus::Cancelled),
        (WorkStatus::Blocked, WorkStatus::Working),
        (WorkStatus::Blocked, WorkStatus::Failed),
        (WorkStatus::Blocked, WorkStatus::Cancelled),
        (WorkStatus::Ready, WorkStatus::Done),
    ];
    let works: Vec<_> = allowed
        .iter()
        .map(|(from, _)| insert_work(database.path(), conversation_id, *from))
        .collect();
    let mut service = WorkService::new(StorageWorker::open(database.path()).unwrap());

    for (work, (_, target)) in works.into_iter().zip(allowed) {
        let transitioned = service
            .transition(TransitionWork {
                work_id: work.id,
                target,
                transitioned_at: CHANGED.into(),
            })
            .await
            .unwrap();
        assert_eq!(transitioned.status, target);
        assert_eq!(transitioned.updated_at, CHANGED);
        assert_eq!(
            transitioned.completed_at.as_deref(),
            target.is_terminal().then_some(CHANGED)
        );

        let retried = service
            .transition(TransitionWork {
                work_id: work.id,
                target,
                transitioned_at: RETRIED.into(),
            })
            .await
            .unwrap();
        assert_eq!(retried, transitioned);
    }
}

#[tokio::test]
async fn direct_ready_transition_is_rejected_until_result_creation_can_be_atomic() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let conversation_id = read_work(database.path(), seeded.primary_work_id).conversation_id;
    let working = insert_work(database.path(), conversation_id, WorkStatus::Working);
    let mut service = WorkService::new(StorageWorker::open(database.path()).unwrap());

    assert_eq!(
        service
            .transition(TransitionWork {
                work_id: working.id,
                target: WorkStatus::Ready,
                transitioned_at: CHANGED.into(),
            })
            .await,
        Err(WorkError::InvalidTransition {
            work_id: working.id,
            from: WorkStatus::Working,
            to: WorkStatus::Ready,
        })
    );
    assert_eq!(read_work(database.path(), working.id), working);
}

#[tokio::test]
async fn invalid_transition_and_timestamp_leave_work_unchanged() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let conversation_id = read_work(database.path(), seeded.primary_work_id).conversation_id;
    let invalid = [
        (WorkStatus::Open, WorkStatus::Ready),
        (WorkStatus::Working, WorkStatus::Done),
        (WorkStatus::Blocked, WorkStatus::Ready),
        (WorkStatus::Ready, WorkStatus::Working),
        (WorkStatus::Done, WorkStatus::Failed),
        (WorkStatus::Failed, WorkStatus::Working),
        (WorkStatus::Cancelled, WorkStatus::Open),
    ];
    let works: Vec<_> = invalid
        .iter()
        .map(|(from, _)| insert_work(database.path(), conversation_id, *from))
        .collect();
    let mut service = WorkService::new(StorageWorker::open(database.path()).unwrap());

    for (work, (from, target)) in works.iter().zip(invalid) {
        assert_eq!(
            service
                .transition(TransitionWork {
                    work_id: work.id,
                    target,
                    transitioned_at: CHANGED.into(),
                })
                .await,
            Err(WorkError::InvalidTransition {
                work_id: work.id,
                from,
                to: target,
            })
        );
        assert_eq!(read_work(database.path(), work.id), *work);
    }

    let open = insert_work(database.path(), conversation_id, WorkStatus::Open);
    assert_eq!(
        service
            .transition(TransitionWork {
                work_id: open.id,
                target: WorkStatus::Working,
                transitioned_at: "".into(),
            })
            .await,
        Err(WorkError::InvalidTimestamp)
    );
    assert_eq!(read_work(database.path(), open.id), open);
}
