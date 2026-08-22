use july_workspace::application::{CreateWorkResult, TransitionWork, WorkError, WorkService};
use july_workspace::domain::{
    Conversation, ConversationId, ConversationKind, DomainError, ResultId, WorkItem, WorkItemId,
    WorkResult, WorkStatus,
};
use july_workspace::runtime::StorageWorker;
use july_workspace::storage::SqliteStore;
use std::path::{Path, PathBuf};

const CREATED: &str = "2026-08-22T08:00:00Z";
const READY_AT: &str = "2026-08-22T09:00:00Z";
const DONE_AT: &str = "2026-08-22T10:00:00Z";

struct TestDatabase {
    directory: PathBuf,
    path: PathBuf,
}

impl TestDatabase {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("july-work-results-{}", ulid::Ulid::generate()));
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

fn seed_work(path: &Path, status: WorkStatus) -> WorkItem {
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
        title: "Produce structured output".into(),
        goal: None,
        status: WorkStatus::Open,
        owner_agent_id: None,
        is_primary: false,
        created_at: CREATED.into(),
        updated_at: CREATED.into(),
        completed_at: None,
    };
    let mut store = SqliteStore::open(path).unwrap();
    store.insert_conversation(&conversation).unwrap();
    store.insert_work_item(&work).unwrap();
    if status == WorkStatus::Working {
        store
            .transition_work(work.id, WorkStatus::Working, CREATED)
            .unwrap()
    } else {
        work
    }
}

fn work_result(
    work_id: WorkItemId,
    id: ResultId,
    supersedes_result_id: Option<ResultId>,
    summary: &str,
) -> WorkResult {
    WorkResult {
        id,
        work_id,
        status: "accepted".into(),
        summary: summary.into(),
        outputs: vec!["artifact://report.json".into(), "line\nbreak".into()],
        evidence: vec![
            "cargo test --test work_results".into(),
            "review: approved".into(),
        ],
        supersedes_result_id,
        created_at: READY_AT.into(),
    }
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

#[tokio::test]
async fn first_result_atomically_marks_work_ready_and_round_trips_structured_content() {
    let database = TestDatabase::new();
    let work = seed_work(database.path(), WorkStatus::Working);
    let expected = work_result(work.id, ResultId::new(), None, "Initial result");
    let mut service = WorkService::new(StorageWorker::open(database.path()).unwrap());

    let created = service
        .create_result(CreateWorkResult {
            result: expected.clone(),
        })
        .await
        .unwrap();

    assert_eq!(created, expected);
    assert_eq!(read_result(database.path(), expected.id), Some(expected));
    let ready = read_work(database.path(), work.id);
    assert_eq!(ready.status, WorkStatus::Ready);
    assert_eq!(ready.updated_at, READY_AT);
    assert_eq!(ready.completed_at, None);
}

#[tokio::test]
async fn result_insert_failure_rolls_back_the_ready_transition() {
    let database = TestDatabase::new();
    let work = seed_work(database.path(), WorkStatus::Working);
    let result = work_result(work.id, ResultId::new(), None, "Will fail");
    let connection = rusqlite::Connection::open(database.path()).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER reject_work_result
             BEFORE INSERT ON work_results
             BEGIN
                 SELECT RAISE(ABORT, 'forced result failure');
             END;",
        )
        .unwrap();
    drop(connection);
    let mut service = WorkService::new(StorageWorker::open(database.path()).unwrap());

    assert!(matches!(
        service
            .create_result(CreateWorkResult {
                result: result.clone(),
            })
            .await,
        Err(WorkError::Runtime(message)) if message.contains("forced result failure")
    ));
    assert_eq!(read_work(database.path(), work.id), work);
    assert_eq!(read_result(database.path(), result.id), None);
}

#[tokio::test]
async fn exact_result_retry_is_unchanged_and_conflicting_content_is_rejected() {
    let database = TestDatabase::new();
    let work = seed_work(database.path(), WorkStatus::Working);
    let result = work_result(work.id, ResultId::new(), None, "Stable result");
    let mut service = WorkService::new(StorageWorker::open(database.path()).unwrap());
    service
        .create_result(CreateWorkResult {
            result: result.clone(),
        })
        .await
        .unwrap();
    let done = service
        .transition(TransitionWork {
            work_id: work.id,
            target: WorkStatus::Done,
            transitioned_at: DONE_AT.into(),
        })
        .await
        .unwrap();

    assert_eq!(
        service
            .create_result(CreateWorkResult {
                result: result.clone(),
            })
            .await
            .unwrap(),
        result
    );
    assert_eq!(read_work(database.path(), work.id), done);

    let mut conflicting = result.clone();
    conflicting.summary = "Conflicting rewrite".into();
    assert_eq!(
        service
            .create_result(CreateWorkResult {
                result: conflicting,
            })
            .await,
        Err(WorkError::ResultConflict(result.id))
    );
    assert_eq!(read_result(database.path(), result.id), Some(result));
    assert_eq!(read_work(database.path(), work.id), done);
}

#[tokio::test]
async fn correction_is_immutable_and_must_supersede_an_existing_result_from_the_same_work() {
    let database = TestDatabase::new();
    let first_work = seed_work(database.path(), WorkStatus::Working);
    let second_work = seed_work(database.path(), WorkStatus::Working);
    let first = work_result(first_work.id, ResultId::new(), None, "Initial result");
    let correction = work_result(
        first_work.id,
        ResultId::new(),
        Some(first.id),
        "Corrected result",
    );
    let mut service = WorkService::new(StorageWorker::open(database.path()).unwrap());
    service
        .create_result(CreateWorkResult {
            result: first.clone(),
        })
        .await
        .unwrap();

    assert_eq!(
        service
            .create_result(CreateWorkResult {
                result: correction.clone(),
            })
            .await
            .unwrap(),
        correction
    );
    assert_eq!(read_result(database.path(), first.id), Some(first.clone()));
    assert_eq!(
        read_result(database.path(), correction.id),
        Some(correction.clone())
    );

    let cross_work = work_result(
        second_work.id,
        ResultId::new(),
        Some(first.id),
        "Wrong source work",
    );
    assert_eq!(
        service
            .create_result(CreateWorkResult {
                result: cross_work.clone(),
            })
            .await,
        Err(WorkError::CrossWorkSupersede {
            result_id: cross_work.id,
            supersedes_result_id: first.id,
        })
    );
    assert_eq!(read_result(database.path(), cross_work.id), None);
    assert_eq!(read_work(database.path(), second_work.id), second_work);

    let missing_id = ResultId::new();
    let missing = work_result(
        second_work.id,
        ResultId::new(),
        Some(missing_id),
        "Missing predecessor",
    );
    assert_eq!(
        service
            .create_result(CreateWorkResult {
                result: missing.clone(),
            })
            .await,
        Err(WorkError::SupersededResultNotFound(missing_id))
    );
    assert_eq!(read_result(database.path(), missing.id), None);
    assert_eq!(read_work(database.path(), second_work.id), second_work);
}

#[tokio::test]
async fn first_result_rejects_missing_or_ineligible_source_work_without_partial_writes() {
    let database = TestDatabase::new();
    let open = seed_work(database.path(), WorkStatus::Open);
    let missing_work_id = WorkItemId::new();
    let missing = work_result(missing_work_id, ResultId::new(), None, "Missing work");
    let ineligible = work_result(open.id, ResultId::new(), None, "Too early");
    let mut service = WorkService::new(StorageWorker::open(database.path()).unwrap());

    assert_eq!(
        service
            .create_result(CreateWorkResult {
                result: missing.clone(),
            })
            .await,
        Err(WorkError::WorkNotFound(missing_work_id))
    );
    assert_eq!(read_result(database.path(), missing.id), None);

    assert_eq!(
        service
            .create_result(CreateWorkResult {
                result: ineligible.clone(),
            })
            .await,
        Err(WorkError::InvalidTransition {
            work_id: open.id,
            from: WorkStatus::Open,
            to: WorkStatus::Ready,
        })
    );
    assert_eq!(read_result(database.path(), ineligible.id), None);
    assert_eq!(read_work(database.path(), open.id), open);
}

#[test]
fn result_cannot_supersede_itself() {
    let id = ResultId::new();
    let result = work_result(WorkItemId::new(), id, Some(id), "Invalid cycle");

    assert_eq!(result.validate(), Err(DomainError::ResultSupersedesItself));
}
