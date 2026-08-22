use july_workspace::application::{
    AddWorkDependency, CreateWorkResult, DependencyError, DependencyService, TransitionWork,
    WorkError, WorkService,
};
use july_workspace::domain::{
    Conversation, ConversationId, ConversationKind, DependencyStatus, MemberType, Message,
    MessageId, ResultId, WorkDependency, WorkItem, WorkItemId, WorkResult, WorkStatus,
};
use july_workspace::runtime::StorageWorker;
use july_workspace::storage::SqliteStore;
use std::path::{Path, PathBuf};

const CREATED: &str = "2026-08-22T08:00:00Z";
const READY_AT: &str = "2026-08-22T09:00:00Z";
const FAILED_AT: &str = "2026-08-22T10:00:00Z";
const CORRECTED_AT: &str = "2026-08-22T11:00:00Z";

struct TestDatabase {
    directory: PathBuf,
    path: PathBuf,
}

impl TestDatabase {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("july-work-dependencies-{}", ulid::Ulid::generate()));
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

fn seed_work(store: &SqliteStore, title: &str) -> WorkItem {
    seed_work_with_status(store, title, WorkStatus::Open)
}

fn seed_work_with_status(store: &SqliteStore, title: &str, status: WorkStatus) -> WorkItem {
    let conversation = Conversation {
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
    };
    let work = WorkItem {
        id: WorkItemId::new(),
        conversation_id: conversation.id,
        title: title.into(),
        goal: None,
        status,
        owner_agent_id: None,
        is_primary: false,
        created_at: CREATED.into(),
        updated_at: CREATED.into(),
        completed_at: status.is_terminal().then(|| CREATED.into()),
    };
    store.insert_conversation(&conversation).unwrap();
    store.insert_work_item(&work).unwrap();
    work
}

fn work_result(
    work_id: WorkItemId,
    id: ResultId,
    supersedes_result_id: Option<ResultId>,
    created_at: &str,
) -> WorkResult {
    WorkResult {
        id,
        work_id,
        status: "accepted".into(),
        summary: format!("Result {id}"),
        outputs: vec![format!("artifact://{id}")],
        evidence: vec!["cargo test --test work_dependencies".into()],
        supersedes_result_id,
        created_at: created_at.into(),
    }
}

fn dependency(upstream_work_id: WorkItemId, downstream_work_id: WorkItemId) -> AddWorkDependency {
    AddWorkDependency {
        upstream_work_id,
        downstream_work_id,
        created_at: CREATED.into(),
    }
}

fn read_dependency(
    path: &Path,
    upstream_work_id: WorkItemId,
    downstream_work_id: WorkItemId,
) -> Option<WorkDependency> {
    SqliteStore::open(path)
        .unwrap()
        .get_work_dependency(upstream_work_id, downstream_work_id)
        .unwrap()
}

fn read_work(path: &Path, id: WorkItemId) -> WorkItem {
    SqliteStore::open(path)
        .unwrap()
        .get_work_item(id)
        .unwrap()
        .unwrap()
}

fn read_result(path: &Path, id: ResultId) -> Option<WorkResult> {
    SqliteStore::open(path)
        .unwrap()
        .get_work_result(id)
        .unwrap()
}

async fn add_dependency(path: &Path, upstream: WorkItemId, downstream: WorkItemId) {
    DependencyService::new(StorageWorker::open(path).unwrap())
        .add(dependency(upstream, downstream))
        .await
        .unwrap();
}

fn install_dependency_transition_failure(path: &Path, status: &str) {
    let connection = rusqlite::Connection::open(path).unwrap();
    connection
        .execute_batch(&format!(
            "CREATE TRIGGER reject_dependency_{status}
             BEFORE UPDATE OF status ON work_dependencies
             WHEN NEW.status = '{status}'
             BEGIN
                 SELECT RAISE(ABORT, 'forced dependency {status} failure');
             END;"
        ))
        .unwrap();
}

#[tokio::test]
async fn add_starts_waiting_and_exact_retry_lists_one_downstream_outcome() {
    let database = TestDatabase::new();
    let (upstream, downstream) = {
        let store = SqliteStore::open(database.path()).unwrap();
        (
            seed_work(&store, "Prerequisite"),
            seed_work(&store, "Consumer"),
        )
    };
    let command = dependency(upstream.id, downstream.id);
    let mut service = DependencyService::new(StorageWorker::open(database.path()).unwrap());

    let added = service.add(command.clone()).await.unwrap();

    assert_eq!(added.status, DependencyStatus::Waiting);
    assert_eq!(added.result_id, None);
    assert_eq!(service.add(command.clone()).await.unwrap(), added);
    let mut conflicting = command;
    conflicting.created_at = READY_AT.into();
    assert_eq!(
        service.add(conflicting).await,
        Err(DependencyError::Conflict {
            upstream_work_id: upstream.id,
            downstream_work_id: downstream.id,
        })
    );
    assert_eq!(
        service.list_for_downstream(downstream.id).await.unwrap(),
        vec![added.clone()]
    );
    assert_eq!(
        read_dependency(database.path(), upstream.id, downstream.id),
        Some(added)
    );
}

#[tokio::test]
async fn add_rejects_missing_self_and_cyclic_edges_without_partial_rows() {
    let database = TestDatabase::new();
    let (first, second, third) = {
        let store = SqliteStore::open(database.path()).unwrap();
        (
            seed_work(&store, "First"),
            seed_work(&store, "Second"),
            seed_work(&store, "Third"),
        )
    };
    let missing = WorkItemId::new();
    let mut service = DependencyService::new(StorageWorker::open(database.path()).unwrap());

    assert_eq!(
        service.add(dependency(missing, first.id)).await,
        Err(DependencyError::WorkNotFound(missing))
    );
    assert_eq!(
        service.add(dependency(first.id, missing)).await,
        Err(DependencyError::WorkNotFound(missing))
    );
    assert_eq!(
        service.list_for_downstream(missing).await,
        Err(DependencyError::WorkNotFound(missing))
    );
    assert_eq!(
        service.add(dependency(first.id, first.id)).await,
        Err(DependencyError::SelfDependency(first.id))
    );

    service.add(dependency(first.id, second.id)).await.unwrap();
    service.add(dependency(second.id, third.id)).await.unwrap();
    assert_eq!(
        service.add(dependency(third.id, first.id)).await,
        Err(DependencyError::Cycle {
            upstream_work_id: third.id,
            downstream_work_id: first.id,
        })
    );

    assert_eq!(
        service.list_for_downstream(first.id).await.unwrap(),
        Vec::<WorkDependency>::new()
    );
}

#[tokio::test]
async fn ready_satisfies_only_outgoing_edges_with_result_reference_without_consumer_or_message_changes()
 {
    let database = TestDatabase::new();
    let (upstream, downstream, unrelated_upstream, unrelated_downstream, messages) = {
        let store = SqliteStore::open(database.path()).unwrap();
        let upstream = seed_work_with_status(&store, "Prerequisite", WorkStatus::Working);
        let downstream = seed_work_with_status(&store, "Consumer", WorkStatus::Blocked);
        let unrelated_upstream =
            seed_work_with_status(&store, "Other prerequisite", WorkStatus::Working);
        let unrelated_downstream =
            seed_work_with_status(&store, "Other consumer", WorkStatus::Blocked);
        let messages = [
            Message {
                id: MessageId::new(),
                conversation_id: downstream.conversation_id,
                sender_type: MemberType::User,
                sender_id: "tony".into(),
                body: "consumer transcript".into(),
                reply_to: None,
                metadata: serde_json::Value::Null,
                created_at: CREATED.into(),
            },
            Message {
                id: MessageId::new(),
                conversation_id: unrelated_downstream.conversation_id,
                sender_type: MemberType::User,
                sender_id: "tony".into(),
                body: "unrelated transcript".into(),
                reply_to: None,
                metadata: serde_json::Value::Null,
                created_at: CREATED.into(),
            },
        ];
        for message in &messages {
            store.insert_message(message).unwrap();
        }
        (
            upstream,
            downstream,
            unrelated_upstream,
            unrelated_downstream,
            messages,
        )
    };
    add_dependency(database.path(), upstream.id, downstream.id).await;
    add_dependency(
        database.path(),
        unrelated_upstream.id,
        unrelated_downstream.id,
    )
    .await;
    add_dependency(database.path(), unrelated_upstream.id, upstream.id).await;
    let result = work_result(upstream.id, ResultId::new(), None, READY_AT);
    let mut work_service = WorkService::new(StorageWorker::open(database.path()).unwrap());

    assert_eq!(
        work_service
            .create_result(CreateWorkResult {
                result: result.clone(),
            })
            .await
            .unwrap(),
        result
    );

    let satisfied = read_dependency(database.path(), upstream.id, downstream.id).unwrap();
    assert_eq!(satisfied.status, DependencyStatus::Satisfied);
    assert_eq!(satisfied.result_id, Some(result.id));
    assert_eq!(read_work(database.path(), downstream.id), downstream);
    for (from, to) in [
        (unrelated_upstream.id, unrelated_downstream.id),
        (unrelated_upstream.id, upstream.id),
    ] {
        let untouched = read_dependency(database.path(), from, to).unwrap();
        assert_eq!(untouched.status, DependencyStatus::Waiting);
        assert_eq!(untouched.result_id, None);
    }
    let store = SqliteStore::open(database.path()).unwrap();
    assert_eq!(
        store.list_messages(downstream.conversation_id).unwrap(),
        vec![messages[0].clone()]
    );
    assert_eq!(
        store
            .list_messages(unrelated_downstream.conversation_id)
            .unwrap(),
        vec![messages[1].clone()]
    );
    assert_eq!(
        rusqlite::Connection::open(database.path())
            .unwrap()
            .query_row("SELECT COUNT(*) FROM publishes", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(store);

    assert_eq!(
        work_service
            .create_result(CreateWorkResult {
                result: result.clone(),
            })
            .await
            .unwrap(),
        result
    );
    assert_eq!(
        read_dependency(database.path(), upstream.id, downstream.id),
        Some(satisfied.clone())
    );
    drop(work_service);
    let mut dependency_service =
        DependencyService::new(StorageWorker::open(database.path()).unwrap());
    assert_eq!(
        dependency_service
            .add(dependency(upstream.id, downstream.id))
            .await
            .unwrap(),
        satisfied
    );
    assert_eq!(
        dependency_service
            .list_for_downstream(downstream.id)
            .await
            .unwrap(),
        vec![satisfied]
    );
}

#[tokio::test]
async fn failed_work_marks_only_waiting_outgoing_edges_failed_and_exact_retry_is_a_noop() {
    let database = TestDatabase::new();
    let (upstream, downstream, unrelated_upstream, unrelated_downstream) = {
        let store = SqliteStore::open(database.path()).unwrap();
        (
            seed_work_with_status(&store, "Prerequisite", WorkStatus::Working),
            seed_work_with_status(&store, "Consumer", WorkStatus::Blocked),
            seed_work_with_status(&store, "Other prerequisite", WorkStatus::Working),
            seed_work_with_status(&store, "Other consumer", WorkStatus::Blocked),
        )
    };
    add_dependency(database.path(), upstream.id, downstream.id).await;
    add_dependency(
        database.path(),
        unrelated_upstream.id,
        unrelated_downstream.id,
    )
    .await;
    let mut service = WorkService::new(StorageWorker::open(database.path()).unwrap());
    let command = TransitionWork {
        work_id: upstream.id,
        target: WorkStatus::Failed,
        transitioned_at: FAILED_AT.into(),
    };

    let failed = service.transition(command.clone()).await.unwrap();

    let dependency = read_dependency(database.path(), upstream.id, downstream.id).unwrap();
    assert_eq!(dependency.status, DependencyStatus::Failed);
    assert_eq!(dependency.result_id, None);
    assert_eq!(read_work(database.path(), downstream.id), downstream);
    assert_eq!(
        read_dependency(
            database.path(),
            unrelated_upstream.id,
            unrelated_downstream.id,
        )
        .unwrap()
        .status,
        DependencyStatus::Waiting
    );
    assert_eq!(service.transition(command).await.unwrap(), failed);
    assert_eq!(
        read_dependency(database.path(), upstream.id, downstream.id),
        Some(dependency)
    );
}

#[tokio::test]
async fn correction_supersedes_only_satisfied_edges_and_new_edges_still_start_waiting() {
    let database = TestDatabase::new();
    let (upstream, first_downstream, later_downstream) = {
        let store = SqliteStore::open(database.path()).unwrap();
        (
            seed_work_with_status(&store, "Prerequisite", WorkStatus::Working),
            seed_work_with_status(&store, "First consumer", WorkStatus::Blocked),
            seed_work_with_status(&store, "Later consumer", WorkStatus::Blocked),
        )
    };
    add_dependency(database.path(), upstream.id, first_downstream.id).await;
    let first = work_result(upstream.id, ResultId::new(), None, READY_AT);
    let correction = work_result(upstream.id, ResultId::new(), Some(first.id), CORRECTED_AT);
    let mut service = WorkService::new(StorageWorker::open(database.path()).unwrap());
    service
        .create_result(CreateWorkResult {
            result: first.clone(),
        })
        .await
        .unwrap();
    drop(service);
    add_dependency(database.path(), upstream.id, later_downstream.id).await;
    let mut service = WorkService::new(StorageWorker::open(database.path()).unwrap());

    service
        .create_result(CreateWorkResult {
            result: correction.clone(),
        })
        .await
        .unwrap();

    let superseded = read_dependency(database.path(), upstream.id, first_downstream.id).unwrap();
    assert_eq!(superseded.status, DependencyStatus::Superseded);
    assert_eq!(superseded.result_id, Some(correction.id));
    let waiting = read_dependency(database.path(), upstream.id, later_downstream.id).unwrap();
    assert_eq!(waiting.status, DependencyStatus::Waiting);
    assert_eq!(waiting.result_id, None);
    assert_eq!(
        service
            .create_result(CreateWorkResult {
                result: correction.clone(),
            })
            .await
            .unwrap(),
        correction
    );
    assert_eq!(
        read_dependency(database.path(), upstream.id, first_downstream.id),
        Some(superseded)
    );
}

#[tokio::test]
async fn satisfied_dependency_failure_rolls_back_work_result_and_edge() {
    let database = TestDatabase::new();
    let (upstream, downstream) = {
        let store = SqliteStore::open(database.path()).unwrap();
        (
            seed_work_with_status(&store, "Prerequisite", WorkStatus::Working),
            seed_work_with_status(&store, "Consumer", WorkStatus::Blocked),
        )
    };
    add_dependency(database.path(), upstream.id, downstream.id).await;
    install_dependency_transition_failure(database.path(), "satisfied");
    let result = work_result(upstream.id, ResultId::new(), None, READY_AT);
    let mut service = WorkService::new(StorageWorker::open(database.path()).unwrap());

    assert!(matches!(
        service
            .create_result(CreateWorkResult {
                result: result.clone(),
            })
            .await,
        Err(WorkError::Runtime(message)) if message.contains("forced dependency satisfied failure")
    ));
    assert_eq!(read_work(database.path(), upstream.id), upstream);
    assert_eq!(read_result(database.path(), result.id), None);
    let dependency = read_dependency(database.path(), upstream.id, downstream.id).unwrap();
    assert_eq!(dependency.status, DependencyStatus::Waiting);
    assert_eq!(dependency.result_id, None);
}

#[tokio::test]
async fn failed_dependency_failure_rolls_back_work_and_edge() {
    let database = TestDatabase::new();
    let (upstream, downstream) = {
        let store = SqliteStore::open(database.path()).unwrap();
        (
            seed_work_with_status(&store, "Prerequisite", WorkStatus::Working),
            seed_work_with_status(&store, "Consumer", WorkStatus::Blocked),
        )
    };
    add_dependency(database.path(), upstream.id, downstream.id).await;
    install_dependency_transition_failure(database.path(), "failed");
    let mut service = WorkService::new(StorageWorker::open(database.path()).unwrap());

    assert!(matches!(
        service
            .transition(TransitionWork {
                work_id: upstream.id,
                target: WorkStatus::Failed,
                transitioned_at: FAILED_AT.into(),
            })
            .await,
        Err(WorkError::Runtime(message)) if message.contains("forced dependency failed failure")
    ));
    assert_eq!(read_work(database.path(), upstream.id), upstream);
    let dependency = read_dependency(database.path(), upstream.id, downstream.id).unwrap();
    assert_eq!(dependency.status, DependencyStatus::Waiting);
    assert_eq!(dependency.result_id, None);
}

#[tokio::test]
async fn superseded_dependency_failure_rolls_back_replacement_result_and_edge() {
    let database = TestDatabase::new();
    let (upstream, downstream) = {
        let store = SqliteStore::open(database.path()).unwrap();
        (
            seed_work_with_status(&store, "Prerequisite", WorkStatus::Working),
            seed_work_with_status(&store, "Consumer", WorkStatus::Blocked),
        )
    };
    add_dependency(database.path(), upstream.id, downstream.id).await;
    let first = work_result(upstream.id, ResultId::new(), None, READY_AT);
    let correction = work_result(upstream.id, ResultId::new(), Some(first.id), CORRECTED_AT);
    let mut service = WorkService::new(StorageWorker::open(database.path()).unwrap());
    service
        .create_result(CreateWorkResult {
            result: first.clone(),
        })
        .await
        .unwrap();
    drop(service);
    install_dependency_transition_failure(database.path(), "superseded");
    let mut service = WorkService::new(StorageWorker::open(database.path()).unwrap());

    assert!(matches!(
        service
            .create_result(CreateWorkResult {
                result: correction.clone(),
            })
            .await,
        Err(WorkError::Runtime(message)) if message.contains("forced dependency superseded failure")
    ));
    assert_eq!(read_result(database.path(), correction.id), None);
    let dependency = read_dependency(database.path(), upstream.id, downstream.id).unwrap();
    assert_eq!(dependency.status, DependencyStatus::Satisfied);
    assert_eq!(dependency.result_id, Some(first.id));
}
