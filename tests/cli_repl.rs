use july_workspace::domain::{Agent, AgentId, Room, RoomId};
use july_workspace::storage::SqliteStore;
use rusqlite::Connection;
use serde_json::json;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::mpsc::{self, Receiver};
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
        self.seed_acp_agent(name, &[])
    }

    fn seed_acp_agent(&self, name: &str, arguments: &[&str]) -> Agent {
        let agent = Agent {
            id: AgentId::new(),
            name: name.into(),
            project_root: self.root.to_string_lossy().into_owned(),
            transport_type: "acp".into(),
            transport_config: json!({
                "executable": "/usr/bin/python3",
                "arguments": std::iter::once(
                    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .join("tests/fixtures/acp_agent.py")
                        .to_string_lossy()
                        .into_owned(),
                ).chain(arguments.iter().map(|argument| (*argument).into())).collect::<Vec<_>>(),
                "environment": {},
                "state_directory": self.root,
                "expected_agent_name": if arguments.contains(&"--claude") { "claude-test" } else { "test-acp-agent" },
                "expected_agent_version": "1.0.0",
            }),
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
struct ChildOutput {
    bytes: Receiver<u8>,
}

#[cfg(unix)]
impl ChildOutput {
    fn take(child: &mut Child) -> Self {
        let stdout = child.stdout.take().unwrap();
        let (sender, bytes) = mpsc::channel();
        std::thread::spawn(move || {
            for byte in std::io::BufReader::new(stdout).bytes() {
                let Ok(byte) = byte else {
                    return;
                };
                if sender.send(byte).is_err() {
                    return;
                }
            }
        });
        Self { bytes }
    }

    fn read_until(&self, needle: &[u8]) -> String {
        let mut output = Vec::new();
        loop {
            let byte = self
                .bytes
                .recv_timeout(Duration::from_secs(2))
                .expect("REPL did not produce expected output");
            output.push(byte);
            if output.ends_with(needle) {
                return String::from_utf8(output).unwrap();
            }
        }
    }
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
    assert_eq!(stderr(&output).matches("invalid command\n").count(), 1);
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

#[test]
fn repl_switches_agents_without_merging_dm_history_or_bindings() {
    let workspace = TestWorkspace::new();
    let room = workspace.seed_room("Operations");
    let codex = workspace.seed_acp_agent("codex", &[]);
    let claude = workspace.seed_acp_agent("claude", &["--claude", "--claude-mode"]);

    let output = workspace.repl(
        "/room Operations\n/dm codex\none\n1\n/dm claude\ntwo\n1\n/back\n/status\nsecond\n1\n/back\n/status\n/quit\n",
    );

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let stderr_output = stderr(&output);
    let output = stdout(&output);
    assert!(output.contains("dm\t"));
    assert!(output.contains("codex"));
    assert!(
        output.contains("claude"),
        "stdout: {}; stderr: {}",
        output,
        stderr_output
    );

    let connection = Connection::open(&workspace.database).unwrap();
    let mut conversations = connection
        .prepare(
            "SELECT c.id, cm.member_id FROM conversations c \
             JOIN conversation_members cm ON cm.conversation_id = c.id \
             WHERE c.type = 'dm' AND cm.member_type = 'agent' ORDER BY cm.member_id",
        )
        .unwrap();
    let conversations = conversations
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(conversations.len(), 2);

    for (conversation_id, agent_id) in conversations {
        let messages: Vec<String> = connection
            .prepare("SELECT body FROM messages WHERE conversation_id = ? ORDER BY created_at, id")
            .unwrap()
            .query_map([&conversation_id], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        if agent_id == codex.id.to_string() {
            assert_eq!(
                messages,
                ["one", "fixture reply", "second", "fixture reply"]
            );
        } else {
            assert_eq!(agent_id, claude.id.to_string());
            assert_eq!(messages, ["two", "fixture reply"]);
        }
        let bindings: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM session_bindings WHERE conversation_id = ? AND agent_id = ?",
                [&conversation_id, &agent_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(bindings, 1);
        let generation: i64 = connection
            .query_row(
                "SELECT generation FROM session_bindings WHERE conversation_id = ? AND agent_id = ?",
                [&conversation_id, &agent_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(generation, 1);
        let status: String = connection
            .query_row(
                "SELECT status FROM session_bindings WHERE conversation_id = ? AND agent_id = ? ORDER BY last_used_at DESC, id DESC LIMIT 1",
                [&conversation_id, &agent_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "disconnected");
    }
    let dm_status = output
        .lines()
        .find(|line| {
            let fields: Vec<_> = line.split('\t').collect();
            fields.len() == 5 && fields[0].ends_with("dm") && fields[2] == "codex"
        })
        .unwrap_or_else(|| panic!("missing Codex DM status in stdout: {output}"));
    let fields: Vec<_> = dm_status.split('\t').collect();
    assert_eq!(fields.len(), 5);
    assert!(!fields[3].is_empty());
    assert_eq!(fields[4], "active");
    assert_eq!(
        output
            .matches(&format!("room\t{}\tOperations\n", room.id))
            .count(),
        3
    );
}

#[test]
fn repl_dm_status_and_failed_switch_preserve_the_active_context() {
    let workspace = TestWorkspace::new();
    workspace.seed_acp_agent("codex", &[]);

    let output = workspace.repl("/dm codex\nhello\n1\n/members\n/dm missing\n/status\n/quit\n");

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stderr(&output).contains("members unavailable in dm\n"));
    assert!(stderr(&output).contains("agent missing does not exist\n"));
    assert!(stdout(&output).contains("dm\t"));
    assert!(stdout(&output).contains("\tcodex\t"));

    let connection = Connection::open(&workspace.database).unwrap();
    let conversations: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM conversations WHERE type = 'dm'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(conversations, 1);
}

#[test]
fn repl_dm_rejects_malformed_known_commands_but_sends_unknown_slashes_exactly() {
    let workspace = TestWorkspace::new();
    workspace.seed_acp_agent("codex", &[]);

    let output = workspace.repl(
        "/dm codex\n/dm\n/dm \n/room \n/status extra\n /status\n1\n/unknown exact\n1\n/quit\n",
    );

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert_eq!(stderr(&output).matches("invalid command\n").count(), 4);
    let connection = Connection::open(&workspace.database).unwrap();
    let messages: Vec<String> = connection
        .prepare("SELECT body FROM messages ORDER BY created_at, id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(messages.len(), 4);
    assert!(messages.iter().any(|message| message == " /status"));
    assert!(messages.iter().any(|message| message == "/unknown exact"));
    assert_eq!(
        messages
            .iter()
            .filter(|message| message.as_str() == "fixture reply")
            .count(),
        2
    );
}

#[cfg(unix)]
#[test]
fn repl_sigint_during_dm_permission_returns_to_prompt_and_disconnects_on_exit() {
    let workspace = TestWorkspace::new();
    let codex = workspace.seed_acp_agent("codex", &[]);
    let mut child = workspace.spawn_repl();
    let output = ChildOutput::take(&mut child);
    assert!(output.read_until(b"> ").ends_with("> "));

    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"/dm codex\nhello\n")
        .unwrap();
    assert!(output.read_until(b"permission> ").ends_with("permission> "));
    assert!(
        Command::new("kill")
            .args(["-INT", &child.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    assert!(output.read_until(b"> ").ends_with("> "));
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"/status\n/quit\n")
        .unwrap();
    let status = output.read_until(b"active\n");
    assert!(status.contains("dm\t"));
    let fields: Vec<_> = status.trim_end().split('\t').collect();
    assert_eq!(fields.len(), 5);
    assert!(!fields[3].is_empty());
    assert_eq!(fields[4], "active");

    let status = wait_for_exit(&mut child, Duration::from_secs(2));
    if status.is_none() {
        child.kill().unwrap();
        child.wait().unwrap();
    }
    assert!(status.is_some(), "REPL did not exit after /quit");
    assert!(status.unwrap().success());
    let connection = Connection::open(&workspace.database).unwrap();
    let binding: String = connection
        .query_row(
            "SELECT status FROM session_bindings WHERE agent_id = ? ORDER BY last_used_at DESC, id DESC LIMIT 1",
            [codex.id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(binding, "disconnected");
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
    std::thread::sleep(Duration::from_millis(10));
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
