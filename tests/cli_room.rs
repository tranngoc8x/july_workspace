use july_workspace::domain::{Agent, AgentId};
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
        let root = std::env::temp_dir().join(format!("july-cli-room-{}", ulid::Ulid::generate()));
        std::fs::create_dir(&root).unwrap();
        let database = root.join("workspace.db");
        Self { root, database }
    }

    fn seed_agent(&self, name: &str) -> Agent {
        let agent = Agent {
            id: AgentId::new(),
            name: name.into(),
            project_root: self.root.to_string_lossy().into_owned(),
            transport_type: "acp".into(),
            transport_config: json!({}),
            status: "active".into(),
            metadata: json!({}),
            created_at: NOW.into(),
            updated_at: NOW.into(),
        };
        SqliteStore::open(&self.database)
            .unwrap()
            .insert_agent(&agent)
            .unwrap();
        agent
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

    fn rooms(&self) -> i64 {
        Connection::open(&self.database)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM rooms", [], |row| row.get(0))
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

fn json_stdout(output: &Output) -> Value {
    assert!(output.status.success(), "stderr: {}", stderr(output));
    serde_json::from_str(&stdout(output)).unwrap()
}

fn json_error(output: &Output, code: &str) {
    assert!(!output.status.success());
    assert!(stdout(output).is_empty());
    assert!(stderr(output).ends_with('\n'));
    let value: Value = serde_json::from_str(&stderr(output)).unwrap();
    assert_eq!(value["error"]["code"], code);
    assert!(value["error"]["message"].is_string());
}

#[test]
fn room_create_list_and_description_render_for_humans() {
    let workspace = TestWorkspace::new();
    let created = workspace.run(&[
        "room",
        "create",
        "Payments",
        "--description",
        "Settlement work",
    ]);
    assert!(created.status.success(), "stderr: {}", stderr(&created));
    let room_id = stdout(&created).trim().to_owned();
    assert_eq!(room_id.len(), 26);

    let listed = workspace.run(&["room", "list"]);
    assert!(listed.status.success(), "stderr: {}", stderr(&listed));
    assert_eq!(
        stdout(&listed),
        format!("{room_id}\tPayments\tSettlement work\tactive\n")
    );
}

#[test]
fn room_members_accept_exact_names_and_typed_ids_and_retain_history() {
    let workspace = TestWorkspace::new();
    let agent = workspace.seed_agent("Codex");
    let room_id = stdout(&workspace.run(&["room", "create", "Payments"]))
        .trim()
        .to_owned();

    let added = workspace.run(&["room", "member", "add", "Payments", "Codex"]);
    assert_eq!(stdout(&added), "active\ttrue\n");
    let repeated = workspace.run(&["room", "member", "add", &room_id, &agent.id.to_string()]);
    assert_eq!(stdout(&repeated), "active\tfalse\n");
    let removed = workspace.run(&["room", "member", "remove", &room_id, &agent.id.to_string()]);
    assert_eq!(stdout(&removed), "left\ttrue\n");
    let repeated = workspace.run(&["room", "member", "remove", "Payments", "Codex"]);
    assert_eq!(stdout(&repeated), "left\tfalse\n");

    let members = workspace.run(&["room", "members", "Payments"]);
    let member_output = stdout(&members);
    let fields: Vec<_> = member_output.trim_end().split('\t').collect();
    assert_eq!(fields[0], room_id);
    assert_eq!(fields[1], agent.id.to_string());
    assert_eq!(fields[2], "");
    assert_eq!(fields[3], "1");
    assert!(!fields[4].is_empty());
    assert!(!fields[5].is_empty());
    assert_eq!(fields[6], "left");
}

#[test]
fn room_rejects_exact_reference_misses_and_malformed_grammar_without_mutation() {
    let workspace = TestWorkspace::new();
    workspace.seed_agent("Codex");
    let created = workspace.run(&["room", "create", "Payments"]);
    assert!(created.status.success());
    let before = workspace.rooms();

    let wrong_case = workspace.run(&["room", "member", "add", "payments", "Codex"]);
    assert!(!wrong_case.status.success());
    assert!(stderr(&wrong_case).contains("room payments does not exist"));
    let unknown_flag = workspace.run(&["room", "create", "Other", "--unknown"]);
    assert!(!unknown_flag.status.success());
    let duplicate_description = workspace.run(&[
        "room",
        "create",
        "Other",
        "--description",
        "one",
        "--description",
        "two",
    ]);
    assert!(!duplicate_description.status.success());
    let missing_value = workspace.run(&["room", "create", "Other", "--description"]);
    assert!(!missing_value.status.success());
    let extra_positional = workspace.run(&["room", "create", "Other", "extra"]);
    assert!(!extra_positional.status.success());
    assert_eq!(workspace.rooms(), before);
}

#[test]
fn room_rejects_flag_references_before_opening_storage() {
    for args in [
        ["room", "members", "--unknown"].as_slice(),
        ["room", "member", "add", "Payments", "--unknown"].as_slice(),
    ] {
        let workspace = TestWorkspace::new();
        let output = workspace.run(args);
        assert!(!output.status.success());
        assert!(stderr(&output).contains("usage: july dm <agent>"));
        assert!(!workspace.database.exists());
    }
}

#[test]
fn room_and_agent_names_with_typed_id_length_remain_names() {
    let workspace = TestWorkspace::new();
    let name = "iiiiiiiiiiiiiiiiiiiiiiiiii";
    assert_eq!(name.len(), 26);
    workspace.seed_agent(name);
    let created = workspace.run(&["room", "create", name]);
    assert!(created.status.success(), "stderr: {}", stderr(&created));

    let added = workspace.run(&["room", "member", "add", name, name]);
    assert_eq!(stdout(&added), "active\ttrue\n");
}

#[test]
fn room_json_success_errors_and_flag_placement_are_framed() {
    let workspace = TestWorkspace::new();
    let agent = workspace.seed_agent("Codex");
    let created = workspace.run(&[
        "--json",
        "room",
        "create",
        "Payments",
        "--description",
        "Settlement work",
    ]);
    let room_id = json_stdout(&created)["room_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let listed = workspace.run(&["room", "list", "--json"]);
    let listed = json_stdout(&listed);
    assert_eq!(listed[0]["room_id"], room_id);
    assert_eq!(listed[0]["name"], "Payments");
    assert_eq!(listed[0]["description"], "Settlement work");
    assert_eq!(listed[0]["status"], "active");
    assert!(listed[0]["created_at"].as_str().is_some());
    assert_eq!(listed[0]["updated_at"], listed[0]["created_at"]);

    let added = workspace.run(&[
        "room",
        "member",
        "add",
        "Payments",
        &agent.id.to_string(),
        "--json",
    ]);
    assert_eq!(
        json_stdout(&added),
        json!({"state": "active", "changed": true})
    );
    let members = workspace.run(&["--json", "room", "members", &room_id]);
    let members = json_stdout(&members);
    assert_eq!(members[0]["room_id"], room_id);
    assert_eq!(members[0]["agent_id"], agent.id.to_string());
    assert_eq!(members[0]["role"], Value::Null);
    assert_eq!(members[0]["generation"], 1);
    assert_eq!(members[0]["left_at"], Value::Null);
    assert_eq!(members[0]["state"], "active");

    let unknown = workspace.run(&["room", "members", "missing", "--json"]);
    json_error(&unknown, "room_not_found");
    let duplicate_name = workspace.run(&["--json", "room", "create", "Payments"]);
    json_error(&duplicate_name, "room_name_conflict");
    let duplicate = workspace.run(&["room", "list", "--json", "--json"]);
    json_error(&duplicate, "usage");
    let dm = workspace.run(&["dm", "Codex", "--json"]);
    json_error(&dm, "usage");
}

#[cfg(unix)]
#[test]
fn room_rejects_invalid_utf8_before_mutation() {
    let workspace = TestWorkspace::new();
    let output = workspace.run_os([OsString::from("room"), OsString::from_vec(vec![0xFF])]);
    assert!(!output.status.success());
    assert!(stdout(&output).is_empty());
    assert!(stderr(&output).contains("valid UTF-8"));
    assert!(!workspace.database.exists());
}

#[cfg(unix)]
#[test]
fn dm_retains_its_invalid_utf8_diagnostic() {
    let workspace = TestWorkspace::new();
    let output = workspace.run_os([OsString::from("dm"), OsString::from_vec(vec![0xFF])]);
    assert!(!output.status.success());
    assert!(stdout(&output).is_empty());
    assert_eq!(stderr(&output), "agent name must be valid UTF-8\n");
    assert!(!workspace.database.exists());
}
