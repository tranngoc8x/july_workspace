use july_workspace::domain::{Agent, AgentId};
use july_workspace::storage::SqliteStore;
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::{Command, Output};
#[cfg(unix)]
use std::{ffi::OsString, os::unix::ffi::OsStringExt};

struct TestWorkspace {
    root: PathBuf,
    database: PathBuf,
}

const NOW: &str = "2026-08-24T00:00:00Z";

impl TestWorkspace {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("july-cli-thread-{}", ulid::Ulid::generate()));
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

    fn threads(&self) -> i64 {
        Connection::open(&self.database)
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM conversations WHERE type = 'thread'",
                [],
                |row| row.get(0),
            )
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
    let error: Value = serde_json::from_str(&stderr(output)).unwrap();
    assert_eq!(error["error"]["code"], code);
}

#[test]
fn thread_create_and_list_render_durable_ids_for_humans_and_json() {
    let workspace = TestWorkspace::new();
    let room_id = stdout(&workspace.run(&["room", "create", "Payments"]))
        .trim()
        .to_owned();

    let created = workspace.run(&[
        "thread",
        "create",
        "Settlement",
        "--room",
        "Payments",
        "--goal",
        "Close books",
    ]);
    assert!(created.status.success(), "stderr: {}", stderr(&created));
    let created_stdout = stdout(&created);
    let ids: Vec<_> = created_stdout.trim().split('\t').collect();
    assert_eq!(ids.len(), 2);
    assert_eq!(ids[0].len(), 26);
    assert_eq!(ids[1].len(), 26);
    assert_eq!(workspace.threads(), 1);

    assert!(workspace.run(&["room", "create", "Other"]).status.success());
    assert!(
        workspace
            .run(&["thread", "create", "Other work", "--room", "Other"])
            .status
            .success()
    );

    let listed = workspace.run(&["thread", "list", "--room", &room_id]);
    assert_eq!(
        stdout(&listed),
        format!("{}\t{room_id}\tSettlement\tClose books\topen\n", ids[0])
    );

    let listed = json_stdout(&workspace.run(&["--json", "thread", "list", "--room", "Payments"]));
    assert_eq!(listed[0]["thread_id"], ids[0]);
    assert_eq!(listed[0]["room_id"], room_id);
    assert_eq!(listed[0]["title"], "Settlement");
    assert_eq!(listed[0]["goal"], "Close books");
    assert_eq!(listed[0]["status"], "open");
    assert!(listed[0]["created_at"].as_str().is_some());
    assert!(listed[0]["updated_at"].as_str().is_some());
}

#[test]
fn thread_membership_preserves_user_and_agent_history_with_idempotent_changes() {
    let workspace = TestWorkspace::new();
    let codex = workspace.seed_agent("Codex");
    let reviewer = workspace.seed_agent("Reviewer");
    let room_id = stdout(&workspace.run(&["room", "create", "Payments"]))
        .trim()
        .to_owned();
    assert_eq!(
        stdout(&workspace.run(&["room", "member", "add", &room_id, "Codex"])),
        "active\ttrue\n"
    );
    assert_eq!(
        stdout(&workspace.run(&["room", "member", "add", &room_id, "Reviewer"])),
        "active\ttrue\n"
    );

    let created = workspace.run(&[
        "thread",
        "create",
        "Settlement",
        "--room",
        "Payments",
        "--member",
        "Codex",
        "--member",
        &codex.id.to_string(),
    ]);
    assert!(created.status.success(), "stderr: {}", stderr(&created));
    let created_stdout = stdout(&created);
    let thread_id = created_stdout.split('\t').next().unwrap().trim().to_owned();

    assert_eq!(
        stdout(&workspace.run(&["thread", "member", "add", &thread_id, "Reviewer",])),
        "active\ttrue\n"
    );
    assert_eq!(
        stdout(&workspace.run(&[
            "thread",
            "member",
            "add",
            &thread_id,
            &reviewer.id.to_string(),
        ])),
        "active\tfalse\n"
    );
    assert_eq!(
        stdout(&workspace.run(&[
            "thread",
            "member",
            "remove",
            &thread_id,
            &reviewer.id.to_string(),
        ])),
        "left\ttrue\n"
    );
    assert_eq!(
        stdout(&workspace.run(&["thread", "member", "remove", &thread_id, "Reviewer",])),
        "left\tfalse\n"
    );

    let members = workspace.run(&["thread", "members", &thread_id, "--json"]);
    let members = json_stdout(&members);
    let members = members.as_array().unwrap();
    assert_eq!(members.len(), 3);
    assert!(members.iter().any(|member| {
        member["thread_id"] == thread_id
            && member["member_type"] == "user"
            && member["member_id"] == "local-user"
            && member["state"] == "active"
    }));
    assert!(members.iter().any(|member| {
        member["member_type"] == "agent"
            && member["member_id"] == codex.id.to_string()
            && member["generation"] == 1
            && member["state"] == "active"
    }));
    assert!(members.iter().any(|member| {
        member["member_type"] == "agent"
            && member["member_id"] == reviewer.id.to_string()
            && member["generation"] == 1
            && member["left_at"].as_str().is_some()
            && member["state"] == "left"
    }));
}

#[test]
fn thread_rejects_invalid_grammar_before_storage_and_exact_reference_misses_before_mutation() {
    for args in [
        ["thread", "create", "Settlement", "--room"].as_slice(),
        ["thread", "list", "--room"].as_slice(),
        ["thread", "members", "not-a-thread-id"].as_slice(),
        ["thread", "member", "add", "not-a-thread-id", "Codex"].as_slice(),
        ["thread", "open", "not-a-thread-id", "--agent", "Codex"].as_slice(),
    ] {
        let workspace = TestWorkspace::new();
        let output = workspace.run(args);
        assert!(!output.status.success());
        assert!(stderr(&output).contains("usage: july dm <agent>"));
        assert!(!workspace.database.exists());
    }

    let workspace = TestWorkspace::new();
    workspace.seed_agent("Codex");
    let room_id = stdout(&workspace.run(&["room", "create", "Payments"]))
        .trim()
        .to_owned();
    let before = workspace.threads();
    let wrong_case = workspace.run(&[
        "--json",
        "thread",
        "create",
        "Settlement",
        "--room",
        &room_id,
        "--member",
        "codex",
    ]);
    json_error(&wrong_case, "agent_not_found");
    assert_eq!(workspace.threads(), before);

    let missing = workspace.run(&[
        "thread",
        "members",
        &july_workspace::domain::ConversationId::new().to_string(),
        "--json",
    ]);
    json_error(&missing, "thread_not_found");
}

#[test]
fn thread_json_flags_are_framed_and_duplicate_or_invalid_utf8_arguments_do_not_open_storage() {
    let duplicate = TestWorkspace::new();
    let output = duplicate.run(&["thread", "list", "--room", "Payments", "--json", "--json"]);
    json_error(&output, "usage");
    assert!(!duplicate.database.exists());

    let unknown = TestWorkspace::new();
    let output = unknown.run(&[
        "--json",
        "thread",
        "list",
        "--room",
        "Payments",
        "--unknown",
    ]);
    json_error(&output, "usage");
    assert!(!unknown.database.exists());

    #[cfg(unix)]
    {
        let invalid = TestWorkspace::new();
        let output = Command::new(env!("CARGO_BIN_EXE_july"))
            .args([
                OsString::from("--json"),
                OsString::from("thread"),
                OsString::from_vec(vec![0xFF]),
            ])
            .env("JULY_WORKSPACE_DB", invalid.database.file_name().unwrap())
            .current_dir(&invalid.root)
            .output()
            .unwrap();
        json_error(&output, "usage");
        assert!(!invalid.database.exists());
    }
}
