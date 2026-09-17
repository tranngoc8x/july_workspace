use july_workspace::domain::WorkScope;
use july_workspace::domain::{Agent, AgentId, ConversationId, WorkItemId};
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
        self.seed_agent_with_id(AgentId::new(), name)
    }

    fn seed_agent_with_id(&self, id: AgentId, name: &str) -> Agent {
        let agent = Agent {
            id,
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
    let thread_id: ConversationId = ids[0].parse().unwrap();
    let primary_work_id: WorkItemId = ids[1].parse().unwrap();
    assert_eq!(thread_id.to_string(), ids[0]);
    assert_eq!(primary_work_id.to_string(), ids[1]);
    let primary_work = SqliteStore::open(&workspace.database)
        .unwrap()
        .get_work_item(primary_work_id)
        .unwrap()
        .unwrap();
    assert_eq!(primary_work.scope, WorkScope::Conversation(thread_id));
    assert!(primary_work.is_primary);
    assert_eq!(workspace.threads(), 1);

    assert!(workspace.run(&["room", "create", "Other"]).status.success());
    let json_created = json_stdout(&workspace.run(&[
        "--json",
        "thread",
        "create",
        "Other work",
        "--room",
        "Other",
    ]));
    let json_thread_id: ConversationId =
        json_created["thread_id"].as_str().unwrap().parse().unwrap();
    let json_primary_work_id: WorkItemId = json_created["primary_work_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(
        json_created,
        json!({
            "thread_id": json_thread_id.to_string(),
            "primary_work_id": json_primary_work_id.to_string(),
        })
    );
    let json_primary_work = SqliteStore::open(&workspace.database)
        .unwrap()
        .get_work_item(json_primary_work_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        json_primary_work.scope,
        WorkScope::Conversation(json_thread_id)
    );
    assert!(json_primary_work.is_primary);

    let listed = workspace.run(&["thread", "list", "--room", &room_id]);
    let listed_output = stdout(&listed);
    assert!(listed_output.starts_with("THREAD ID"));
    assert!(listed_output.contains("ROOM ID"));
    assert!(listed_output.contains("TITLE"));
    assert!(listed_output.contains("GOAL"));
    assert!(listed_output.contains("STATUS\n"));
    assert!(listed_output.contains(ids[0]));
    assert!(listed_output.contains(&room_id));
    assert!(listed_output.contains("Settlement"));
    assert!(listed_output.contains("Close books"));
    assert!(listed_output.ends_with("open\n"));
    assert!(!listed_output.contains('\t'));

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
    let codex =
        workspace.seed_agent_with_id("01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(), "Codex");
    let reviewer =
        workspace.seed_agent_with_id("01ARZ3NDEKTSV4RRFFQ69G5FAW".parse().unwrap(), "Reviewer");
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
    assert_eq!(
        stdout(&workspace.run(&["thread", "member", "add", &thread_id, "Reviewer",])),
        "active\ttrue\n"
    );

    let members = workspace.run(&["thread", "members", &thread_id, "--json"]);
    let members = json_stdout(&members);
    let members = members.as_array().unwrap();
    assert_eq!(members.len(), 4);
    assert!(
        members
            .iter()
            .all(|member| member["joined_at"].as_str().is_some())
    );
    assert!(members[1]["left_at"].as_str().is_some());
    let codex_id = codex.id.to_string();
    let reviewer_id = reviewer.id.to_string();
    assert_eq!(
        members
            .iter()
            .map(|member| {
                (
                    member["thread_id"].as_str().unwrap(),
                    member["member_type"].as_str().unwrap(),
                    member["member_id"].as_str().unwrap(),
                    member["generation"].as_u64().unwrap(),
                    member["state"].as_str().unwrap(),
                )
            })
            .collect::<Vec<_>>(),
        vec![
            (thread_id.as_str(), "agent", codex_id.as_str(), 1, "active"),
            (thread_id.as_str(), "agent", reviewer_id.as_str(), 1, "left"),
            (thread_id.as_str(), "user", "july", 1, "active"),
            (
                thread_id.as_str(),
                "agent",
                reviewer_id.as_str(),
                2,
                "active"
            ),
        ]
    );

    let members = stdout(&workspace.run(&["thread", "members", &thread_id]));
    assert!(members.starts_with("THREAD ID"));
    assert!(members.contains("TYPE"));
    assert!(members.contains("MEMBER ID"));
    assert!(members.contains("GENERATION"));
    assert!(members.contains("JOINED AT"));
    assert!(members.contains("LEFT AT"));
    assert!(members.contains("STATE\n"));
    assert!(members.contains(&codex_id));
    assert!(members.contains(&reviewer_id));
    assert!(!members.contains('\t'));
}

#[test]
fn thread_rejects_invalid_grammar_before_storage_and_exact_reference_misses_before_mutation() {
    for args in [
        ["thread", "create", "Settlement", "--room"].as_slice(),
        ["thread", "list", "--room"].as_slice(),
        ["thread", "members", "not-a-thread-id"].as_slice(),
        ["thread", "member", "add", "not-a-thread-id", "Codex"].as_slice(),
    ] {
        let workspace = TestWorkspace::new();
        let output = workspace.run(args);
        assert!(!output.status.success());
        assert!(stderr(&output).contains("usage: july dm <agent>"));
        assert!(!workspace.database.exists());
    }

    // `thread open` resolves its Agent before any session starts.
    let workspace = TestWorkspace::new();
    let thread_id = ConversationId::new().to_string();
    let output = workspace.run(&["thread", "open", &thread_id, "--agent", "Codex"]);
    assert!(!output.status.success());
    assert!(stdout(&output).is_empty());
    assert!(stderr(&output).contains("agent Codex does not exist"));
    assert_eq!(workspace.threads(), 0);

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

#[test]
fn thread_json_errors_preserve_membership_and_open_state_codes() {
    let workspace = TestWorkspace::new();
    let codex = workspace.seed_agent("Codex");
    workspace.seed_agent("Reviewer");
    let room_id = stdout(&workspace.run(&["room", "create", "Payments"]))
        .trim()
        .to_owned();

    let missing_room_membership = workspace.run(&[
        "thread",
        "create",
        "Settlement",
        "--room",
        &room_id,
        "--member",
        "Codex",
        "--json",
    ]);
    json_error(&missing_room_membership, "room_membership_required");

    for agent in ["Codex", "Reviewer"] {
        assert_eq!(
            stdout(&workspace.run(&["room", "member", "add", &room_id, agent])),
            "active\ttrue\n"
        );
    }
    let created = json_stdout(&workspace.run(&[
        "thread",
        "create",
        "Settlement",
        "--room",
        &room_id,
        "--member",
        &codex.id.to_string(),
        "--json",
    ]));
    let thread_id = created["thread_id"].as_str().unwrap();
    Connection::open(&workspace.database)
        .unwrap()
        .execute(
            "UPDATE conversations SET status = 'closed' WHERE id = ?1",
            [thread_id],
        )
        .unwrap();
    let closed = workspace.run(&["--json", "thread", "member", "add", thread_id, "Reviewer"]);
    json_error(&closed, "thread_not_open");
}
