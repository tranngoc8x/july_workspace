use july_workspace::domain::{Agent, AgentId, Room, RoomId};
use july_workspace::storage::SqliteStore;
use rusqlite::Connection;
use serde_json::json;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant};

struct TestWorkspace {
    root: PathBuf,
    database: PathBuf,
}

const NOW: &str = "2026-08-24T00:00:00Z";

impl TestWorkspace {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("july-cli-repl-{}", ulid::Ulid::generate()));
        std::fs::create_dir(&root).unwrap();
        let database = root.join("workspace.db");
        Self { root, database }
    }

    fn run(&self, input: &str, args: &[&str]) -> Output {
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

    fn repl(&self, input: &str) -> Output {
        self.run(input, &[])
    }

    #[cfg(unix)]
    fn spawn_repl(&self) -> Child {
        Command::new(env!("CARGO_BIN_EXE_july"))
            .env("JULY_WORKSPACE_DB", self.database.file_name().unwrap())
            .current_dir(&self.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    }

    fn rooms(&self) -> i64 {
        Connection::open(&self.database)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM rooms", [], |row| row.get(0))
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

    fn add_member(&self, room: &Room, agent: &Agent) {
        SqliteStore::open(&self.database)
            .unwrap()
            .add_room_member(room.id, agent.id, None, NOW)
            .unwrap();
    }

    fn remove_member(&self, room: &Room, agent: &Agent) {
        SqliteStore::open(&self.database)
            .unwrap()
            .remove_room_member(room.id, agent.id, NOW)
            .unwrap();
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

#[cfg(unix)]
fn read_prompt(child: &mut Child) {
    let mut prompt = [0; 2];
    child
        .stdout
        .as_mut()
        .unwrap()
        .read_exact(&mut prompt)
        .unwrap();
    assert_eq!(&prompt, b"> ");
}

#[cfg(unix)]
fn wait_for_exit(child: &mut Child, timeout: Duration) -> Option<ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return Some(status);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn repl_root_commands_are_nonfatal_and_do_not_mutate_rooms() {
    let workspace = TestWorkspace::new();

    let output = workspace.repl("/status\n\nwords\n/back\n/members\n/dm codex\n/quit\n");

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert_eq!(stdout(&output).matches("> ").count(), 7);
    assert!(stdout(&output).contains("root\n"));
    assert_eq!(stderr(&output).matches("invalid command\n").count(), 2);
    assert!(stderr(&output).contains("already at root\n"));
    assert!(stderr(&output).contains("members unavailable at root\n"));
    assert_eq!(workspace.rooms(), 0);
}

#[test]
fn repl_eof_exits_cleanly_and_json_is_still_usage() {
    let workspace = TestWorkspace::new();

    let eof = workspace.repl("");
    assert!(eof.status.success(), "stderr: {}", stderr(&eof));
    assert_eq!(stdout(&eof), "> ");
    assert!(stderr(&eof).is_empty());

    let json_workspace = TestWorkspace::new();
    let json = json_workspace.run("", &["--json"]);
    assert!(!json.status.success());
    assert!(stdout(&json).is_empty());
    assert!(stderr(&json).contains("\"code\":\"usage\""));
    assert!(!json_workspace.database.exists());
}

#[test]
fn repl_room_stack_restores_context_and_lists_only_active_members() {
    let workspace = TestWorkspace::new();
    let payments = workspace.seed_room("Payments");
    let operations = workspace.seed_room("Operations");
    let active = workspace.seed_agent("Active");
    let left = workspace.seed_agent("Left");
    workspace.add_member(&payments, &active);
    workspace.add_member(&payments, &left);
    workspace.remove_member(&payments, &left);

    let output = workspace.repl(&format!(
        "/room Payments\n/status\n/room Operations\n/status\n/back\n/status\n/room {}\n/back\n/room missing\n/status\n/members\n/quit\n",
        payments.id
    ));

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert_eq!(
        stdout(&output)
            .matches(&format!("room\t{}\tPayments\n", payments.id))
            .count(),
        7
    );
    assert_eq!(
        stdout(&output)
            .matches(&format!("room\t{}\tOperations\n", operations.id))
            .count(),
        2
    );
    assert!(stderr(&output).contains("room missing does not exist\n"));
    assert!(stdout(&output).contains(&format!(
        "{}\t{}\t\t1\t{NOW}\t\tactive\n",
        payments.id, active.id
    )));
    assert!(!stdout(&output).contains(&left.id.to_string()));
    assert_eq!(workspace.rooms(), 2);
}

#[cfg(unix)]
#[test]
fn repl_sigint_exits_while_stdin_remains_open() {
    let workspace = TestWorkspace::new();
    let mut child = workspace.spawn_repl();
    read_prompt(&mut child);

    assert!(
        Command::new("kill")
            .args(["-INT", &child.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    let status = wait_for_exit(&mut child, Duration::from_millis(500));
    if status.is_none() {
        child.kill().unwrap();
        child.wait().unwrap();
    }
    assert!(status.is_some(), "REPL did not exit after SIGINT");
    assert!(status.unwrap().success());
}

#[cfg(unix)]
#[test]
fn repl_broken_stdout_exits_without_a_panic() {
    let workspace = TestWorkspace::new();
    let mut child = workspace.spawn_repl();
    read_prompt(&mut child);
    drop(child.stdout.take());
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"/status\n")
        .unwrap();
    drop(child.stdin.take());

    let status = wait_for_exit(&mut child, Duration::from_millis(500));
    if status.is_none() {
        child.kill().unwrap();
        child.wait().unwrap();
    }
    assert_eq!(status.unwrap().code(), Some(1));
}
