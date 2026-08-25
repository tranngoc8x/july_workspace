//! `july agent` is the administrative agent lifecycle: it configures a logical
//! Agent identity bound to a project. It never starts an AgentSession and never
//! grants Room membership.

use july_workspace::domain::{Room, RoomId};
use july_workspace::storage::SqliteStore;
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::{Command, Output};

const NOW: &str = "2026-08-24T00:00:00Z";

struct TestWorkspace {
    root: PathBuf,
    database: PathBuf,
}

impl TestWorkspace {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("july-cli-agent-{}", ulid::Ulid::generate()));
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

    fn seed_room(&self, name: &str) -> Room {
        let room = Room {
            id: RoomId::new(),
            name: name.into(),
            description: None,
            status: "active".into(),
            created_at: NOW.into(),
            updated_at: NOW.into(),
        };
        SqliteStore::open(&self.database)
            .unwrap()
            .insert_room(&room)
            .unwrap();
        room
    }

    fn count(&self, sql: &str) -> i64 {
        Connection::open(&self.database)
            .unwrap()
            .query_row(sql, [], |row| row.get(0))
            .unwrap()
    }
}

impl Drop for TestWorkspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

#[test]
fn agent_add_creates_an_identity_without_a_session_or_room_membership() {
    let workspace = TestWorkspace::new();
    workspace.seed_room("Operations");

    let added = workspace.run(&[
        "agent",
        "add",
        "cashpoint",
        "--project",
        "/work/cashpoint",
        "--runtime",
        "codex",
    ]);

    assert!(added.status.success(), "stderr: {}", stderr(&added));
    let rendered = stdout(&added);
    let fields: Vec<_> = rendered.trim_end().split('\t').collect();
    assert_eq!(fields.len(), 6);
    assert_eq!(fields[1], "cashpoint");
    assert_eq!(fields[2], "/work/cashpoint");
    assert_eq!(fields[3], "acp");
    assert_eq!(fields[4], "codex");
    assert_eq!(fields[5], "active");

    // Identity only: no session binding, no room membership.
    assert_eq!(workspace.count("SELECT COUNT(*) FROM session_bindings"), 0);
    assert_eq!(workspace.count("SELECT COUNT(*) FROM room_members"), 0);
    assert_eq!(workspace.count("SELECT COUNT(*) FROM conversations"), 0);
}

#[test]
fn agent_add_rejects_a_duplicate_name_and_a_missing_project() {
    let workspace = TestWorkspace::new();
    assert!(
        workspace
            .run(&["agent", "add", "cashpoint", "--project", "/work/cashpoint"])
            .status
            .success()
    );

    let duplicate = workspace.run(&[
        "agent",
        "add",
        "cashpoint",
        "--project",
        "/work/other",
        "--json",
    ]);
    assert!(!duplicate.status.success());
    assert!(stderr(&duplicate).contains("\"code\":\"agent_name_conflict\""));

    let missing_project = workspace.run(&["agent", "add", "cashpoint"]);
    assert!(!missing_project.status.success());
    assert!(stderr(&missing_project).contains("usage"));

    assert_eq!(workspace.count("SELECT COUNT(*) FROM agents"), 1);
}

#[test]
fn agent_list_show_and_remove_render_human_and_json() {
    let workspace = TestWorkspace::new();
    let config = workspace.root.join("codex.json");
    std::fs::write(
        &config,
        json!({ "executable": "/usr/bin/codex", "arguments": ["acp"] }).to_string(),
    )
    .unwrap();
    workspace.run(&[
        "agent",
        "add",
        "cashpoint",
        "--project",
        "/work/cashpoint",
        "--runtime",
        "codex",
        "--config",
        config.to_str().unwrap(),
    ]);
    workspace.run(&["agent", "add", "pay", "--project", "/work/pay"]);

    let listed = workspace.run(&["agent", "list", "--json"]);
    assert!(listed.status.success(), "stderr: {}", stderr(&listed));
    let agents: Vec<Value> = serde_json::from_str(stdout(&listed).trim_end()).unwrap();
    assert_eq!(agents.len(), 2);
    assert_eq!(agents[0]["name"], "cashpoint");
    assert_eq!(agents[0]["runtime"], "codex");
    assert_eq!(agents[1]["name"], "pay");
    assert_eq!(agents[1]["runtime"], "");

    let shown = workspace.run(&["agent", "show", "cashpoint"]);
    assert!(stdout(&shown).contains("cashpoint\t/work/cashpoint\tacp\tcodex\tactive"));

    // The stored connection details came from --config, not from the caller.
    let transport: String = Connection::open(&workspace.database)
        .unwrap()
        .query_row(
            "SELECT transport_config_json FROM agents WHERE name = 'cashpoint'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(transport.contains("/usr/bin/codex"));

    // Removal retires the identity; it does not delete the record.
    let removed = workspace.run(&["agent", "remove", "pay", "--json"]);
    assert!(removed.status.success(), "stderr: {}", stderr(&removed));
    let removed: Value = serde_json::from_str(stdout(&removed).trim_end()).unwrap();
    assert_eq!(removed["status"], "inactive");
    assert_eq!(workspace.count("SELECT COUNT(*) FROM agents"), 2);

    let missing = workspace.run(&["agent", "show", "nobody", "--json"]);
    assert!(!missing.status.success());
    assert!(stderr(&missing).contains("\"code\":\"agent_not_found\""));
}
