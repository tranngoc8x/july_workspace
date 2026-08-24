use july_workspace::domain::{
    Conversation, ConversationId, ConversationKind, PublishId, ResultId, WorkItem, WorkItemId,
    WorkResult, WorkStatus,
};
use july_workspace::storage::SqliteStore;
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::{Command, Output};
#[cfg(unix)]
use std::{ffi::OsString, os::unix::ffi::OsStringExt};

const NOW: &str = "2026-08-24T00:00:00Z";

struct TestWorkspace {
    root: PathBuf,
    database: PathBuf,
}

impl TestWorkspace {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("july-cli-publish-{}", ulid::Ulid::generate()));
        std::fs::create_dir(&root).unwrap();
        let database = root.join("workspace.db");
        Self { root, database }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_july"))
            .args(args)
            .env("JULY_WORKSPACE_DB", self.database.file_name().unwrap())
            .current_dir(&self.root)
            .output()
            .unwrap()
    }

    #[cfg(unix)]
    fn run_os(&self, args: impl IntoIterator<Item = OsString>) -> Output {
        Command::new(env!("CARGO_BIN_EXE_july"))
            .args(args)
            .env("JULY_WORKSPACE_DB", self.database.file_name().unwrap())
            .current_dir(&self.root)
            .output()
            .unwrap()
    }

    fn seed_result(&self) -> (ConversationId, ConversationId, ResultId) {
        let source = conversation();
        let target = conversation();
        let work = WorkItem {
            id: WorkItemId::new(),
            conversation_id: source.id,
            title: "Publish result".into(),
            goal: None,
            status: WorkStatus::Open,
            owner_agent_id: None,
            is_primary: false,
            created_at: NOW.into(),
            updated_at: NOW.into(),
            completed_at: None,
        };
        let result = WorkResult {
            id: ResultId::new(),
            work_id: work.id,
            status: "accepted".into(),
            summary: "Ready to publish".into(),
            outputs: vec![],
            evidence: vec![],
            supersedes_result_id: None,
            created_at: NOW.into(),
        };
        let mut store = SqliteStore::open(&self.database).unwrap();
        store.insert_conversation(&source).unwrap();
        store.insert_conversation(&target).unwrap();
        store.insert_work_item(&work).unwrap();
        store
            .transition_work(work.id, WorkStatus::Working, NOW)
            .unwrap();
        store.create_work_result(&result).unwrap();
        (source.id, target.id, result.id)
    }

    fn publishes(&self) -> i64 {
        Connection::open(&self.database)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM publishes", [], |row| row.get(0))
            .unwrap()
    }
}

impl Drop for TestWorkspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
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
        created_at: NOW.into(),
        updated_at: NOW.into(),
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

fn json_error(output: &Output, code: &str) {
    assert!(!output.status.success());
    assert!(stdout(output).is_empty());
    let error: Value = serde_json::from_str(&stderr(output)).unwrap();
    assert_eq!(error["error"]["code"], code);
    assert!(error["error"]["message"].is_string());
}

#[test]
fn publish_renders_human_and_json_and_retries_with_the_durable_publish() {
    let workspace = TestWorkspace::new();
    let (source_id, target_id, result_id) = workspace.seed_result();

    let first = workspace.run(&[
        "publish",
        &result_id.to_string(),
        "--to",
        &target_id.to_string(),
    ]);
    assert!(first.status.success(), "stderr: {}", stderr(&first));
    let first_stdout = stdout(&first);
    let fields: Vec<_> = first_stdout.trim_end().split('\t').collect();
    assert_eq!(fields.len(), 5);
    let publish_id: PublishId = fields[0].parse().unwrap();
    assert_eq!(publish_id.to_string(), fields[0]);
    let rendered_result_id: ResultId = fields[1].parse().unwrap();
    assert_eq!(rendered_result_id.to_string(), fields[1]);
    let rendered_source_id: ConversationId = fields[2].parse().unwrap();
    assert_eq!(rendered_source_id.to_string(), fields[2]);
    let rendered_target_id: ConversationId = fields[3].parse().unwrap();
    assert_eq!(rendered_target_id.to_string(), fields[3]);
    assert_eq!(fields[1], result_id.to_string());
    assert_eq!(fields[2], source_id.to_string());
    assert_eq!(fields[3], target_id.to_string());
    assert!(!fields[4].is_empty());

    let retry = workspace.run(&[
        "publish",
        &result_id.to_string(),
        "--to",
        &target_id.to_string(),
    ]);
    assert!(retry.status.success(), "stderr: {}", stderr(&retry));
    assert_eq!(stdout(&retry), first_stdout);
    assert_eq!(workspace.publishes(), 1);

    let json_output = workspace.run(&[
        "--json",
        "publish",
        &result_id.to_string(),
        "--to",
        &target_id.to_string(),
    ]);
    assert!(
        json_output.status.success(),
        "stderr: {}",
        stderr(&json_output)
    );
    assert_eq!(
        serde_json::from_str::<Value>(&stdout(&json_output)).unwrap(),
        json!({
            "publish_id": fields[0],
            "result_id": result_id.to_string(),
            "source_conversation_id": source_id.to_string(),
            "target_conversation_id": target_id.to_string(),
            "published_at": fields[4],
        })
    );
    let stored = SqliteStore::open(&workspace.database)
        .unwrap()
        .get_publish(publish_id)
        .unwrap()
        .unwrap();
    assert_eq!(stored.id, publish_id);
    assert_eq!(stored.result_id, result_id);
    assert_eq!(stored.source_conversation_id, source_id);
    assert_eq!(stored.target_conversation_id, target_id);
    assert_eq!(stored.created_at, fields[4]);
}

#[test]
fn publish_reports_missing_result_and_target_as_typed_json_errors() {
    let workspace = TestWorkspace::new();
    let (_, target_id, result_id) = workspace.seed_result();

    let missing_result = workspace.run(&[
        "publish",
        &ResultId::new().to_string(),
        "--to",
        &target_id.to_string(),
        "--json",
    ]);
    json_error(&missing_result, "result_not_found");

    let missing_target = workspace.run(&[
        "--json",
        "publish",
        &result_id.to_string(),
        "--to",
        &ConversationId::new().to_string(),
    ]);
    json_error(&missing_target, "target_not_found");
    assert_eq!(workspace.publishes(), 0);
}

#[test]
fn publish_rejects_invalid_grammar_and_ids_before_creating_storage() {
    let result_id = ResultId::new().to_string();
    let target_id = ConversationId::new().to_string();
    let lowercase_result = result_id.to_ascii_lowercase();
    let lowercase_target = target_id.to_ascii_lowercase();
    for args in [
        ["publish", &result_id].as_slice(),
        ["publish", &result_id, "--to"].as_slice(),
        [
            "publish", &result_id, "--to", &target_id, "--to", &target_id,
        ]
        .as_slice(),
        ["publish", &result_id, "--unknown", &target_id].as_slice(),
        ["publish", &result_id, "--to", &target_id, "extra"].as_slice(),
        ["publish", "not-a-result", "--to", &target_id].as_slice(),
        ["publish", &lowercase_result, "--to", &target_id].as_slice(),
        ["publish", &result_id, "--to", "not-a-conversation"].as_slice(),
        ["publish", &result_id, "--to", &lowercase_target].as_slice(),
        [
            "publish", &result_id, "--to", &target_id, "--json", "--json",
        ]
        .as_slice(),
    ] {
        let workspace = TestWorkspace::new();
        let output = workspace.run(args);
        assert!(!output.status.success());
        assert!(stderr(&output).contains("usage: july dm <agent>"));
        assert!(!workspace.database.exists());
    }

    let workspace = TestWorkspace::new();
    let output = workspace.run(&[
        "--unknown",
        "publish",
        &result_id,
        "--to",
        &target_id,
        "--json",
    ]);
    json_error(&output, "usage");
    assert!(!workspace.database.exists());
}

#[cfg(unix)]
#[test]
fn publish_rejects_invalid_utf8_before_creating_storage() {
    let workspace = TestWorkspace::new();
    let output = workspace.run_os([
        OsString::from("--json"),
        OsString::from("publish"),
        OsString::from_vec(vec![0xFF]),
        OsString::from("--to"),
        OsString::from(ConversationId::new().to_string()),
    ]);
    json_error(&output, "usage");
    assert!(!workspace.database.exists());
}
