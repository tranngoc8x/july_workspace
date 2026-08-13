use july_workspace::domain::{Agent, ConversationId, SessionBindingStatus};
use july_workspace::storage::SqliteStore;
use rusqlite::Connection;
use serde_json::json;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const NOW: &str = "2026-08-13T00:00:00Z";

struct TestWorkspace {
    root: PathBuf,
    database: PathBuf,
}

impl TestWorkspace {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("july-cli-{}", ulid::Ulid::generate()));
        std::fs::create_dir(&root).unwrap();
        let database = root.join("workspace.db");
        Self { root, database }
    }

    fn seed_agent(&self, transport_type: &str, transport_config: serde_json::Value) {
        let store = SqliteStore::open(&self.database).unwrap();
        store
            .insert_agent(&Agent {
                id: Default::default(),
                name: "codex".into(),
                project_root: self.root.to_string_lossy().into_owned(),
                transport_type: transport_type.into(),
                transport_config,
                status: "active".into(),
                metadata: json!({}),
                created_at: NOW.into(),
                updated_at: NOW.into(),
            })
            .unwrap();
    }

    fn fixture_config(&self) -> serde_json::Value {
        json!({
            "executable": "/usr/bin/python3",
            "arguments": [PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/acp_agent.py")
                .to_string_lossy()],
            "environment": {},
            "state_directory": self.root,
            "expected_agent_name": "test-acp-agent",
            "expected_agent_version": "1.0.0"
        })
    }

    fn run(&self, input: &str, args: &[&str]) -> std::process::Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_july"))
            .args(args)
            .env("JULY_WORKSPACE_DB", self.database.file_name().unwrap())
            .current_dir(&self.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }
}

impl Drop for TestWorkspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn cli_rejects_bad_arguments_and_transport_configuration() {
    let workspace = TestWorkspace::new();
    let usage = workspace.run("", &[]);
    assert!(!usage.status.success());
    assert!(String::from_utf8_lossy(&usage.stderr).contains("usage: july dm <agent>"));

    workspace.seed_agent("a2a", json!({}));
    let unsupported = workspace.run("", &["dm", "codex"]);
    assert!(!unsupported.status.success());
    assert!(String::from_utf8_lossy(&unsupported.stderr).contains("unsupported transport"));

    let invalid = TestWorkspace::new();
    let mut config = invalid.fixture_config();
    config
        .as_object_mut()
        .unwrap()
        .insert("unexpected".into(), json!(true));
    invalid.seed_agent("acp", config);
    let output = invalid.run("", &["dm", "codex"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unexpected"));
}

#[test]
fn cli_runs_one_dm_turn_and_persists_transport_neutral_state() {
    let workspace = TestWorkspace::new();
    workspace.seed_agent("acp", workspace.fixture_config());

    let output = workspace.run("hello\n1\n/quit\n", &["dm", "codex"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("fixture reply"));

    let connection = Connection::open(&workspace.database).unwrap();
    let conversation: String = connection
        .query_row(
            "SELECT id FROM conversations WHERE type = 'dm'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let conversation_id: ConversationId = conversation.parse().unwrap();
    let store = SqliteStore::open(&workspace.database).unwrap();
    let messages = store.list_messages(conversation_id).unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].body, "hello");
    assert_eq!(messages[0].metadata["july"]["channel"], "dm");
    assert_eq!(messages[0].metadata["july"]["direction"], "outbound");
    assert_eq!(messages[1].body, "fixture reply");
    assert_eq!(messages[1].metadata["july"]["direction"], "inbound");

    let agent = store.get_agent_by_name("codex").unwrap().unwrap();
    let binding = store
        .get_latest_session_binding(conversation_id, agent.id)
        .unwrap()
        .unwrap();
    assert_eq!(binding.status, SessionBindingStatus::Disconnected);
    let (outcome, selected): (String, Option<String>) = connection
        .query_row(
            "SELECT outcome, selected_option_id FROM permission_decisions",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(outcome, "selected");
    assert_eq!(selected.as_deref(), Some("allow-once"));
}
