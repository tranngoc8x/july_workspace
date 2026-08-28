use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use july_workspace::application::ChatEvent;
use july_workspace::cli::InactiveTuiBridge;
use july_workspace::domain::{
    Agent, AgentId, Conversation, ConversationId, ConversationKind, ResultId, Room, RoomId,
    WorkItem, WorkItemId, WorkResult, WorkStatus,
};
use july_workspace::storage::SqliteStore;
use july_workspace::tui::app::{App, AppCommand, AppEvent, CommandResult, Context};
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

    fn seed_thread(&self, room: &Room, title: &str, agents: &[&Agent]) -> ConversationId {
        let thread = Conversation {
            id: ConversationId::new(),
            kind: ConversationKind::Thread,
            room_id: Some(room.id),
            title: Some(title.into()),
            goal: None,
            parent_conversation_id: None,
            origin_conversation_id: None,
            status: "open".into(),
            created_at: NOW.into(),
            updated_at: NOW.into(),
        };
        SqliteStore::open(&self.database)
            .unwrap()
            .create_thread_with_primary_work(
                &thread,
                WorkItemId::new(),
                "local-user",
                &agents.iter().map(|agent| agent.id).collect::<Vec<_>>(),
            )
            .unwrap();
        thread.id
    }

    /// An accepted Result in its own source Conversation, ready to publish.
    fn seed_result(&self) -> ResultId {
        let source = Conversation {
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
        };
        SqliteStore::open(&self.database)
            .unwrap()
            .insert_conversation(&source)
            .unwrap();
        self.seed_result_in(source.id)
    }

    /// An accepted Result on a new work item inside an existing Conversation.
    fn seed_result_in(&self, conversation_id: ConversationId) -> ResultId {
        let work = WorkItem {
            id: WorkItemId::new(),
            conversation_id,
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
        store.insert_work_item(&work).unwrap();
        store
            .transition_work(work.id, WorkStatus::Working, NOW)
            .unwrap();
        store.create_work_result(&result).unwrap();
        result.id
    }

    /// Link two Threads by a work dependency: the upstream Thread's work feeds
    /// the downstream Thread's work, which is what `/publish` resolves.
    fn link_threads(&self, upstream: ConversationId, downstream: ConversationId) {
        let mut store = SqliteStore::open(&self.database).unwrap();
        let upstream_work = store.list_work_items(upstream).unwrap()[0].id;
        let downstream_work = store.list_work_items(downstream).unwrap()[0].id;
        store
            .add_work_dependency(upstream_work, downstream_work, NOW)
            .unwrap();
    }

    fn messages(&self, conversation_id: ConversationId) -> Vec<(String, String)> {
        Connection::open(&self.database)
            .unwrap()
            .prepare(
                "SELECT body, metadata_json FROM messages WHERE conversation_id = ? \
                 ORDER BY created_at, id",
            )
            .unwrap()
            .query_map([conversation_id.to_string()], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
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

fn tui_command(app: &mut App, input: &str) -> AppCommand {
    for character in input.chars() {
        app.reduce(AppEvent::Key(KeyEvent::new(
            KeyCode::Char(character),
            KeyModifiers::NONE,
        )));
    }
    app.reduce(AppEvent::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    )))
    .pop()
    .expect("non-blank input emits one command")
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
    assert!(
        stderr(&output)
            .contains("/members is unavailable in root context (available in: room, thread)\n")
    );
    assert_eq!(workspace.rooms(), 0);
}

#[test]
fn repl_help_is_context_aware_and_explains_one_command() {
    let workspace = TestWorkspace::new();
    let room = workspace.seed_room("Operations");
    let codex = workspace.seed_acp_agent("codex", &[]);
    workspace.add_member(&room, &codex);
    let settlement = workspace.seed_thread(&room, "Settlement", &[&codex]);

    let output = workspace.repl(&format!(
        "/help\n/help thread\n/help nope\n/room Operations\n/thread {settlement} --agent codex\n\
         /help\n/quit\n"
    ));

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let stdout_output = stdout(&output);
    let (root_help, thread_help) = stdout_output.split_once("room\t").unwrap();
    // Root help offers navigation but not Thread-only inspection.
    assert!(root_help.contains("/dm <agent>"));
    assert!(!root_help.contains("/work"));
    assert!(!root_help.contains("/members"));
    // Thread help adds the Thread surface.
    assert!(thread_help.contains("/work"));
    assert!(thread_help.contains("/results"));
    assert!(thread_help.contains("/publish <result> [--to <thread>]"));
    // Detailed help comes from the same registry metadata.
    assert!(stdout_output.contains("usage\n  /thread <thread> [--agent <agent>]"));
    assert!(stdout_output.contains("contexts\n  room, thread"));
    assert!(stderr(&output).contains("unknown command: nope\n"));
}

#[test]
fn repl_agents_is_inspection_only_and_guides_onboarding_when_empty() {
    let workspace = TestWorkspace::new();

    let empty = workspace.repl("/agents\n/agents add cashpoint\n/quit\n");
    assert!(empty.status.success(), "stderr: {}", stderr(&empty));
    assert!(stderr(&empty).contains(
        "no agents configured; add one with: \
         july agent add <name> --project <path> --adapter <id>\n"
    ));
    // `/agents` never mutates: the add form is rejected, not interpreted.
    assert_eq!(stderr(&empty).matches("invalid command\n").count(), 1);

    let codex = workspace.seed_acp_agent("codex", &[]);
    let listed = workspace.repl("/agents\n/quit\n");
    assert!(stdout(&listed).contains(&format!("{}\tcodex\t", codex.id)));
    // Listing an agent starts no session.
    assert_eq!(
        Connection::open(&workspace.database)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM session_bindings", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn repl_exits_on_the_canonical_exit_and_its_quit_alias() {
    for line in ["/exit\n", "/quit\n"] {
        let workspace = TestWorkspace::new();

        let output = workspace.repl(line);

        assert!(output.status.success(), "stderr: {}", stderr(&output));
        assert!(stderr(&output).is_empty());
        assert_eq!(stdout(&output), "> ");
    }
}

#[test]
fn repl_thread_new_creates_a_thread_in_the_current_room() {
    let workspace = TestWorkspace::new();
    let room = workspace.seed_room("Operations");

    let output = workspace.repl(
        "/thread new \"Refund flow\"\n/room Operations\n\
         /thread new \"Refund flow\" --goal \"Implement refund API\"\n/thread new\n/quit\n",
    );

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(
        stderr(&output)
            .contains("/thread new is unavailable in root context (available in: room, thread)\n")
    );
    assert_eq!(stderr(&output).matches("invalid command\n").count(), 1);

    let connection = Connection::open(&workspace.database).unwrap();
    let (title, goal, thread_room): (String, String, String) = connection
        .query_row(
            "SELECT title, goal, room_id FROM conversations WHERE type = 'thread'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(title, "Refund flow");
    assert_eq!(goal, "Implement refund API");
    assert_eq!(thread_room, room.id.to_string());
}

#[test]
fn repl_inspection_commands_report_workspace_state_without_mutating_it() {
    let workspace = TestWorkspace::new();
    let room = workspace.seed_room("Operations");
    let codex = workspace.seed_acp_agent("codex", &[]);
    workspace.add_member(&room, &codex);
    let settlement = workspace.seed_thread(&room, "Settlement", &[&codex]);
    let result_id = workspace.seed_result_in(settlement);

    let output = workspace.repl(&format!(
        "/rooms\n/agents\n/work\n/room Operations\n/thread {settlement} --agent codex\n\
         /work\n/results\n/quit\n"
    ));

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let stdout_output = stdout(&output);
    assert!(stdout_output.contains(&format!("{}\tOperations\tactive\n", room.id)));
    assert!(stdout_output.contains(&format!("{}\tcodex\t", codex.id)));
    assert!(
        stderr(&output).contains("/work is unavailable in root context (available in: thread)\n")
    );
    assert!(stdout_output.contains("\tSettlement\t"));
    assert!(stdout_output.contains(&format!("{result_id}\t")));
    // Inspection is read-only.
    assert_eq!(workspace.rooms(), 1);
    assert!(workspace.messages(settlement).is_empty());
}

#[test]
fn repl_restart_rebinds_the_conversation_without_leaving_it() {
    let workspace = TestWorkspace::new();
    workspace.seed_acp_agent("codex", &[]);

    let output = workspace.repl("/dm codex\nhello\n1\n/restart\n/status\n/quit\n");

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    // `/restart` reports the restored context, so `/status` still shows the DM.
    assert_eq!(stdout(&output).matches("\tcodex\t").count(), 2);

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
    assert!(
        stderr(&output)
            .contains("/members is unavailable in dm context (available in: room, thread)\n")
    );
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

#[test]
fn repl_thread_context_keeps_dm_and_thread_transcripts_separate() {
    let workspace = TestWorkspace::new();
    let room = workspace.seed_room("Operations");
    let codex = workspace.seed_acp_agent("codex", &[]);
    workspace.add_member(&room, &codex);
    let settlement = workspace.seed_thread(&room, "Settlement", &[&codex]);
    let refunds = workspace.seed_thread(&room, "Refunds", &[&codex]);

    let output = workspace.repl(&format!(
        // A Thread is entered from its Room: `/thread` is unavailable in a DM.
        "/room Operations\n/dm codex\ndm one\n1\n/back\n/thread {settlement} --agent codex\n\
         thread one\n1\n/status\n/members\n/back\n/status\n\
         /thread {refunds} --agent codex\nthread two\n1\n/back\n/quit\n"
    ));

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let stdout_output = stdout(&output);
    assert!(stdout_output.contains(&format!("thread\t{settlement}\tcodex\n")));
    assert!(stdout_output.contains(&format!("thread\t{refunds}\tcodex\n")));

    let dm_conversation: String = Connection::open(&workspace.database)
        .unwrap()
        .query_row(
            "SELECT id FROM conversations WHERE type = 'dm'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let dm_messages = workspace.messages(dm_conversation.parse().unwrap());
    assert_eq!(
        dm_messages
            .iter()
            .map(|(body, _)| body.as_str())
            .collect::<Vec<_>>(),
        ["dm one", "fixture reply"]
    );
    assert!(
        dm_messages
            .iter()
            .all(|(_, metadata)| metadata.contains("\"channel\":\"dm\""))
    );

    for (thread_id, sent) in [(settlement, "thread one"), (refunds, "thread two")] {
        let messages = workspace.messages(thread_id);
        assert_eq!(
            messages
                .iter()
                .map(|(body, _)| body.as_str())
                .collect::<Vec<_>>(),
            [sent, "fixture reply"],
            "thread {thread_id} transcript"
        );
        assert!(
            messages
                .iter()
                .all(|(_, metadata)| metadata.contains("\"channel\":\"thread\""))
        );
    }

    // `/status` in the Thread reports the Thread binding, not the DM one.
    let status = stdout_output
        .lines()
        .find(|line| {
            let fields: Vec<_> = line.split('\t').collect();
            fields.len() == 5
                && fields[0].ends_with("thread")
                && fields[1] == settlement.to_string()
        })
        .unwrap_or_else(|| panic!("missing thread status in stdout: {stdout_output}"));
    let fields: Vec<_> = status.split('\t').collect();
    assert_eq!(fields[2], "codex");
    assert!(!fields[3].is_empty());
    assert_eq!(fields[4], "active");

    // `/members` lists the Thread's active members only.
    assert!(stdout_output.contains(&format!("{settlement}\tagent\t{}\t1\t", codex.id)));
    assert!(stdout_output.contains(&format!("{settlement}\tuser\tlocal-user\t1\t")));

    let connection = Connection::open(&workspace.database).unwrap();
    for conversation in [settlement.to_string(), refunds.to_string(), dm_conversation] {
        let bindings: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM session_bindings WHERE conversation_id = ?",
                [&conversation],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(bindings, 1, "conversation {conversation} binding count");
    }
}

#[test]
fn repl_thread_switch_failures_leave_the_previous_context_active() {
    let workspace = TestWorkspace::new();
    let operations = workspace.seed_room("Operations");
    let payments = workspace.seed_room("Payments");
    let codex = workspace.seed_acp_agent("codex", &[]);
    let outsider = workspace.seed_acp_agent("outsider", &[]);
    workspace.add_member(&operations, &codex);
    workspace.add_member(&payments, &codex);
    let settlement = workspace.seed_thread(&payments, "Settlement", &[&codex]);

    let output = workspace.repl(&format!(
        "/room Operations\n/thread {settlement} --agent codex\n/status\n\
         /thread {settlement}\n/thread not-an-id --agent codex\n\
         /thread {settlement} --agent missing\n/thread {settlement} --bogus codex\n/status\n\
         /room Payments\n/thread {settlement} --agent outsider\n/status\n/back\n/status\n/quit\n"
    ));

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let stderr_output = stderr(&output);
    // Both the explicit and the deterministic `/thread` form check the Room.
    assert_eq!(
        stderr_output
            .matches(&format!(
                "thread {settlement} does not belong to room {}",
                operations.id
            ))
            .count(),
        2
    );
    assert_eq!(stderr_output.matches("invalid command\n").count(), 1);
    assert!(stderr_output.contains("usage: july dm <agent>"));
    assert!(stderr_output.contains("agent missing does not exist"));
    assert!(stderr_output.contains(&format!(
        "agent {} must be an active member of room",
        outsider.id
    )));
    // Every failed switch kept the previous descriptor.
    assert_eq!(
        stdout(&output)
            .matches(&format!("room\t{}\tOperations\n", operations.id))
            .count(),
        5
    );
    assert!(workspace.messages(settlement).is_empty());
}

#[tokio::test]
async fn inactive_tui_bridge_reuses_repl_navigation_exact_chat_and_raw_permission_event() {
    let workspace = TestWorkspace::new();
    let room = workspace.seed_room("vna");
    let foreign_room = workspace.seed_room("foreign");
    let agent = workspace.seed_agent("codex");
    workspace.add_member(&room, &agent);
    workspace.add_member(&foreign_room, &agent);
    let thread = workspace.seed_thread(&room, "work", &[&agent]);
    let foreign_thread = workspace.seed_thread(&foreign_room, "foreign work", &[&agent]);

    let legacy = workspace.repl("/exit\n");
    assert!(legacy.status.success(), "stderr: {}", stderr(&legacy));
    assert_eq!(stdout(&legacy), "> ");

    let mut bridge = InactiveTuiBridge::open(&workspace.database).await.unwrap();
    let mut app = App::new(Context::root());

    for (input, expected_label) in [("/room vna", "room · vna"), ("/dm codex", "dm · codex")] {
        let event = bridge.execute(tui_command(&mut app, input)).await.unwrap();
        app.reduce(event);
        assert_eq!(app.context().label(), expected_label);
    }

    let active_dm = app.context().clone();
    let failed = bridge
        .execute(tui_command(&mut app, "/dm missing"))
        .await
        .unwrap();
    assert!(matches!(
        &failed,
        AppEvent::CommandFinished {
            result: CommandResult::Failed(_),
            ..
        }
    ));
    app.reduce(failed);
    assert_eq!(app.context(), &active_dm);

    for (input, expected_label) in [
        ("/back".to_owned(), "room · vna"),
        (format!("/thread {thread} --agent codex"), "thread · codex"),
    ] {
        let event = bridge.execute(tui_command(&mut app, &input)).await.unwrap();
        app.reduce(event);
        assert_eq!(app.context().label(), expected_label);
    }

    let active_thread = app.context().clone();
    let failed = bridge
        .execute(tui_command(
            &mut app,
            &format!("/thread {foreign_thread} --agent codex"),
        ))
        .await
        .unwrap();
    assert!(matches!(
        &failed,
        AppEvent::CommandFinished {
            result: CommandResult::Failed(_),
            ..
        }
    ));
    app.reduce(failed);
    assert_eq!(app.context(), &active_thread);

    let exact = "/provider-command  keep  spacing";
    let submitted = bridge.execute(tui_command(&mut app, exact)).await.unwrap();
    assert!(matches!(
        &submitted,
        AppEvent::CommandFinished {
            result: CommandResult::Submitted,
            ..
        }
    ));
    app.reduce(submitted);

    let permission = bridge.next_event().await.unwrap().unwrap();
    assert!(matches!(
        &permission,
        AppEvent::Chat(ChatEvent::PermissionRequested { .. })
    ));
    app.reduce(permission);

    let persisted: i64 = Connection::open(&workspace.database)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE body = ?1",
            [exact],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(persisted, 1);

    let connection = Connection::open(&workspace.database).unwrap();
    let bindings = connection
        .prepare("SELECT id, conversation_id FROM session_bindings WHERE agent_id = ?1 ORDER BY id")
        .unwrap()
        .query_map([agent.id.to_string()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(bindings.len(), 2);
    assert_ne!(bindings[0].0, bindings[1].0);
    assert_ne!(bindings[0].1, bindings[1].1);

    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn inactive_tui_bridge_exposes_successful_command_output_to_app() {
    let workspace = TestWorkspace::new();
    workspace.seed_agent("codex");
    let mut bridge = InactiveTuiBridge::open(&workspace.database).await.unwrap();
    let mut app = App::new(Context::root());

    let opened = bridge
        .execute(tui_command(&mut app, "/dm codex"))
        .await
        .unwrap();
    app.reduce(opened);
    let active_dm = app.context().clone();

    let status = bridge
        .execute(tui_command(&mut app, "/status"))
        .await
        .unwrap();
    app.reduce(status);

    assert_eq!(app.context(), &active_dm);
    assert!(
        app.status()
            .is_some_and(|status| status.starts_with("dm\t") && status.contains("\tcodex\t")),
        "successful /status output was not visible: {:?}",
        app.status()
    );
    assert!(!app.turn_active());
    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn inactive_tui_bridge_buffers_raw_events_while_execute_waits_for_its_completion() {
    let workspace = TestWorkspace::new();
    workspace.seed_acp_agent("codex", &["--no-permission"]);
    let mut bridge = InactiveTuiBridge::open(&workspace.database).await.unwrap();
    let mut app = App::new(Context::root());

    let opened = bridge
        .execute(tui_command(&mut app, "/dm codex"))
        .await
        .unwrap();
    app.reduce(opened);
    let submitted = bridge
        .execute(tui_command(&mut app, "exact buffered turn"))
        .await
        .unwrap();
    app.reduce(submitted);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let origin = app.context().id().clone();
    let _rooms = bridge
        .execute(AppCommand::Execute {
            context: origin,
            input: "/rooms".into(),
        })
        .await
        .unwrap();

    let mut buffered = Vec::new();
    for _ in 0..3 {
        buffered.push(
            tokio::time::timeout(Duration::from_millis(250), bridge.next_event())
                .await
                .expect("execute discarded an earlier queued event")
                .unwrap()
                .unwrap(),
        );
    }
    assert!(matches!(
        &buffered[0],
        AppEvent::Chat(ChatEvent::TextDelta(text)) if text == "fixture reply"
    ));
    assert!(matches!(
        &buffered[1],
        AppEvent::Chat(ChatEvent::MessageCompleted(_))
    ));
    assert!(matches!(
        &buffered[2],
        AppEvent::Chat(ChatEvent::TurnCompleted)
    ));

    bridge.shutdown().await.unwrap();
}

#[test]
fn repl_publish_resolves_the_single_downstream_target() {
    let workspace = TestWorkspace::new();
    let room = workspace.seed_room("Operations");
    let codex = workspace.seed_acp_agent("codex", &[]);
    workspace.add_member(&room, &codex);
    let settlement = workspace.seed_thread(&room, "Settlement", &[&codex]);
    let payments = workspace.seed_thread(&room, "Payments", &[&codex]);
    workspace.link_threads(settlement, payments);
    let result_id = workspace.seed_result();

    let output = workspace.repl(&format!(
        "/publish {result_id}\n/room Operations\n/publish {result_id}\n\
         /thread {settlement} --agent codex\n/publish not-a-result\n/publish {result_id}\n/quit\n"
    ));

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(
        stderr(&output)
            .contains("/publish is unavailable in root context (available in: thread)\n")
    );
    assert!(
        stderr(&output)
            .contains("/publish is unavailable in room context (available in: thread)\n")
    );
    assert!(stderr(&output).contains("usage: july dm <agent>"));
    let published = stdout(&output)
        .lines()
        .find(|line| line.contains(&result_id.to_string()))
        .map(str::to_owned)
        .unwrap_or_else(|| panic!("missing publish output: {}", stdout(&output)));
    let fields: Vec<_> = published.split('\t').collect();
    assert_eq!(fields.len(), 5);
    assert_eq!(fields[1], result_id.to_string());
    // The target is the linked downstream Thread, never the current one.
    assert_eq!(fields[3], payments.to_string());

    let connection = Connection::open(&workspace.database).unwrap();
    let (publishes, target): (i64, String) = connection
        .query_row(
            "SELECT COUNT(*), MAX(target_conversation_id) FROM publishes",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(publishes, 1);
    assert_eq!(target, payments.to_string());
}

#[test]
fn repl_publish_reports_missing_targets_and_requires_to_when_ambiguous() {
    let workspace = TestWorkspace::new();
    let room = workspace.seed_room("Operations");
    let codex = workspace.seed_acp_agent("codex", &[]);
    workspace.add_member(&room, &codex);
    let settlement = workspace.seed_thread(&room, "Settlement", &[&codex]);
    let payments = workspace.seed_thread(&room, "Payments", &[&codex]);
    let refunds = workspace.seed_thread(&room, "Refunds", &[&codex]);
    let unlinked = workspace.seed_thread(&room, "Unlinked", &[&codex]);
    let result_id = workspace.seed_result();

    // No downstream link at all.
    let output = workspace.repl(&format!(
        "/room Operations\n/thread {unlinked} --agent codex\n/publish {result_id}\n/quit\n"
    ));
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stderr(&output).contains(&format!(
        "conversation {unlinked} has no downstream publish target\n"
    )));

    // Two downstream links: ambiguous until `--to` names one.
    workspace.link_threads(settlement, payments);
    workspace.link_threads(settlement, refunds);
    let output = workspace.repl(&format!(
        "/room Operations\n/thread {settlement} --agent codex\n/publish {result_id}\n\
         /publish {result_id} --to {refunds}\n/quit\n"
    ));
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stderr(&output).contains(&format!(
        "conversation {settlement} has 2 downstream publish targets; use --to <target>\n"
    )));

    let connection = Connection::open(&workspace.database).unwrap();
    let (publishes, target): (i64, String) = connection
        .query_row(
            "SELECT COUNT(*), MAX(target_conversation_id) FROM publishes",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(publishes, 1);
    assert_eq!(target, refunds.to_string());
}

#[test]
fn thread_open_streams_one_turn_and_detaches_on_exit() {
    let workspace = TestWorkspace::new();
    let room = workspace.seed_room("Operations");
    let codex = workspace.seed_acp_agent("codex", &[]);
    workspace.add_member(&room, &codex);
    let settlement = workspace.seed_thread(&room, "Settlement", &[&codex]);

    let output = workspace.run(
        "hello thread\n1\n/quit\n",
        &[
            "thread",
            "open",
            &settlement.to_string(),
            "--agent",
            "codex",
        ],
    );

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("[codex] > "));
    assert_eq!(
        workspace
            .messages(settlement)
            .iter()
            .map(|(body, _)| body.as_str())
            .collect::<Vec<_>>(),
        ["hello thread", "fixture reply"]
    );
    let binding: String = Connection::open(&workspace.database)
        .unwrap()
        .query_row(
            "SELECT status FROM session_bindings WHERE conversation_id = ?",
            [settlement.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(binding, "disconnected");
}

#[test]
fn thread_open_rejects_json_framing_and_malformed_grammar() {
    for args in [
        ["thread", "open"].as_slice(),
        ["thread", "open", "01ARZ3NDEKTSV4RRFFQ69G5FAV"].as_slice(),
        ["thread", "open", "01ARZ3NDEKTSV4RRFFQ69G5FAV", "--agent"].as_slice(),
        [
            "thread",
            "open",
            "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "--agent",
            "codex",
            "--json",
        ]
        .as_slice(),
        ["thread", "open", "not-an-id", "--agent", "codex"].as_slice(),
    ] {
        let workspace = TestWorkspace::new();
        let output = workspace.run("", args);
        assert!(!output.status.success());
        assert!(stderr(&output).contains("usage: july dm <agent>"));
        assert!(!workspace.database.exists(), "args: {args:?}");
    }
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
fn repl_sigint_during_thread_permission_cancels_the_turn_and_keeps_the_context() {
    let workspace = TestWorkspace::new();
    let room = workspace.seed_room("Operations");
    let codex = workspace.seed_acp_agent("codex", &[]);
    workspace.add_member(&room, &codex);
    let settlement = workspace.seed_thread(&room, "Settlement", &[&codex]);
    let mut child = workspace.spawn_repl();
    let output = ChildOutput::take(&mut child);
    assert!(output.read_until(b"> ").ends_with("> "));

    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(
            format!("/room Operations\n/thread {settlement} --agent codex\nhello\n").as_bytes(),
        )
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
    assert!(status.contains(&format!("thread\t{settlement}\tcodex\t")));

    let exit = wait_for_exit(&mut child, Duration::from_secs(2));
    if exit.is_none() {
        child.kill().unwrap();
        child.wait().unwrap();
    }
    assert!(exit.is_some(), "REPL did not exit after /quit");
    assert!(exit.unwrap().success());
    let binding: String = Connection::open(&workspace.database)
        .unwrap()
        .query_row(
            "SELECT status FROM session_bindings WHERE conversation_id = ?",
            [settlement.to_string()],
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
