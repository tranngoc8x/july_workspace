use july_workspace::application::{PublishError, PublishResult, PublishService, PublishedResult};
use july_workspace::domain::{
    Conversation, ConversationId, ConversationKind, MemberType, Message, MessageId, PublishId,
    ResultId, WorkItem, WorkItemId, WorkResult, WorkStatus,
};
use july_workspace::runtime::StorageWorker;
use july_workspace::storage::SqliteStore;
use std::path::{Path, PathBuf};

const CREATED: &str = "2026-08-22T08:00:00Z";
const READY_AT: &str = "2026-08-22T09:00:00Z";
const PUBLISHED_AT: &str = "2026-08-22T10:00:00Z";

struct TestDatabase {
    directory: PathBuf,
    path: PathBuf,
}

impl TestDatabase {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("july-result-publish-{}", ulid::Ulid::generate()));
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

fn seed_result(
    store: &mut SqliteStore,
    source: &Conversation,
    supersedes_result_id: Option<ResultId>,
    summary: &str,
) -> WorkResult {
    let work_id = if let Some(supersedes_result_id) = supersedes_result_id {
        store
            .get_work_result(supersedes_result_id)
            .unwrap()
            .unwrap()
            .work_id
    } else {
        let work = WorkItem {
            id: WorkItemId::new(),
            conversation_id: source.id,
            title: "Produce structured output".into(),
            goal: None,
            status: WorkStatus::Working,
            owner_agent_id: None,
            is_primary: false,
            created_at: CREATED.into(),
            updated_at: CREATED.into(),
            completed_at: None,
        };
        store.insert_work_item(&work).unwrap();
        work.id
    };
    let result = WorkResult {
        id: ResultId::new(),
        work_id,
        status: "accepted".into(),
        summary: summary.into(),
        outputs: vec!["artifact://report.json".into(), "line\nbreak".into()],
        evidence: vec!["cargo test --test result_publish".into()],
        supersedes_result_id,
        created_at: READY_AT.into(),
    };
    store.create_work_result(&result).unwrap()
}

fn publish_command(
    publish_id: PublishId,
    result_id: ResultId,
    target_conversation_id: ConversationId,
    published_at: &str,
) -> PublishResult {
    PublishResult {
        publish_id,
        result_id,
        target_conversation_id,
        published_at: published_at.into(),
    }
}

fn publish_count(path: &Path) -> i64 {
    rusqlite::Connection::open(path)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM publishes", [], |row| row.get(0))
        .unwrap()
}

#[tokio::test]
async fn target_queries_the_published_immutable_result_and_source_without_transcript_copy() {
    let database = TestDatabase::new();
    let source = conversation();
    let target = conversation();
    let source_message = Message {
        id: MessageId::new(),
        conversation_id: source.id,
        sender_type: MemberType::User,
        sender_id: "tony".into(),
        body: "private source reasoning".into(),
        reply_to: None,
        metadata: serde_json::Value::Null,
        created_at: CREATED.into(),
    };
    let (first, correction) = {
        let mut store = SqliteStore::open(database.path()).unwrap();
        store.insert_conversation(&source).unwrap();
        store.insert_conversation(&target).unwrap();
        store.insert_message(&source_message).unwrap();
        let first = seed_result(&mut store, &source, None, "Initial result");
        let correction = seed_result(&mut store, &source, Some(first.id), "Corrected result");
        (first, correction)
    };
    let publish_id = PublishId::new();
    let expected = PublishedResult {
        publish_id,
        result: correction.clone(),
        source_conversation_id: source.id,
        target_conversation_id: target.id,
        published_at: PUBLISHED_AT.into(),
    };
    let mut service = PublishService::new(StorageWorker::open(database.path()).unwrap());

    assert_eq!(
        service
            .publish(publish_command(
                publish_id,
                correction.id,
                target.id,
                PUBLISHED_AT,
            ))
            .await
            .unwrap(),
        expected
    );
    assert_eq!(
        service.list_for_target(target.id).await.unwrap(),
        vec![expected]
    );

    let store = SqliteStore::open(database.path()).unwrap();
    assert_eq!(
        store.list_messages(source.id).unwrap(),
        vec![source_message]
    );
    assert!(store.list_messages(target.id).unwrap().is_empty());
    assert_eq!(store.get_work_result(first.id).unwrap(), Some(first));
    assert_eq!(
        store.get_work_result(correction.id).unwrap(),
        Some(correction)
    );
    assert_eq!(publish_count(database.path()), 1);
}

#[tokio::test]
async fn natural_key_retry_returns_the_existing_publish_even_with_a_new_id_and_timestamp() {
    let database = TestDatabase::new();
    let source = conversation();
    let target = conversation();
    let result = {
        let mut store = SqliteStore::open(database.path()).unwrap();
        store.insert_conversation(&source).unwrap();
        store.insert_conversation(&target).unwrap();
        seed_result(&mut store, &source, None, "Stable result")
    };
    let first_id = PublishId::new();
    let retry_id = PublishId::new();
    let mut service = PublishService::new(StorageWorker::open(database.path()).unwrap());
    let first = service
        .publish(publish_command(
            first_id,
            result.id,
            target.id,
            PUBLISHED_AT,
        ))
        .await
        .unwrap();

    assert_eq!(
        service
            .publish(publish_command(
                retry_id,
                result.id,
                target.id,
                "2026-08-22T11:00:00Z",
            ))
            .await
            .unwrap(),
        first
    );
    assert_eq!(
        service.list_for_target(target.id).await.unwrap(),
        vec![first]
    );
    assert_eq!(publish_count(database.path()), 1);
    assert_eq!(
        SqliteStore::open(database.path())
            .unwrap()
            .get_publish(retry_id)
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn conflicting_publish_id_is_rejected_without_a_second_mapping() {
    let database = TestDatabase::new();
    let source = conversation();
    let first_target = conversation();
    let second_target = conversation();
    let result = {
        let mut store = SqliteStore::open(database.path()).unwrap();
        store.insert_conversation(&source).unwrap();
        store.insert_conversation(&first_target).unwrap();
        store.insert_conversation(&second_target).unwrap();
        seed_result(&mut store, &source, None, "Stable result")
    };
    let publish_id = PublishId::new();
    let mut service = PublishService::new(StorageWorker::open(database.path()).unwrap());
    let first = service
        .publish(publish_command(
            publish_id,
            result.id,
            first_target.id,
            PUBLISHED_AT,
        ))
        .await
        .unwrap();

    assert_eq!(
        service
            .publish(publish_command(
                publish_id,
                result.id,
                second_target.id,
                PUBLISHED_AT,
            ))
            .await,
        Err(PublishError::PublishIdConflict(publish_id))
    );
    assert_eq!(
        service.list_for_target(first_target.id).await.unwrap(),
        vec![first]
    );
    assert!(
        service
            .list_for_target(second_target.id)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(publish_count(database.path()), 1);
}

#[tokio::test]
async fn missing_result_work_or_target_leave_no_row() {
    let database = TestDatabase::new();
    let source = conversation();
    let target = conversation();
    let valid = {
        let mut store = SqliteStore::open(database.path()).unwrap();
        store.insert_conversation(&source).unwrap();
        store.insert_conversation(&target).unwrap();
        seed_result(&mut store, &source, None, "Valid result")
    };
    let orphan_result_id = ResultId::new();
    let missing_work_id = WorkItemId::new();
    let connection = rusqlite::Connection::open(database.path()).unwrap();
    connection
        .pragma_update(None, "foreign_keys", "OFF")
        .unwrap();
    connection
        .execute(
            "INSERT INTO work_results(
                id, work_id, status, summary, outputs_json, evidence_json, created_at
             ) VALUES (?1, ?2, 'accepted', 'orphan', '[]', '[]', ?3)",
            rusqlite::params![
                orphan_result_id.to_string(),
                missing_work_id.to_string(),
                READY_AT
            ],
        )
        .unwrap();
    drop(connection);
    let mut service = PublishService::new(StorageWorker::open(database.path()).unwrap());
    let missing_result_id = ResultId::new();
    let missing_target_id = ConversationId::new();

    assert_eq!(
        service
            .publish(publish_command(
                PublishId::new(),
                missing_result_id,
                target.id,
                PUBLISHED_AT,
            ))
            .await,
        Err(PublishError::ResultNotFound(missing_result_id))
    );
    assert_eq!(
        service
            .publish(publish_command(
                PublishId::new(),
                orphan_result_id,
                target.id,
                PUBLISHED_AT,
            ))
            .await,
        Err(PublishError::WorkNotFound(missing_work_id))
    );
    assert_eq!(
        service
            .publish(publish_command(
                PublishId::new(),
                valid.id,
                missing_target_id,
                PUBLISHED_AT,
            ))
            .await,
        Err(PublishError::TargetNotFound(missing_target_id))
    );
    assert_eq!(publish_count(database.path()), 0);
}

#[tokio::test]
async fn same_conversation_publish_returns_the_structured_result_and_is_exactly_idempotent() {
    let database = TestDatabase::new();
    let conversation = conversation();
    let result = {
        let mut store = SqliteStore::open(database.path()).unwrap();
        store.insert_conversation(&conversation).unwrap();
        seed_result(&mut store, &conversation, None, "Same-context result")
    };
    let command = publish_command(PublishId::new(), result.id, conversation.id, PUBLISHED_AT);
    let expected = PublishedResult {
        publish_id: command.publish_id,
        result,
        source_conversation_id: conversation.id,
        target_conversation_id: conversation.id,
        published_at: PUBLISHED_AT.into(),
    };
    let mut service = PublishService::new(StorageWorker::open(database.path()).unwrap());

    assert_eq!(service.publish(command.clone()).await.unwrap(), expected);
    assert_eq!(service.publish(command).await.unwrap(), expected);
    assert_eq!(
        service.list_for_target(conversation.id).await.unwrap(),
        vec![expected]
    );
    assert_eq!(publish_count(database.path()), 1);
}
