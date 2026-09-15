use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use july_workspace::application::ChatEvent;
use july_workspace::cli::InactiveTuiBridge;
use july_workspace::domain::WorkScope;
use july_workspace::domain::{
    Agent, AgentId, Conversation, ConversationId, ConversationKind, Decision, DecisionId,
    DecisionOwner, DecisionStatus, DecisionType, MemberType, Message, MessageId, ResultId, Room,
    RoomId, RoomMessage, RoomMessageId, WorkItem, WorkItemId, WorkResult, WorkStatus,
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

    fn seed_failed_dm_delivery(&self, body: &str) -> (MessageId, Agent) {
        let source = self.seed_acp_agent("delivery-source", &[]);
        let target = self.seed_acp_agent("delivery-target", &[]);
        let message_id = MessageId::new();
        let mut store = SqliteStore::open(&self.database).unwrap();
        store
            .persist_agent_direct_message(message_id, source.id, target.id, body, NOW)
            .unwrap();
        store
            .mark_delivery_failed(message_id, target.id, NOW)
            .unwrap();
        (message_id, target)
    }

    fn seed_failed_thread_delivery(
        &self,
        thread_id: ConversationId,
        source: &Agent,
        target: &Agent,
        body: &str,
    ) -> MessageId {
        let message = Message {
            id: MessageId::new(),
            conversation_id: thread_id,
            sender_type: MemberType::Agent,
            sender_id: source.id.to_string(),
            body: body.into(),
            reply_to: None,
            metadata: json!({"mention": target.id.to_string()}),
            created_at: NOW.into(),
        };
        let mut store = SqliteStore::open(&self.database).unwrap();
        store
            .insert_message_with_pending_delivery(&message, target.id, Some("capsule"))
            .unwrap();
        store
            .mark_delivery_capsule_delivered(message.id, target.id, NOW)
            .unwrap();
        store
            .mark_delivery_failed(message.id, target.id, NOW)
            .unwrap();
        message.id
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

    fn seed_decision(
        &self,
        thread_id: ConversationId,
        title: &str,
        owner: DecisionOwner,
    ) -> Decision {
        let decision = Decision {
            id: DecisionId::new(),
            thread_id,
            decision_type: DecisionType::Technical,
            title: title.into(),
            decision: None,
            reason: None,
            selected_proposal_id: None,
            alternatives: vec!["keep current contract".into(), "replace contract".into()],
            evidence: vec!["test:payment_contract".into()],
            participants: Vec::new(),
            decision_owner: owner,
            status: DecisionStatus::Pending,
            supersedes_decision_id: None,
            created_at: NOW.into(),
            updated_at: NOW.into(),
        };
        SqliteStore::open(&self.database)
            .unwrap()
            .record_decision(&decision)
            .unwrap();
        decision
    }

    fn seed_message(
        &self,
        conversation_id: ConversationId,
        sender_type: MemberType,
        sender_id: &str,
        body: &str,
        created_at: &str,
    ) {
        SqliteStore::open(&self.database)
            .unwrap()
            .insert_message(&Message {
                id: MessageId::new(),
                conversation_id,
                sender_type,
                sender_id: sender_id.into(),
                body: body.into(),
                reply_to: None,
                metadata: serde_json::Value::Null,
                created_at: created_at.into(),
            })
            .unwrap();
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
            scope: WorkScope::Conversation(conversation_id),
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

/// A prompt and the reply it draws are persisted independently, so their
/// storage order can interleave across turns. Assert the prompts in order and
/// the replies by count.
fn assert_turns(workspace: &TestWorkspace, conversation: ConversationId, prompts: &[&str]) {
    let messages = workspace.messages(conversation);
    let sent: Vec<&str> = messages
        .iter()
        .map(|(body, _)| body.as_str())
        .filter(|body| *body != "fixture reply")
        .collect();
    assert_eq!(sent, prompts, "prompts in {conversation}");
    assert_eq!(
        messages.len() - sent.len(),
        prompts.len(),
        "one reply per prompt in {conversation}"
    );
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
    for _ in 0..2 {
        let commands = app.reduce(AppEvent::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        match commands.as_slice() {
            [] => {}
            [command] => return command.clone(),
            _ => panic!("completion emitted {} commands", commands.len()),
        }
    }
    panic!("non-blank input did not emit a command after completion");
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
            .contains("/members is unavailable in root context (available in: room, work)\n")
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
    assert!(stdout_output.contains("contexts\n  room, work"));
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
    let listed = stdout(&listed);
    assert!(listed.contains("AGENT ID"));
    assert!(listed.contains("NAME"));
    assert!(listed.contains("PROJECT"));
    assert!(listed.contains("TRANSPORT"));
    assert!(listed.contains("STATUS\n"));
    assert!(listed.contains(&codex.id.to_string()));
    assert!(!listed.contains('\t'));
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
fn repl_lists_and_retries_failed_deliveries_without_changing_context() {
    let workspace = TestWorkspace::new();
    let (message_id, target) = workspace.seed_failed_dm_delivery("repl retry body");

    let output = workspace.repl(&format!(
        "/status\n/deliveries\n/delivery retry {message_id} --agent {}\n/status\n/deliveries\n/quit\n",
        target.name
    ));

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stderr(&output).is_empty(), "stderr: {}", stderr(&output));
    let output = stdout(&output);
    assert!(output.contains("MESSAGE ID"));
    assert!(output.contains(&message_id.to_string()));
    assert!(output.contains("repl retry body"));
    assert!(output.contains(&format!("delivered\t{message_id}\t{}", target.id)));
    assert!(output.contains("No failed deliveries."));
    assert_eq!(output.matches("root\n").count(), 2);
}

#[test]
fn repl_retries_a_delivery_for_the_current_thread_without_opening_a_second_context() {
    let workspace = TestWorkspace::new();
    let room = workspace.seed_room("Operations");
    let source = workspace.seed_acp_agent("thread-source", &[]);
    let target = workspace.seed_acp_agent("thread-target", &[]);
    workspace.add_member(&room, &source);
    workspace.add_member(&room, &target);
    let thread = workspace.seed_thread(&room, "Delivery retry", &[&source, &target]);
    let message_id =
        workspace.seed_failed_thread_delivery(thread, &source, &target, "current thread retry");

    let output = workspace.repl(&format!(
        "/room Operations\n/thread {thread} --agent {}\n/delivery retry {message_id} --agent {}\n1\n/status\n/quit\n",
        target.name, target.name
    ));

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stderr(&output).is_empty(), "stderr: {}", stderr(&output));
    let output = stdout(&output);
    assert!(output.contains(&format!("delivered\t{message_id}\t{}", target.id)));
    assert!(output.contains(&format!("work\t{thread}\t{}", target.name)));
}

#[test]
fn repl_detaches_a_temporary_retry_context_before_opening_that_thread() {
    let workspace = TestWorkspace::new();
    let room = workspace.seed_room("Operations");
    let source = workspace.seed_acp_agent("temp-source", &[]);
    let target = workspace.seed_acp_agent("temp-target", &[]);
    workspace.add_member(&room, &source);
    workspace.add_member(&room, &target);
    let thread = workspace.seed_thread(&room, "Temporary retry", &[&source, &target]);
    let message_id =
        workspace.seed_failed_thread_delivery(thread, &source, &target, "temporary retry");

    let output = workspace.repl(&format!(
        "/room Operations\n/delivery retry {message_id} --agent {}\n/thread {thread} --agent {}\n/status\n/quit\n",
        target.name, target.name
    ));

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stderr(&output).is_empty(), "stderr: {}", stderr(&output));
    let output = stdout(&output);
    assert!(output.contains(&format!("delivered\t{message_id}\t{}", target.id)));
    assert!(output.contains(&format!("work\t{thread}\t{}", target.name)));
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
            .contains("/thread new is unavailable in root context (available in: room, work)\n")
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
        "/rooms\n/agents\n/work\n/room Operations\n/members\n/work\n\
         /thread {settlement} --agent codex\n/members\n/work\n/results\n/quit\n"
    ));

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let stdout_output = stdout(&output);
    for header in [
        "ROOM ID",
        "AGENT ID",
        "THREAD ID",
        "WORK ID",
        "RESULT ID",
        "MEMBER ID",
    ] {
        assert!(stdout_output.contains(header), "missing header: {header}");
    }
    assert!(stdout_output.contains(&room.id.to_string()));
    assert!(stdout_output.contains(&codex.id.to_string()));
    assert!(
        stderr(&output)
            .contains("/work is unavailable in root context (available in: room, work)\n")
    );
    assert!(stdout_output.contains("Settlement"));
    assert!(stdout_output.contains(&result_id.to_string()));
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
    let stdout_output = stdout(&output);
    assert!(stdout_output.contains("ROOM ID"));
    assert!(stdout_output.contains("AGENT ID"));
    let active_member = stdout_output
        .lines()
        .find(|line| line.contains(&active.id.to_string()))
        .unwrap();
    assert!(active_member.starts_with(&payments.id.to_string()));
    assert!(active_member.contains(NOW));
    assert!(active_member.ends_with("active"));
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
            .contains("/members is unavailable in dm context (available in: room, work)\n")
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
fn repl_dm_accepts_an_at_prefix_and_sends_a_trailing_message_in_the_same_turn() {
    let workspace = TestWorkspace::new();
    workspace.seed_acp_agent("codex", &[]);

    let output = workspace.repl("/dm @codex con task nao mo khong\n1\n/quit\n");

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    // `@codex` mo dung agent thay vi tro thanh mot cai ten khong ton tai.
    assert!(stdout(&output).contains("dm\t"));
    assert!(stdout(&output).contains("\tcodex"));
    assert!(!stderr(&output).contains("does not exist"));

    let connection = Connection::open(&workspace.database).unwrap();
    let messages: Vec<String> = connection
        .prepare("SELECT body FROM messages ORDER BY created_at, id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    // Phan sau ten agent duoc gui nguyen van, khong bi nhet vao ten.
    assert!(
        messages.iter().any(|body| body == "con task nao mo khong"),
        "messages: {messages:?}"
    );
}

#[test]
fn repl_dm_rejects_malformed_known_commands_but_sends_unknown_slashes_exactly() {
    let workspace = TestWorkspace::new();
    workspace.seed_acp_agent("codex", &[]);

    let output = workspace
        .repl("/dm codex\n/dm\n/dm \n/room \n/status extra\n /status\n/unknown exact\n1\n/quit\n");

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
    assert_eq!(messages.len(), 2);
    // Leading blanks route to the command, so " /status" never reaches the agent.
    assert!(!messages.iter().any(|message| message.trim() == "/status"));
    assert!(messages.iter().any(|message| message == "/unknown exact"));
    assert_eq!(
        messages
            .iter()
            .filter(|message| message.as_str() == "fixture reply")
            .count(),
        1
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
    assert!(stdout_output.contains(&format!("work\t{settlement}\tcodex\n")));
    assert!(stdout_output.contains(&format!("work\t{refunds}\tcodex\n")));

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
            fields.len() == 5 && fields[0].ends_with("work") && fields[1] == settlement.to_string()
        })
        .unwrap_or_else(|| panic!("missing thread status in stdout: {stdout_output}"));
    let fields: Vec<_> = status.split('\t').collect();
    assert_eq!(fields[2], "codex");
    assert!(!fields[3].is_empty());
    assert_eq!(fields[4], "active");

    // `/members` lists the Thread's active members only.
    assert!(stdout_output.lines().any(|line| {
        line.starts_with(&settlement.to_string())
            && line.contains("agent")
            && line.contains(&codex.id.to_string())
            && line.ends_with("active")
    }));
    assert!(stdout_output.lines().any(|line| {
        line.starts_with(&settlement.to_string())
            && line.contains("user")
            && line.contains("local-user")
            && line.ends_with("active")
    }));

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
async fn inactive_tui_bridge_projects_visible_commands_for_root_room_dm_and_work() {
    const ROOT: &[&str] = &[
        "/dm",
        "/room",
        "/back",
        "/rooms",
        "/agents",
        "/deliveries",
        "/decisions",
        "/decision accept",
        "/decision reject",
        "/decision work",
        "/status",
        "/delivery retry",
        "/help",
        "/exit",
    ];
    const ROOM: &[&str] = &[
        "/dm",
        "/room",
        "/back",
        "/rooms",
        "/agents",
        "/deliveries",
        "/decisions",
        "/decision accept",
        "/decision reject",
        "/decision work",
        "/members",
        "/work",
        "/status",
        "/delivery retry",
        "/help",
        "/exit",
    ];
    const DM: &[&str] = &[
        "/dm",
        "/room",
        "/back",
        "/rooms",
        "/agents",
        "/deliveries",
        "/decisions",
        "/decision accept",
        "/decision reject",
        "/decision work",
        "/status",
        "/restart",
        "/delivery retry",
        "/help",
        "/exit",
    ];
    const WORK: &[&str] = &[
        "/dm",
        "/room",
        "/back",
        "/rooms",
        "/agents",
        "/deliveries",
        "/decisions",
        "/decision accept",
        "/decision reject",
        "/decision work",
        "/members",
        "/work assign",
        "/work status",
        "/work result",
        "/work",
        "/results",
        "/status",
        "/publish",
        "/restart",
        "/delivery retry",
        "/help",
        "/exit",
    ];

    let workspace = TestWorkspace::new();
    let room = workspace.seed_room("vna");
    let agent = workspace.seed_agent("codex");
    workspace.add_member(&room, &agent);
    let thread = workspace.seed_thread(&room, "work", &[&agent]);
    let mut bridge = InactiveTuiBridge::open(&workspace.database).await.unwrap();
    let mut app = App::new(bridge.initial_context());
    let names = |app: &App| app.context().commands().to_vec();

    assert_eq!(
        names(&app),
        ROOT.iter()
            .map(|name| (*name).to_owned())
            .collect::<Vec<_>>()
    );
    for (input, expected) in [
        ("/room vna".to_owned(), ROOM),
        ("/dm codex".to_owned(), DM),
        ("/back".to_owned(), ROOM),
        (format!("/thread {thread} --agent codex"), WORK),
    ] {
        let event = bridge.execute(tui_command(&mut app, &input)).await.unwrap();
        app.reduce(event);
        assert_eq!(
            names(&app),
            expected
                .iter()
                .map(|name| (*name).to_owned())
                .collect::<Vec<_>>()
        );
    }

    bridge.shutdown().await.unwrap();
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

    for (input, expected_label) in [
        ("/room vna", "room::vna"),
        ("/dm codex", "room::vna > dm::codex"),
    ] {
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
        ("/back".to_owned(), "room::vna"),
        (
            format!("/thread {thread} --agent codex"),
            "room::vna > thread::work > codex",
        ),
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
async fn inactive_tui_bridge_replaces_history_by_exact_conversation() {
    let workspace = TestWorkspace::new();
    let room = workspace.seed_room("vna");
    let agent = workspace.seed_agent("codex");
    workspace.add_member(&room, &agent);
    let thread = workspace.seed_thread(&room, "work", &[&agent]);
    let dm = SqliteStore::open(&workspace.database)
        .unwrap()
        .get_or_create_dm("local-user", agent.id, NOW)
        .unwrap();
    workspace.seed_message(
        dm.id,
        MemberType::User,
        "local-user",
        "dm-user",
        "2026-09-01T10:00:01Z",
    );
    workspace.seed_message(
        dm.id,
        MemberType::Agent,
        &agent.id.to_string(),
        "dm-agent",
        "2026-09-01T10:00:02Z",
    );
    workspace.seed_message(
        thread,
        MemberType::User,
        "local-user",
        "thread-user",
        "2026-09-01T10:00:03Z",
    );
    workspace.seed_message(
        thread,
        MemberType::Agent,
        &agent.id.to_string(),
        "thread-agent",
        "2026-09-01T10:00:04Z",
    );

    let mut bridge = InactiveTuiBridge::open(&workspace.database).await.unwrap();
    let mut app = App::new(bridge.initial_context());
    let event = bridge
        .execute(tui_command(&mut app, "/dm codex"))
        .await
        .unwrap();
    app.reduce(event);
    assert!(app.transcript().contains("dm-user"));
    assert!(app.transcript().contains("dm-agent"));
    assert!(!app.transcript().contains("thread-user"));

    for input in [
        "/back".to_owned(),
        "/room vna".to_owned(),
        format!("/thread {thread} --agent codex"),
    ] {
        let event = bridge.execute(tui_command(&mut app, &input)).await.unwrap();
        app.reduce(event);
    }
    assert!(app.transcript().contains("thread-user"));
    assert!(app.transcript().contains("thread-agent"));
    assert!(!app.transcript().contains("dm-user"));

    let event = bridge
        .execute(tui_command(&mut app, "/back"))
        .await
        .unwrap();
    app.reduce(event);
    assert!(app.transcript().is_empty());
    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn inactive_tui_bridge_hydrates_room_history_with_explicit_sender_labels() {
    let workspace = TestWorkspace::new();
    let room = workspace.seed_room("vna");
    let agent = workspace.seed_agent("codex");
    workspace.add_member(&room, &agent);
    let mut store = SqliteStore::open(&workspace.database).unwrap();
    for index in 0..=50 {
        let (sender_type, sender_id) = if index == 50 {
            (MemberType::Agent, agent.id.to_string())
        } else {
            (MemberType::User, "local-user".to_owned())
        };
        store
            .append_room_message(&RoomMessage {
                id: RoomMessageId::new(),
                room_id: room.id,
                sender_type,
                sender_id,
                body: format!("room-{index:02}"),
                mentions: vec![],
                reply_to: None,
                created_at: format!("2026-09-01T10:00:{index:02}Z"),
            })
            .unwrap();
    }
    drop(store);

    let mut bridge = InactiveTuiBridge::open(&workspace.database).await.unwrap();
    let mut app = App::new(bridge.initial_context());
    let event = bridge
        .execute(tui_command(&mut app, "/room vna"))
        .await
        .unwrap();
    assert!(matches!(
        event,
        AppEvent::CommandFinished {
            result: CommandResult::ContextWithHistory(_),
            ..
        }
    ));
    app.reduce(event);
    let transcript = app.transcript();
    assert!(transcript.starts_with("… showing 50 most recent messages …"));
    assert!(!transcript.contains("room-00"));
    assert!(transcript.contains("[user:local-user] room-01"));
    assert!(transcript.contains(&format!("[agent:{}] room-50", agent.id)));
    assert_eq!(transcript.matches("room-").count(), 50);
    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn inactive_tui_room_plain_submit_replaces_once_becomes_idle_and_accepts_a_second_submit() {
    let workspace = TestWorkspace::new();
    let room = workspace.seed_room("vna");
    let agent = workspace.seed_acp_agent("codex", &[]);
    workspace.add_member(&room, &agent);
    let mut bridge = InactiveTuiBridge::open(&workspace.database).await.unwrap();
    let mut app = App::new(bridge.initial_context());

    let opened = bridge
        .execute(tui_command(&mut app, "/room vna"))
        .await
        .unwrap();
    app.reduce(opened);
    for body in ["first room message", "second room message"] {
        let event = bridge.execute(tui_command(&mut app, body)).await.unwrap();
        assert!(matches!(
            event,
            AppEvent::CommandFinished {
                result: CommandResult::ContextWithHistory(_),
                ..
            }
        ));
        app.reduce(event);
        assert!(!app.turn_active(), "Room persistence has no live turn");
        assert_eq!(app.transcript().matches(body).count(), 1);
    }
    assert_eq!(app.transcript().matches("first room message").count(), 1);
    assert_eq!(app.transcript().matches("second room message").count(), 1);
    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn inactive_tui_bridge_mentions_hydrate_before_live_deltas() {
    let workspace = TestWorkspace::new();
    workspace.seed_acp_agent("codex", &["--no-permission"]);
    let mut bridge = InactiveTuiBridge::open(&workspace.database).await.unwrap();
    let mut app = App::new(bridge.initial_context());

    bridge
        .dispatch(tui_command(&mut app, "@codex stripped prompt"))
        .unwrap();
    let submitted = tokio::time::timeout(Duration::from_secs(1), bridge.next_event())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(
        &submitted,
        AppEvent::CommandFinished {
            context,
            result: CommandResult::SubmittedWithContext(_),
        } if context == app.context().id()
    ));
    app.reduce(submitted);
    assert!(app.context().label().contains("dm::codex"));
    assert_eq!(app.transcript().matches("› stripped prompt").count(), 1);
    assert!(!app.transcript().contains("@codex"));

    let next = tokio::time::timeout(Duration::from_secs(1), bridge.next_event())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(
        next,
        AppEvent::Chat(ChatEvent::TextDelta(ref text)) if text == "fixture reply"
    ));
    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn inactive_tui_bridge_mention_installs_history_before_protocol_failure() {
    let workspace = TestWorkspace::new();
    workspace.seed_acp_agent("codex", &["--protocol-error"]);
    let mut bridge = InactiveTuiBridge::open(&workspace.database).await.unwrap();
    let mut app = App::new(bridge.initial_context());

    bridge
        .dispatch(tui_command(&mut app, "@codex persisted once"))
        .unwrap();
    let submitted = tokio::time::timeout(Duration::from_secs(1), bridge.next_event())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(
        &submitted,
        AppEvent::CommandFinished {
            context,
            result: CommandResult::SubmittedWithContext(_),
        } if context == app.context().id()
    ));
    app.reduce(submitted);
    assert!(app.context().label().contains("dm::codex"));
    assert_eq!(app.transcript().matches("› persisted once").count(), 1);

    let failed = tokio::time::timeout(Duration::from_secs(1), bridge.next_event())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(
        failed,
        AppEvent::Chat(ChatEvent::TurnFailed(
            july_workspace::application::ChatFailureKind::Protocol
        ))
    ));
    app.reduce(failed);
    assert_eq!(app.transcript().matches("› persisted once").count(), 1);
    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn inactive_tui_bridge_unchanged_context_submission_does_not_reload_history() {
    let workspace = TestWorkspace::new();
    workspace.seed_acp_agent("codex", &["--protocol-error"]);
    let mut bridge = InactiveTuiBridge::open(&workspace.database).await.unwrap();
    let mut app = App::new(bridge.initial_context());
    let opened = bridge
        .execute(tui_command(&mut app, "/dm codex"))
        .await
        .unwrap();
    app.reduce(opened);
    app.reduce(AppEvent::Chat(ChatEvent::TextDelta(
        "local sentinel".into(),
    )));
    app.reduce(AppEvent::Chat(ChatEvent::TurnCompleted));

    let submitted = bridge
        .execute(tui_command(&mut app, "ordinary prompt"))
        .await
        .unwrap();
    assert!(matches!(
        &submitted,
        AppEvent::CommandFinished {
            result: CommandResult::Submitted,
            ..
        }
    ));
    app.reduce(submitted);
    assert!(app.transcript().contains("local sentinel"));
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
    let transcript = app.transcript();
    assert!(
        transcript
            .lines()
            .any(|line| line.starts_with("dm\t") && line.contains("\tcodex\t")),
        "successful /status output was not visible: {transcript:?}"
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

#[tokio::test]
async fn inactive_tui_bridge_keeps_the_context_after_a_failed_turn() {
    let workspace = TestWorkspace::new();
    workspace.seed_acp_agent("codex", &["--protocol-error"]);
    let mut bridge = InactiveTuiBridge::open(&workspace.database).await.unwrap();
    let mut app = App::new(Context::root());

    let opened = bridge
        .execute(tui_command(&mut app, "/dm codex"))
        .await
        .unwrap();
    app.reduce(opened);

    for prompt in ["first failed turn", "second failed turn"] {
        let submitted = bridge.execute(tui_command(&mut app, prompt)).await.unwrap();
        app.reduce(submitted);
        let failed = tokio::time::timeout(Duration::from_secs(1), bridge.next_event())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(matches!(
            failed,
            AppEvent::Chat(ChatEvent::TurnFailed(
                july_workspace::application::ChatFailureKind::Protocol
            ))
        ));
        app.reduce(failed);
    }

    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn tui_bridge_delivers_the_selected_permission_without_text_input() {
    let workspace = TestWorkspace::new();
    workspace.seed_acp_agent("codex", &[]);
    let mut bridge = InactiveTuiBridge::open(&workspace.database).await.unwrap();
    let mut app = App::new(Context::root());

    let opened = bridge
        .execute(tui_command(&mut app, "/dm codex"))
        .await
        .unwrap();
    app.reduce(opened);
    let submitted = bridge
        .execute(tui_command(&mut app, "needs permission"))
        .await
        .unwrap();
    app.reduce(submitted);
    app.reduce(bridge.next_event().await.unwrap().unwrap());

    let commands = app.reduce(AppEvent::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    )));
    let [permission] = commands.as_slice() else {
        panic!("permission selection did not emit one command");
    };
    bridge.dispatch(permission.clone()).unwrap();

    loop {
        let event = tokio::time::timeout(Duration::from_secs(1), bridge.next_event())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let completed = matches!(event, AppEvent::Chat(ChatEvent::TurnCompleted));
        app.reduce(event);
        if completed {
            break;
        }
    }

    let selected: String = Connection::open(&workspace.database)
        .unwrap()
        .query_row(
            "SELECT selected_option_id FROM permission_decisions",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(selected, "allow-once");
    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn tui_cancel_ack_and_late_permission_remain_escapable_without_a_second_cancel() {
    let workspace = TestWorkspace::new();
    workspace.seed_acp_agent("codex", &["--permission-after-cancel"]);
    let mut bridge = InactiveTuiBridge::open(&workspace.database).await.unwrap();
    let mut app = App::new(Context::root());

    let command = tui_command(&mut app, "/dm codex");
    let event = bridge.execute(command).await.unwrap();
    app.reduce(event);
    let command = tui_command(&mut app, "cancel me");
    let event = bridge.execute(command).await.unwrap();
    app.reduce(event);
    let commands = app.reduce(AppEvent::Key(KeyEvent::new(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
    )));
    let [cancel] = commands.as_slice() else {
        panic!("first Ctrl-C did not emit one cancel");
    };
    bridge.dispatch(cancel.clone()).unwrap();

    for _ in 0..2 {
        let event = tokio::time::timeout(Duration::from_secs(1), bridge.next_event())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        app.reduce(event);
    }
    assert!(app.permission().is_some());
    assert_eq!(
        app.turn_state(),
        july_workspace::tui::app::TurnState::CancelAcknowledged
    );
    assert!(
        app.reduce(AppEvent::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )))
        .is_empty()
    );
    assert!(app.exit_requested());

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
        stderr(&output).contains("/publish is unavailable in root context (available in: work)\n")
    );
    assert!(
        stderr(&output).contains("/publish is unavailable in room context (available in: work)\n")
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
    assert!(status.contains(&format!("work\t{settlement}\tcodex\t")));

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

#[test]
fn repl_single_mention_opens_direct_work_and_sends_the_prompt() {
    let workspace = TestWorkspace::new();
    workspace.seed_acp_agent("codex", &[]);

    let output = workspace.repl("@codex fix callback retry\n1\n/quit\n");

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let conversation: String = Connection::open(&workspace.database)
        .unwrap()
        .query_row(
            "SELECT id FROM conversations WHERE type = 'dm'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let messages = workspace.messages(conversation.parse().unwrap());
    assert_eq!(
        messages
            .iter()
            .map(|(body, _)| body.as_str())
            .collect::<Vec<_>>(),
        ["fix callback retry", "fixture reply"]
    );
}

#[test]
fn repl_bare_mention_enters_direct_work_without_sending_anything() {
    let workspace = TestWorkspace::new();
    workspace.seed_acp_agent("codex", &[]);

    let output = workspace.repl("@codex\n/status\n/quit\n");

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("\tcodex\t"));
    let conversation: String = Connection::open(&workspace.database)
        .unwrap()
        .query_row(
            "SELECT id FROM conversations WHERE type = 'dm'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(workspace.messages(conversation.parse().unwrap()).is_empty());
}

#[test]
fn repl_room_mentions_reject_nonmembers_without_side_effects() {
    let workspace = TestWorkspace::new();
    let room = workspace.seed_room("vna");
    let codex = workspace.seed_acp_agent("codex", &[]);
    workspace.seed_acp_agent("pay", &[]);
    workspace.add_member(&room, &codex);
    let output =
        workspace.repl("/room vna\n@codex @pay implement refund flow\n@missing hello\n/quit\n");
    assert!(output.status.success());
    assert!(!stderr(&output).is_empty());
    let connection = Connection::open(&workspace.database).unwrap();
    for table in ["room_messages", "conversations", "session_bindings"] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0, "unexpected side effect in {table}");
    }
    let members: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM room_members WHERE left_at IS NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(members, 1);
}

#[test]
fn repl_multiple_mentions_outside_a_room_report_where_work_lives() {
    let workspace = TestWorkspace::new();
    workspace.seed_acp_agent("codex", &[]);
    workspace.seed_acp_agent("pay", &[]);

    let output = workspace.repl("@codex @pay implement refund flow\n/quit\n");

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stderr(&output).contains("work with several agents needs a room"));
}

#[test]
fn repl_unknown_mention_is_reported_and_keeps_the_context() {
    let workspace = TestWorkspace::new();
    workspace.seed_acp_agent("codex", &[]);

    let output = workspace.repl("@codex hello\n1\n@nobody hi\n/status\n/quit\n");

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stderr(&output).contains("agent nobody does not exist"));
    assert_eq!(stdout(&output).matches("\tcodex\t").count(), 1);
}

#[test]
fn repl_work_lists_room_work_and_opens_one_without_thread() {
    let workspace = TestWorkspace::new();
    let room = workspace.seed_room("vna");
    let codex = workspace.seed_acp_agent("codex", &[]);
    workspace.add_member(&room, &codex);
    let refunds = workspace.seed_thread(&room, "Refund flow", &[&codex]);

    let output = workspace.repl(&format!(
        "/room vna\n/work\n/work {refunds}\nsupport partial refund too\n1\n/quit\n"
    ));

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let stdout_output = stdout(&output);
    assert!(stdout_output.contains("THREAD ID"));
    assert!(stdout_output.lines().any(|line| {
        line.starts_with(&refunds.to_string())
            && line.contains("open")
            && line.ends_with("Refund flow")
    }));
    assert!(stdout_output.contains(&format!("work\t{refunds}\tcodex\n")));
    assert_eq!(
        workspace
            .messages(refunds)
            .iter()
            .map(|(body, _)| body.as_str())
            .collect::<Vec<_>>(),
        ["support partial refund too", "fixture reply"]
    );
}

#[test]
fn repl_plain_room_input_persists_once_without_conversation_side_effects() {
    let workspace = TestWorkspace::new();
    let room = workspace.seed_room("vna");
    let agent = workspace.seed_acp_agent("codex", &[]);
    workspace.add_member(&room, &agent);

    let output = workspace.repl("/room vna\ninvestigate refund issue\n/quit\n");

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let connection = Connection::open(&workspace.database).unwrap();
    let messages = connection
        .prepare(
            "SELECT room_id, sender_type, sender_id, body
             FROM room_messages ORDER BY created_at, id",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        messages,
        vec![(
            room.id.to_string(),
            "user".into(),
            "local-user".into(),
            "investigate refund issue".into(),
        )]
    );
    for table in [
        "conversations",
        "work_items",
        "session_bindings",
        "message_deliveries",
    ] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0, "plain Room input must not create {table}");
    }
}

#[test]
fn repl_help_teaches_mentions_and_hides_threads() {
    let workspace = TestWorkspace::new();
    workspace.seed_room("vna");

    let output = workspace.repl("/room vna\n/help\n/quit\n");

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let stdout_output = stdout(&output);
    assert!(stdout_output.contains("@cashpoint @pay implement refund flow"));
    assert!(stdout_output.contains("/work [work]"));
    assert!(!stdout_output.contains("/thread"));
}

#[test]
fn repl_repeated_mention_resumes_the_same_direct_work() {
    let workspace = TestWorkspace::new();
    workspace.seed_acp_agent("codex", &[]);

    let output = workspace.repl("@codex one\n1\n/back\n@codex two\n1\n/quit\n");

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let connection = Connection::open(&workspace.database).unwrap();
    let conversations: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM conversations WHERE type = 'dm'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(conversations, 1, "a second mention resumes the first work");
    let conversation: String = connection
        .query_row(
            "SELECT id FROM conversations WHERE type = 'dm'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_turns(&workspace, conversation.parse().unwrap(), &["one", "two"]);
}

#[test]
fn repl_plain_prompt_inside_work_stays_in_that_work() {
    let workspace = TestWorkspace::new();
    let room = workspace.seed_room("vna");
    let codex = workspace.seed_acp_agent("codex", &[]);
    let pay = workspace.seed_acp_agent("pay", &[]);
    workspace.add_member(&room, &codex);
    workspace.add_member(&room, &pay);

    let thread = workspace.seed_thread(&room, "existing work", &[&codex, &pay]);
    let output = workspace.repl(
        &format!("/room vna\n/thread {thread} --agent codex\nimplement refund flow\n1\nsupport partial refund too\n1\n/quit\n"),
    );

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let connection = Connection::open(&workspace.database).unwrap();
    let threads: Vec<String> = connection
        .prepare("SELECT id FROM conversations WHERE type = 'thread'")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(threads.len(), 1, "the follow-up prompt made no new work");
    assert_turns(
        &workspace,
        threads[0].parse().unwrap(),
        &["implement refund flow", "support partial refund too"],
    );
}

#[test]
fn repl_work_control_mutates_only_explicit_work_in_the_active_context() {
    let workspace = TestWorkspace::new();
    let room = workspace.seed_room("vna");
    let codex = workspace.seed_agent("codex");
    workspace.add_member(&room, &codex);
    let active_thread = workspace.seed_thread(&room, "active", &[&codex]);
    let other_thread = workspace.seed_thread(&room, "other", &[&codex]);
    let store = SqliteStore::open(&workspace.database).unwrap();
    let active_work = store.list_work_items(active_thread).unwrap()[0].id;
    let other_work = store.list_work_items(other_thread).unwrap()[0].id;
    drop(store);

    let output = workspace.repl(&format!(
        "/room vna\n/work {active_thread}\n/work assign {active_work} --agent codex\n\
         /work status {active_work} working\n\
         /work result {active_work} --status accepted --summary refund contract verified\n\
         /work assign {other_work} --agent codex\n/quit\n"
    ));

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let output_text = stdout(&output);
    assert!(output_text.contains(&format!("assigned\t{active_work}\t{}\n", codex.id)));
    assert!(output_text.contains(&format!("status\t{active_work}\tworking\n")));
    assert!(output_text.contains(&format!("result\t{active_work}\t")));
    assert!(stderr(&output).contains(&format!("work {other_work} does not exist\n")));

    let store = SqliteStore::open(&workspace.database).unwrap();
    let active = store.list_work_items(active_thread).unwrap().remove(0);
    assert_eq!(active.owner_agent_id, Some(codex.id));
    assert_eq!(active.status, WorkStatus::Ready);
    let results = store.list_work_results(active_thread).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].work_id, active_work);
    assert_eq!(results[0].status, "accepted");
    assert_eq!(results[0].summary, "refund contract verified");
    assert_eq!(
        store.list_work_items(other_thread).unwrap()[0].owner_agent_id,
        None
    );
}

#[test]
fn repl_work_control_rejects_room_scope_and_invalid_mutations() {
    let workspace = TestWorkspace::new();
    let room = workspace.seed_room("vna");
    let codex = workspace.seed_agent("codex");
    workspace.add_member(&room, &codex);
    let thread = workspace.seed_thread(&room, "active", &[&codex]);
    let work = SqliteStore::open(&workspace.database)
        .unwrap()
        .list_work_items(thread)
        .unwrap()[0]
        .id;

    let output = workspace.repl(&format!(
        "/room vna\n/work assign {work} --agent codex\n/work {thread}\n\
         /work status {work} nonsense\n/work assign {work} codex\n\
         /work result {work} --status --bad --summary invalid\n/quit\n"
    ));

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let errors = stderr(&output);
    assert!(errors.contains("/work assign is unavailable in room context"));
    assert_eq!(errors.matches("invalid command\n").count(), 3);
    let work = SqliteStore::open(&workspace.database)
        .unwrap()
        .list_work_items(thread)
        .unwrap()
        .remove(0);
    assert_eq!(work.status, WorkStatus::Open);
    assert_eq!(work.owner_agent_id, None);
}

#[test]
fn repl_decision_inbox_accepts_rejects_and_creates_explicit_work() {
    let workspace = TestWorkspace::new();
    let room = workspace.seed_room("payments");
    let codex = workspace.seed_agent("codex");
    workspace.add_member(&room, &codex);
    let thread = workspace.seed_thread(&room, "callback retry", &[&codex]);
    let accepted = workspace.seed_decision(thread, "Choose retry contract", DecisionOwner::User);
    let rejected = workspace.seed_decision(thread, "Replace stable API", DecisionOwner::User);
    let literal = workspace.seed_decision(thread, "Keep flag text", DecisionOwner::User);
    let work_id = WorkItemId::new();
    let literal_work_id = WorkItemId::new();

    let output = workspace.repl(&format!(
        "/decisions\n\
         /decision accept {} --decision keep current contract --reason integration tests pass\n\
         /decision reject {} --reason stable API already works\n\
         /decision work {} --work-id {work_id} --title verify retry contract --agent codex\n\
         /decision accept {} --decision \"keep --reason semantics\"\n\
         /decision work {} --work-id {literal_work_id} --title \"verify --agent routing\"\n\
         /decisions\n/quit\n",
        accepted.id, rejected.id, accepted.id, literal.id, literal.id
    ));

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stderr(&output).is_empty(), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains(&accepted.id.to_string()));
    assert!(text.contains("Choose retry contract"));
    assert!(text.contains("test:payment_contract"));
    assert!(text.contains(&format!("decided\t{}\n", accepted.id)));
    assert!(text.contains(&format!("cancelled\t{}\n", rejected.id)));
    assert!(text.contains(&format!("work\t{}\t{work_id}\n", accepted.id)));
    assert!(text.contains("No pending decisions."));

    let store = SqliteStore::open(&workspace.database).unwrap();
    let accepted = store.get_decision(accepted.id).unwrap().unwrap();
    assert_eq!(accepted.status, DecisionStatus::Decided);
    assert_eq!(accepted.decision.as_deref(), Some("keep current contract"));
    assert_eq!(accepted.reason.as_deref(), Some("integration tests pass"));
    let rejected = store.get_decision(rejected.id).unwrap().unwrap();
    assert_eq!(rejected.status, DecisionStatus::Cancelled);
    assert_eq!(rejected.reason.as_deref(), Some("stable API already works"));
    let work = store.get_work_item(work_id).unwrap().unwrap();
    assert_eq!(work.title, "verify retry contract");
    assert_eq!(work.owner_agent_id, Some(codex.id));
    assert_eq!(
        store
            .get_decision(literal.id)
            .unwrap()
            .unwrap()
            .decision
            .as_deref(),
        Some("keep --reason semantics")
    );
    assert_eq!(
        store.get_work_item(literal_work_id).unwrap().unwrap().title,
        "verify --agent routing"
    );
}

#[test]
fn repl_decision_actions_reject_non_user_ownership_and_bad_grammar() {
    let workspace = TestWorkspace::new();
    let room = workspace.seed_room("payments");
    let architect = workspace.seed_agent("architect");
    workspace.add_member(&room, &architect);
    let thread = workspace.seed_thread(&room, "contract", &[&architect]);
    let decision = workspace.seed_decision(
        thread,
        "Architect-owned decision",
        DecisionOwner::Agent(architect.id),
    );

    let output = workspace.repl(&format!(
        "/decision accept {} --decision override\n\
         /decision reject {} --reason override\n\
         /decision accept {} --decision\n/quit\n",
        decision.id, decision.id, decision.id
    ));

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let errors = stderr(&output);
    assert_eq!(errors.matches("belongs to owner").count(), 2);
    assert!(errors.contains("invalid command"));
    assert_eq!(
        SqliteStore::open(&workspace.database)
            .unwrap()
            .get_decision(decision.id)
            .unwrap()
            .unwrap()
            .status,
        DecisionStatus::Pending
    );
}

#[test]
fn repl_room_mentions_persist_once_activate_only_targets_and_keep_private_output_hidden() {
    let workspace = TestWorkspace::new();
    let room = workspace.seed_room("vna");
    let codex = workspace.seed_acp_agent("codex", &["--no-permission"]);
    let pay = workspace.seed_acp_agent("pay", &["--no-permission"]);
    let idle = workspace.seed_acp_agent("idle", &["--no-permission"]);
    for agent in [&codex, &pay, &idle] {
        workspace.add_member(&room, agent);
    }
    let output = workspace.repl("/room vna\n@codex @codex @pay refund flow\n/status\n/quit\n");
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(!stdout(&output).contains("fixture reply"));
    let connection = Connection::open(&workspace.database).unwrap();
    let bodies: Vec<String> = connection
        .prepare("SELECT body FROM room_messages")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(bodies, ["@codex @codex @pay refund flow"]);
    let targets: Vec<String> = connection
        .prepare("SELECT agent_id FROM room_message_activations ORDER BY agent_id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let mut expected = vec![codex.id.to_string(), pay.id.to_string()];
    expected.sort();
    assert_eq!(targets, expected);
    let conversations: i64 = connection
        .query_row("SELECT COUNT(*) FROM conversations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(conversations, 0);
}

#[test]
fn repl_mentioning_the_same_agents_inside_a_work_continues_it() {
    let workspace = TestWorkspace::new();
    let room = workspace.seed_room("vna");
    let codex = workspace.seed_acp_agent("codex", &[]);
    let pay = workspace.seed_acp_agent("pay", &[]);
    workspace.add_member(&room, &codex);
    workspace.add_member(&room, &pay);

    // The second mention names the same pair, in the other order, from inside
    // the explicitly opened work.
    let thread = workspace.seed_thread(&room, "existing work", &[&codex, &pay]);
    let output = workspace.repl(&format!(
        "/room vna\n/thread {thread} --agent codex\nrefund flow\n1\n@pay @codex also partial refund\n1\n/quit\n"
    ));

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let connection = Connection::open(&workspace.database).unwrap();
    let threads: Vec<String> = connection
        .prepare("SELECT id FROM conversations WHERE type = 'thread'")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(threads.len(), 1, "the repeated mention resumed the work");
    assert_turns(
        &workspace,
        threads[0].parse().unwrap(),
        &["refund flow", "also partial refund"],
    );
}

#[test]
fn repl_mentioning_a_different_set_inside_a_work_still_creates_one() {
    let workspace = TestWorkspace::new();
    let room = workspace.seed_room("vna");
    let codex = workspace.seed_acp_agent("codex", &[]);
    let pay = workspace.seed_acp_agent("pay", &[]);
    let ops = workspace.seed_acp_agent("ops", &[]);
    workspace.add_member(&room, &codex);
    workspace.add_member(&room, &pay);
    workspace.add_member(&room, &ops);

    let thread = workspace.seed_thread(&room, "existing work", &[&codex, &pay]);
    let output = workspace.repl(&format!(
        "/room vna\n/thread {thread} --agent codex\nrefund flow\n1\n@codex @ops callback retry\n1\n/quit\n"
    ));

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let threads: i64 = Connection::open(&workspace.database)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM conversations WHERE type = 'thread'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(threads, 2, "a different set of agents is a different work");
}

#[test]
fn repl_mentioning_the_open_agent_inside_direct_work_continues_it() {
    let workspace = TestWorkspace::new();
    workspace.seed_acp_agent("codex", &[]);

    let output = workspace.repl("@codex one\n1\n@codex two\n1\n/quit\n");

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let connection = Connection::open(&workspace.database).unwrap();
    let conversation: String = connection
        .query_row(
            "SELECT id FROM conversations WHERE type = 'dm'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_turns(&workspace, conversation.parse().unwrap(), &["one", "two"]);
}

#[tokio::test]
async fn inactive_tui_room_mentions_keep_fifo_history_and_drain_all_permissions() {
    let workspace = TestWorkspace::new();
    let room = workspace.seed_room("vna");
    for name in ["codex", "pay"] {
        let agent = workspace.seed_acp_agent(name, &[]);
        workspace.add_member(&room, &agent);
    }
    let mut bridge = InactiveTuiBridge::open(&workspace.database).await.unwrap();
    let mut app = App::new(bridge.initial_context());
    let opened = bridge
        .execute(tui_command(&mut app, "/room vna"))
        .await
        .unwrap();
    app.reduce(opened);
    let context = app.context().id().to_owned();
    for body in ["@codex @codex @pay refund flow", "@codex follow up"] {
        bridge.dispatch(tui_command(&mut app, body)).unwrap();
        let first = tokio::time::timeout(Duration::from_secs(5), bridge.next_event())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(
            matches!(&first, AppEvent::CommandFinished { context: origin, result: CommandResult::SubmittedWithContext(_) } if origin == &context)
        );
        app.reduce(first);
        assert_eq!(app.context().id(), &context);
        assert!(app.turn_active());
        assert_eq!(app.transcript().matches(body).count(), 1);
        let mut permissions = 0;
        while app.turn_active() {
            let event = tokio::time::timeout(Duration::from_secs(5), bridge.next_event())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(!matches!(&event, AppEvent::Chat(ChatEvent::TextDelta(_))));
            app.reduce(event);
            if app.permission().is_some() {
                permissions += 1;
                let commands = app.reduce(AppEvent::Key(KeyEvent::new(
                    KeyCode::Enter,
                    KeyModifiers::NONE,
                )));
                for command in commands {
                    bridge.dispatch(command).unwrap();
                }
            }
        }
        assert_eq!(permissions, if body.contains("@pay") { 2 } else { 1 });
        assert_eq!(app.context().id(), &context);
        assert!(!app.transcript().contains("fixture reply"));
    }
    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn inactive_tui_room_cancel_and_failure_restore_input_without_private_output() {
    for (arguments, cancel) in [
        (&["--permission-after-cancel"][..], true),
        (&["--protocol-error"][..], false),
    ] {
        let workspace = TestWorkspace::new();
        let room = workspace.seed_room("vna");
        let agent = workspace.seed_acp_agent("codex", arguments);
        workspace.add_member(&room, &agent);
        let mut bridge = InactiveTuiBridge::open(&workspace.database).await.unwrap();
        let mut app = App::new(bridge.initial_context());
        let opened = bridge
            .execute(tui_command(&mut app, "/room vna"))
            .await
            .unwrap();
        app.reduce(opened);
        bridge
            .dispatch(tui_command(&mut app, "@codex investigate"))
            .unwrap();
        let submitted = bridge.next_event().await.unwrap().unwrap();
        app.reduce(submitted);
        if cancel {
            bridge.dispatch(AppCommand::CancelTurn).unwrap();
        }
        while app.turn_active() {
            let event = tokio::time::timeout(Duration::from_secs(5), bridge.next_event())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(!matches!(&event, AppEvent::Chat(ChatEvent::TextDelta(_))));
            for command in app.reduce(event) {
                bridge.dispatch(command).unwrap();
            }
        }
        assert!(app.permission().is_none());
        assert!(app.context().label().contains("room::vna"));
        assert!(
            app.transcript()
                .contains(if cancel { "cancelled" } else { "failed" })
        );
        bridge.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn inactive_tui_room_cancel_with_visible_permission_keeps_bridge_alive() {
    let workspace = TestWorkspace::new();
    let room = workspace.seed_room("vna");
    let agent = workspace.seed_acp_agent("codex", &[]);
    workspace.add_member(&room, &agent);
    let mut bridge = InactiveTuiBridge::open(&workspace.database).await.unwrap();
    let mut app = App::new(bridge.initial_context());
    let opened = bridge
        .execute(tui_command(&mut app, "/room vna"))
        .await
        .unwrap();
    app.reduce(opened);
    bridge
        .dispatch(tui_command(&mut app, "@codex investigate"))
        .unwrap();
    while app.permission().is_none() {
        let event = tokio::time::timeout(Duration::from_secs(5), bridge.next_event())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        app.reduce(event);
    }
    bridge.dispatch(AppCommand::CancelTurn).unwrap();
    while app.turn_active() {
        let event = tokio::time::timeout(Duration::from_secs(5), bridge.next_event())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        for command in app.reduce(event) {
            bridge.dispatch(command).unwrap();
        }
    }
    let event = bridge
        .execute(tui_command(&mut app, "still here"))
        .await
        .unwrap();
    app.reduce(event);
    assert!(app.transcript().contains("still here"));
    assert!(app.permission().is_none());
    bridge.shutdown().await.unwrap();
}

#[tokio::test]
async fn inactive_tui_room_cancel_during_stalled_startup_restores_input() {
    let workspace = TestWorkspace::new();
    let room = workspace.seed_room("vna");
    let agent = workspace.seed_acp_agent("slow", &["--hang-new"]);
    workspace.add_member(&room, &agent);
    let mut bridge = InactiveTuiBridge::open(&workspace.database).await.unwrap();
    let mut app = App::new(bridge.initial_context());
    let opened = bridge
        .execute(tui_command(&mut app, "/room vna"))
        .await
        .unwrap();
    app.reduce(opened);
    bridge
        .dispatch(tui_command(&mut app, "@slow investigate"))
        .unwrap();
    app.reduce(bridge.next_event().await.unwrap().unwrap());
    bridge.dispatch(AppCommand::CancelTurn).unwrap();
    while app.turn_active() {
        let event = tokio::time::timeout(Duration::from_secs(3), bridge.next_event())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        app.reduce(event);
    }
    let event = tokio::time::timeout(
        Duration::from_secs(3),
        bridge.execute(tui_command(&mut app, "still here")),
    )
    .await
    .unwrap()
    .unwrap();
    app.reduce(event);
    assert!(app.transcript().contains("still here"));
    tokio::time::timeout(Duration::from_secs(3), bridge.shutdown())
        .await
        .unwrap()
        .unwrap();
}

#[test]
fn room_agent_mcp_publishes_authenticated_messages_and_rotates_scope_on_resume() {
    let workspace = TestWorkspace::new();
    let room = workspace.seed_room("vna");
    let log = workspace.root.join("mcp.jsonl");
    let log_arg = format!("--room-mcp-log={}", log.display());
    let codex = workspace.seed_acp_agent("codex", &["--room-mcp", "--no-permission", &log_arg]);
    let pay = workspace.seed_acp_agent("pay", &["--no-permission"]);
    let ops = workspace.seed_acp_agent("ops", &["--no-permission"]);
    workspace.seed_agent("outside");
    for agent in [&codex, &pay, &ops] {
        workspace.add_member(&room, agent);
    }
    let output = workspace.repl("/room vna\n@codex first\n@codex second\n/quit\n");
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(!stdout(&output).contains("fixture reply"));
    let logs = std::fs::read_to_string(log).unwrap_or_else(|error| {
        panic!(
            "agent must launch the injected MCP tool: {error}; stdout: {}; stderr: {}",
            stdout(&output),
            stderr(&output)
        )
    });
    let logs: Vec<serde_json::Value> = logs
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(logs.len(), 2);
    for (turn, log) in logs.iter().enumerate() {
        let results = log["results"].as_array().unwrap();
        assert_eq!(results[0]["result"]["protocolVersion"], "2025-03-26");
        let tool = &results[1]["result"]["tools"][0];
        assert_eq!(tool["name"], "send_room_message");
        assert_eq!(tool["inputSchema"]["properties"]["targets"]["minItems"], 0);
        assert!(tool["inputSchema"]["properties"].get("sender_id").is_none());
        assert!(tool["inputSchema"]["properties"].get("room_id").is_none());
        assert_eq!(results[2]["result"]["isError"], false);
        assert_eq!(results[2]["result"], results[3]["result"]);
        for error in &results[4..] {
            assert_eq!(error["result"]["isError"], true, "{error}");
        }
        assert_eq!(results.len(), 10 + turn);
    }
    let store = SqliteStore::open(&workspace.database).unwrap();
    let (messages, _) = store.list_recent_room_messages(room.id, 20).unwrap();
    assert_eq!(messages.len(), 4);
    let published: Vec<_> = messages
        .iter()
        .filter(|message| message.sender_type == MemberType::Agent)
        .collect();
    assert_eq!(published.len(), 2);
    for message in published {
        assert_eq!(message.sender_id, codex.id.to_string());
        assert_eq!(message.room_id, room.id);
        assert_eq!(message.body, "shared agent message");
        assert_eq!(message.mentions, vec![pay.id, ops.id]);
    }
    let connection = Connection::open(&workspace.database).unwrap();
    let counts: (i64,i64,i64) = connection.query_row("SELECT (SELECT COUNT(*) FROM conversations), (SELECT COUNT(*) FROM room_message_activations), (SELECT COUNT(*) FROM session_bindings)", [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap();
    assert_eq!(counts, (0, 6, 3));
}

#[test]
fn room_mcp_stdio_rejects_malformed_requests_and_requires_initialization() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_july"))
        .arg("__room-mcp")
        .env_remove("JULY_ROOM_SOCKET")
        .env_remove("JULY_ROOM_TOKEN")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let input = concat!(
        "not-json\n",
        "{\"jsonrpc\":\"1.0\",\"id\":1,\"method\":\"ping\"}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\"clientInfo\":{\"name\":\"test\",\"version\":\"1\"}}}\n",
        "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"tools/call\",\"params\":{\"name\":\"send_room_message\",\"arguments\":{\"targets\":[\"pay\"],\"body\":\"hello\"}}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":5,\"method\":\"ping\"}\n",
    );
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let replies: Vec<serde_json::Value> = stdout(&output)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(replies.len(), 6);
    assert_eq!(replies[0]["error"]["code"], -32700);
    assert_eq!(replies[1]["error"]["code"], -32600);
    assert_eq!(replies[2]["error"]["code"], -32601);
    assert_eq!(replies[3]["result"]["protocolVersion"], "2024-11-05");
    assert_eq!(replies[4]["result"]["isError"], true);
    assert_eq!(replies[5]["result"], json!({}));
}

#[tokio::test]
async fn room_a2a_shared_reply_is_visible_once_without_waking_other_members() {
    for (tui, structured) in [(false, false), (true, false), (false, true), (true, true)] {
        let workspace = TestWorkspace::new();
        let room = workspace.seed_room("vna");
        let other_room = workspace.seed_room("other");
        let fixture = workspace.root.join("room_shared_reply.py");
        let source = include_str!("fixtures/acp_agent.py");
        let marker = "        if \"--room-mcp\" in sys.argv:";
        let hook = r#"        role = os.environ["ROOM_TEST_ROLE"]
        content = message["params"]["prompt"][0]["text"]
        trigger = content.split("Current message:\nMessage: ", 1)[1].split("\n", 1)[0]
        args = {
            "targets": ["pay"] if role == "cashpoint" else [],
            "body": "check shared refund contract" if role == "cashpoint" else "shared refund answer",
            "reply_to": trigger,
            "request_id": "shared-reply",
        }
        if role == "cashpoint" and os.environ["ROOM_TEST_STRUCTURED"] == "true":
            args["work"] = {"action": "create", "title": "Implement refund contract", "goal": "Return test evidence"}
        config = dict(room_configs[session_id][0])
        config["command"] = os.environ["ROOM_TEST_JULY"]
        results = call_room_mcp(config, [args, args])
        assert results[2]["result"]["isError"] is False, results
        assert results[2]["result"] == results[3]["result"], results
"#;
        std::fs::write(&fixture, source.replace(marker, &format!("{hook}{marker}"))).unwrap();
        let cashpoint = workspace.seed_acp_agent("cashpoint", &["--no-permission"]);
        let pay = workspace.seed_acp_agent("pay", &["--no-permission"]);
        let idle = workspace.seed_acp_agent("idle", &["--no-permission"]);
        let connection = Connection::open(&workspace.database).unwrap();
        for agent in [&cashpoint, &pay, &idle] {
            workspace.add_member(&room, agent);
            let mut config = agent.transport_config.clone();
            config["arguments"][0] = json!(fixture);
            config["environment"] = json!({
                "ROOM_TEST_ROLE": agent.name,
                "ROOM_TEST_STRUCTURED": structured.to_string(),
                "ROOM_TEST_JULY": env!("CARGO_BIN_EXE_july"),
                "ACP_PROMPT_LOG": workspace.root.join(format!("{}.prompts", agent.name)),
            });
            connection
                .execute(
                    "UPDATE agents SET transport_config_json = ?1 WHERE id = ?2",
                    [config.to_string(), agent.id.to_string()],
                )
                .unwrap();
        }
        let transcript = if tui {
            let mut bridge = InactiveTuiBridge::open(&workspace.database).await.unwrap();
            let mut app = App::new(bridge.initial_context());
            let opened = bridge
                .execute(tui_command(&mut app, "/room vna"))
                .await
                .unwrap();
            app.reduce(opened);
            let origin = app.context().id().clone();
            bridge
                .dispatch(tui_command(&mut app, "@cashpoint investigate refund"))
                .unwrap();
            let mut publications = 0;
            tokio::time::timeout(Duration::from_secs(15), async {
                loop {
                    let event = bridge.next_event().await.unwrap().unwrap();
                    assert!(!matches!(&event, AppEvent::Chat(ChatEvent::TextDelta(_))));
                    if matches!(&event, AppEvent::RoomMessage(_)) {
                        publications += 1;
                    }
                    app.reduce(event);
                    if !app.turn_active() {
                        break;
                    }
                }
            })
            .await
            .expect("shared Room reply must finish");
            assert_eq!(publications, 2, "{}", app.transcript());
            assert_eq!(app.context().id(), &origin);
            let transcript = app.transcript().to_owned();
            bridge.shutdown().await.unwrap();
            transcript
        } else {
            let output = workspace.repl("/room vna\n@cashpoint investigate refund\n/quit\n");
            assert!(
                output.status.success() && stderr(&output).is_empty(),
                "structured={structured}: {}",
                stderr(&output)
            );
            stdout(&output)
        };
        assert_eq!(
            transcript.matches("shared refund answer").count(),
            1,
            "{transcript}"
        );
        assert!(
            transcript.contains(&format!("[agent:{}] shared refund answer", pay.id)),
            "{transcript}"
        );
        assert!(!transcript.contains("fixture reply"));
        for name in ["cashpoint", "pay"] {
            let prompts =
                std::fs::read_to_string(workspace.root.join(format!("{name}.prompts"))).unwrap();
            assert_eq!(prompts.lines().count(), 1, "no reactivation of {name}");
        }
        assert!(!workspace.root.join("idle.prompts").exists());
        let store = SqliteStore::open(&workspace.database).unwrap();
        let messages = store.list_recent_room_messages(room.id, 20).unwrap().0;
        assert_eq!(messages.len(), 3);
        let request = messages
            .iter()
            .find(|m| m.sender_id == cashpoint.id.to_string())
            .unwrap();
        let reply = messages
            .iter()
            .find(|m| m.sender_id == pay.id.to_string())
            .unwrap();
        assert_eq!(request.mentions, vec![pay.id]);
        assert_eq!(reply.sender_type, MemberType::Agent);
        assert_eq!(reply.room_id, room.id);
        assert_eq!(reply.reply_to, Some(request.id));
        assert_eq!(reply.body, "shared refund answer");
        assert!(reply.mentions.is_empty());
        assert!(
            store
                .list_recent_room_messages(other_room.id, 20)
                .unwrap()
                .0
                .is_empty()
        );
        let works: i64 = connection
            .query_row("SELECT COUNT(*) FROM work_items", [], |r| r.get(0))
            .unwrap();
        assert_eq!(works, i64::from(structured));
        if structured {
            let (work_id, binding_room, requester, owner): (String, String, String, String) = connection.query_row(
                "SELECT b.work_id,b.room_id,b.requester_agent_id,b.owner_agent_id FROM room_a2a_task_bindings b JOIN room_message_work m ON m.work_id=b.work_id WHERE m.message_id=?1",
                [request.id.to_string()], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
            let work = store
                .get_work_item(work_id.parse().unwrap())
                .unwrap()
                .unwrap();
            assert_eq!(work.scope, WorkScope::Room(room.id));
            assert_eq!(work.owner_agent_id, Some(pay.id));
            assert_eq!(binding_room, room.id.to_string());
            assert_eq!(requester, cashpoint.id.to_string());
            assert_eq!(owner, pay.id.to_string());
            let prompts = std::fs::read_to_string(workspace.root.join("pay.prompts")).unwrap();
            assert!(prompts.contains(&work_id));
            assert!(prompts.contains("Implement refund contract"));
            assert!(prompts.contains("Return test evidence"));
            let mut worker =
                july_workspace::runtime::StorageWorker::open(&workspace.database).unwrap();
            let wire = worker
                .prepare_room_a2a_message(request.id, pay.id)
                .await
                .unwrap();
            let task = worker
                .prepare_room_a2a_task(request.id, pay.id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(task["kind"], "task");
            assert_eq!(task["status"]["state"], "submitted");
            assert_eq!(task["id"], wire["taskId"]);
            assert_eq!(wire["metadata"]["july.work_id"], work_id);
            assert_eq!(
                worker
                    .receive_room_a2a_message(pay.id, &wire)
                    .await
                    .unwrap()
                    .message
                    .id,
                request.id
            );
            for pointer in ["/taskId", "/metadata/july.work_id"] {
                let mut forged = wire.clone();
                *forged.pointer_mut(pointer).unwrap() = json!("forged");
                assert!(
                    worker
                        .receive_room_a2a_message(pay.id, &forged)
                        .await
                        .is_err()
                );
            }
            assert!(
                worker
                    .receive_room_a2a_message(idle.id, &wire)
                    .await
                    .is_err()
            );
            worker.shutdown().await.unwrap();
        }
        let activations: i64 = connection
            .query_row("SELECT COUNT(*) FROM room_message_activations", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(activations, 2);
        for table in ["conversations", "messages"] {
            let count: i64 = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, 0, "{table}");
        }
    }
}

#[tokio::test]
async fn room_a2a_publication_waits_for_busy_recipient_and_survives_sender_completion() {
    let workspace = TestWorkspace::new();
    let room = workspace.seed_room("vna");
    let fixture = workspace.root.join("room_a2a_agent.py");
    // Reuse the real ACP/MCP fixture; gate only this scenario's prompt ordering.
    let source = include_str!("fixtures/acp_agent.py");
    let marker = "        if \"--room-mcp\" in sys.argv:";
    assert!(source.contains(marker));
    let hook = r#"        role = os.environ["ROOM_TEST_ROLE"]
        gate = Path(os.environ["ROOM_TEST_GATE"])
        def wait_for(name):
            deadline = time.monotonic() + 10
            while not (gate / name).exists():
                assert time.monotonic() < deadline, name
                time.sleep(0.005)
        if role == "cashpoint":
            wait_for("pay-started")
            args = {"targets": ["pay"], "body": "explicit shared refund answer", "request_id": "same-publication"}
            config = dict(room_configs[session_id][0])
            # InactiveTuiBridge runs inside this test binary; use the actual CLI MCP entrypoint.
            config["command"] = os.environ["ROOM_TEST_JULY"]
            results = call_room_mcp(config, [args, args])
            assert results[2]["result"]["isError"] is False, results
            assert results[2]["result"] == results[3]["result"], results
            (gate / "published").write_text("yes")
        elif role == "pay" and not (gate / "pay-started").exists():
            (gate / "pay-started").write_text("yes")
            wait_for("release-pay")
"#;
    std::fs::write(&fixture, source.replace(marker, &format!("{hook}{marker}"))).unwrap();
    let cashpoint = workspace.seed_acp_agent("cashpoint", &["--no-permission"]);
    let pay = workspace.seed_acp_agent("pay", &["--no-permission"]);
    let idle = workspace.seed_acp_agent("idle", &["--no-permission"]);
    let connection = Connection::open(&workspace.database).unwrap();
    for agent in [&cashpoint, &pay, &idle] {
        workspace.add_member(&room, agent);
        let mut config = agent.transport_config.clone();
        config["arguments"][0] = json!(fixture);
        config["environment"] = json!({
            "ROOM_TEST_ROLE": agent.name,
            "ROOM_TEST_JULY": env!("CARGO_BIN_EXE_july"),
            "ROOM_TEST_GATE": workspace.root,
            "ACP_PROMPT_LOG": workspace.root.join(format!("{}.prompts", agent.name)),
        });
        connection
            .execute(
                "UPDATE agents SET transport_config_json = ?1 WHERE id = ?2",
                [config.to_string(), agent.id.to_string()],
            )
            .unwrap();
    }
    let mut bridge = InactiveTuiBridge::open(&workspace.database).await.unwrap();
    let mut app = App::new(bridge.initial_context());
    let opened = bridge
        .execute(tui_command(&mut app, "/room vna"))
        .await
        .unwrap();
    app.reduce(opened);
    bridge
        .dispatch(tui_command(&mut app, "@cashpoint @pay investigate refund"))
        .unwrap();
    let mut publications = 0;
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let event = bridge.next_event().await.unwrap().unwrap();
            assert!(!matches!(&event, AppEvent::Chat(ChatEvent::TextDelta(_))));
            if matches!(&event, AppEvent::RoomMessage(_)) {
                publications += 1;
                // A shared publication is displayed while pay still owns its first turn.
                assert!(!workspace.root.join("release-pay").exists());
                std::fs::write(workspace.root.join("release-pay"), "go").unwrap();
            }
            app.reduce(event);
            if !app.turn_active() {
                break;
            }
        }
    })
    .await
    .expect("Room publication and queued recipient must finish");
    assert_eq!(publications, 1, "{}", app.transcript());
    assert_eq!(
        app.transcript()
            .matches("explicit shared refund answer")
            .count(),
        1
    );
    assert!(!app.transcript().contains("fixture reply"));
    bridge.shutdown().await.unwrap();
    let prompts = |name: &str| -> Vec<String> {
        std::fs::read_to_string(workspace.root.join(format!("{name}.prompts")))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    };
    assert_eq!(prompts("cashpoint").len(), 1);
    let received = prompts("pay");
    assert_eq!(received.len(), 2);
    assert!(!received[0].contains("explicit shared refund answer"));
    assert!(received[1].contains("explicit shared refund answer"));
    assert!(!workspace.root.join("idle.prompts").exists());
    let messages = SqliteStore::open(&workspace.database)
        .unwrap()
        .list_recent_room_messages(room.id, 20)
        .unwrap()
        .0;
    assert_eq!(messages.len(), 2);
    let published = messages
        .iter()
        .find(|message| message.sender_type == MemberType::Agent)
        .unwrap();
    assert_eq!(published.sender_id, cashpoint.id.to_string());
    assert_eq!(published.mentions, vec![pay.id]);
    for table in ["conversations", "messages", "work_items"] {
        assert_eq!(
            connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0,
            "{table}"
        );
    }
    let activations: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM room_message_activations WHERE status = 'completed'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(activations, 3);
}

#[tokio::test]
async fn room_a2a_stalled_recipient_does_not_hide_sender_permission_and_can_be_cancelled() {
    let workspace = TestWorkspace::new();
    let room = workspace.seed_room("vna");
    let fixture = workspace.root.join("room_slow_recipient.py");
    let source = include_str!("fixtures/acp_agent.py");
    let hook = r#"        config = dict(room_configs[session_id][0])
        config["command"] = os.environ["ROOM_TEST_JULY"]
        results = call_room_mcp(config, [{"targets": ["slow", "idle"], "body": "shared startup request", "request_id": "startup"}])
        assert results[2]["result"]["isError"] is False, results
        deadline = time.monotonic() + 10
        while not Path(os.environ["ROOM_TEST_STARTED"]).exists():
            assert time.monotonic() < deadline
            time.sleep(0.005)
"#;
    let source = source.replace(
        "        if \"--room-mcp\" in sys.argv:",
        &format!("{hook}        if \"--room-mcp\" in sys.argv:"),
    );
    let source = source.replace(
        "    elif method == \"session/new\":",
        r#"    elif method == "session/new":
        if "--hang-new" in sys.argv:
            gate = Path(os.environ["ROOM_TEST_STARTED"])
            gate.write_text("started")
            deadline = time.monotonic() + 10
            while not gate.with_suffix(".release").exists():
                assert time.monotonic() < deadline
                time.sleep(0.005)
            sys.argv.remove("--hang-new")
"#,
    );
    std::fs::write(&fixture, source).unwrap();
    let sender = workspace.seed_agent("cashpoint");
    let slow = workspace.seed_acp_agent("slow", &["--hang-new"]);
    let idle = workspace.seed_acp_agent("idle", &["--no-permission"]);
    let connection = Connection::open(&workspace.database).unwrap();
    for agent in [&sender, &slow, &idle] {
        workspace.add_member(&room, agent);
        let mut config = agent.transport_config.clone();
        config["arguments"][0] = json!(fixture);
        config["environment"] = json!({
            "ROOM_TEST_JULY": env!("CARGO_BIN_EXE_july"),
            "ROOM_TEST_STARTED": workspace.root.join("slow-started"),
            "ACP_PROMPT_LOG": workspace.root.join(format!("{}.prompts", agent.name)),
        });
        connection
            .execute(
                "UPDATE agents SET transport_config_json = ?1 WHERE id = ?2",
                [config.to_string(), agent.id.to_string()],
            )
            .unwrap();
    }
    let mut bridge = InactiveTuiBridge::open(&workspace.database).await.unwrap();
    let mut app = App::new(bridge.initial_context());
    let event = bridge
        .execute(tui_command(&mut app, "/room vna"))
        .await
        .unwrap();
    app.reduce(event);
    bridge
        .dispatch(tui_command(&mut app, "@cashpoint investigate"))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while app.permission().is_none() {
            let event = bridge.next_event().await.unwrap().unwrap();
            app.reduce(event);
            assert!(app.turn_active(), "{}", app.transcript());
        }
    })
    .await
    .expect("sender permission must arrive during stalled recipient startup");
    assert!(workspace.root.join("slow-started").exists());
    bridge.dispatch(AppCommand::CancelTurn).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while app.turn_active() {
            app.reduce(bridge.next_event().await.unwrap().unwrap());
        }
    })
    .await
    .expect("cancel must discard recipient startup and pending recipients");
    assert!(app.permission().is_none());
    assert!(app.transcript().contains("shared startup request"));
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM room_message_activations WHERE agent_id = ?1",
                [idle.id.to_string()],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    // The remote session/new may finish after cancellation; it must never receive a prompt.
    std::fs::write(workspace.root.join("slow-started.release"), "release").unwrap();
    let event = tokio::time::timeout(
        Duration::from_secs(5),
        bridge.execute(tui_command(&mut app, "/dm slow")),
    )
    .await
    .unwrap()
    .unwrap();
    app.reduce(event);
    assert!(
        app.context().label().contains("dm::slow"),
        "{}",
        app.transcript()
    );
    tokio::time::timeout(Duration::from_secs(5), bridge.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT status FROM room_message_activations WHERE agent_id = ?1",
                [slow.id.to_string()],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "failed"
    );
    assert!(!workspace.root.join("slow.prompts").exists());
    assert!(!workspace.root.join("idle.prompts").exists());
}

#[test]
fn room_a2a_restart_replaces_missing_acp_session_and_preserves_shared_work() {
    let workspace = TestWorkspace::new();
    let room = workspace.seed_room("vna");
    let cashpoint = workspace.seed_acp_agent("cashpoint", &["--no-permission"]);
    let pay = workspace.seed_acp_agent("pay", &["--no-permission"]);
    let fixture = workspace.root.join("room_restart.py");
    let marker = "        if \"--room-mcp\" in sys.argv:";
    let hook = r#"        role = os.environ["ROOM_TEST_ROLE"]
        content = message["params"]["prompt"][0]["text"]
        current = content.split("Current message:\nMessage: ", 1)[1]
        trigger = current.split("\n", 1)[0]
        recovered = "Recovered shared Room context." in content
        args = {
            "targets": ["pay"] if role == "cashpoint" else [],
            "body": "delegated restart contract" if role == "cashpoint" else ("recovered shared answer" if recovered else "initial shared answer"),
            "reply_to": trigger,
            "request_id": "stable-publication",
        }
        if role == "cashpoint":
            args["work"] = {"action": "create", "title": "Restart contract", "goal": "Keep task identity"}
        config = dict(room_configs[session_id][0])
        config["command"] = os.environ["ROOM_TEST_JULY"]
        results = call_room_mcp(config, [args, args])
        assert results[2]["result"]["isError"] is False, results
        assert results[2]["result"] == results[3]["result"], results
"#;
    std::fs::write(
        &fixture,
        include_str!("fixtures/acp_agent.py").replace(marker, &format!("{hook}{marker}")),
    )
    .unwrap();
    let connection = Connection::open(&workspace.database).unwrap();
    for agent in [&cashpoint, &pay] {
        workspace.add_member(&room, agent);
        let mut config = agent.transport_config.clone();
        config["arguments"][0] = json!(fixture);
        config["environment"] = json!({
            "ROOM_TEST_ROLE": agent.name,
            "ROOM_TEST_JULY": env!("CARGO_BIN_EXE_july"),
            "ACP_PROMPT_LOG": workspace.root.join(format!("{}.prompts", agent.name)),
        });
        connection
            .execute(
                "UPDATE agents SET transport_config_json=?1 WHERE id=?2",
                [config.to_string(), agent.id.to_string()],
            )
            .unwrap();
    }
    let first = workspace.repl("/room vna\n@cashpoint start contract\n/quit\n");
    assert!(
        first.status.success() && stderr(&first).is_empty(),
        "{}",
        stderr(&first)
    );
    assert_eq!(stdout(&first).matches("initial shared answer").count(), 1);
    assert!(!stdout(&first).contains("fixture reply"));
    let before = SqliteStore::open(&workspace.database).unwrap();
    let messages = before.list_recent_room_messages(room.id, 20).unwrap().0;
    assert_eq!(messages.len(), 3);
    let delegation = messages
        .iter()
        .find(|m| m.body == "delegated restart contract")
        .unwrap();
    let shared = before
        .get_room_message_work(delegation.id)
        .unwrap()
        .unwrap();
    assert_eq!(shared.work.owner_agent_id, Some(pay.id));
    let first_binding = before
        .get_room_session_binding(room.id, pay.id)
        .unwrap()
        .unwrap();
    drop(before);

    let second = workspace.repl("/room vna\n@pay continue existing contract\n/quit\n");
    assert!(
        second.status.success() && stderr(&second).is_empty(),
        "{}",
        stderr(&second)
    );
    let transcript = stdout(&second);
    assert_eq!(
        transcript.matches("recovered shared answer").count(),
        1,
        "{transcript}"
    );
    assert!(!transcript.contains("fixture reply"));
    let after = SqliteStore::open(&workspace.database).unwrap();
    let current = after.get_room_message_work(delegation.id).unwrap().unwrap();
    assert_eq!(current.work, shared.work);
    assert_eq!(current.binding, shared.binding);
    let replacement = after
        .get_room_session_binding(room.id, pay.id)
        .unwrap()
        .unwrap();
    assert_eq!(replacement.generation, first_binding.generation + 1);
    assert_ne!(replacement.id, first_binding.id);
    let messages = after.list_recent_room_messages(room.id, 20).unwrap().0;
    assert_eq!(messages.len(), 5);
    assert_eq!(
        messages
            .iter()
            .filter(|m| m.body == "delegated restart contract")
            .count(),
        1
    );
    let answer = messages
        .iter()
        .find(|m| m.body == "recovered shared answer")
        .unwrap();
    assert_eq!(answer.sender_id, pay.id.to_string());
    assert_eq!(
        answer.reply_to,
        Some(
            messages
                .iter()
                .find(|m| m.body.contains("continue existing contract"))
                .unwrap()
                .id
        )
    );
    for table in ["work_items", "room_a2a_task_bindings"] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "{table}");
    }
    for table in ["messages", "conversations"] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "{table}");
    }
    let prompts = std::fs::read_to_string(workspace.root.join("pay.prompts")).unwrap();
    let prompts: Vec<String> = prompts
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(prompts.len(), 2);
    assert!(!prompts[0].contains("Recovered shared Room context."));
    assert!(prompts[1].contains("Recovered shared Room context."));
    assert!(prompts[1].contains(&format!("Unfinished Work: {}", shared.work.id)));
    assert!(prompts[1].contains(&format!("A2A Task: {}", shared.binding.task_id)));
    assert!(!prompts[1].contains("fixture reply"));
    assert_eq!(
        std::fs::read_to_string(workspace.root.join("cashpoint.prompts"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[test]
fn room_a2a_complete_demo_keeps_two_agent_question_and_answer_in_shared_room() {
    let workspace = TestWorkspace::new();
    let room = workspace.seed_room("vna");
    let fixture = workspace.root.join("room_complete_demo.py");
    let source = include_str!("fixtures/acp_agent.py");
    let marker = "        if \"--room-mcp\" in sys.argv:";
    assert!(source.contains(marker));
    let hook = r#"        role = os.environ["ROOM_TEST_ROLE"]
        gate = Path(os.environ["ROOM_TEST_GATE"])
        content = message["params"]["prompt"][0]["text"]
        trigger = content.split("Current message:\nMessage: ", 1)[1].split("\n", 1)[0]
        config = dict(room_configs[session_id][0])
        config["command"] = os.environ["ROOM_TEST_JULY"]
        def publish(body, targets, key):
            args = {"body": body, "targets": targets, "reply_to": trigger, "request_id": key}
            results = call_room_mcp(config, [args, args])
            assert results[2]["result"]["isError"] is False, results
            assert results[2]["result"] == results[3]["result"], results
        initial = gate / (role + "-initial")
        if not initial.exists():
            publish(role + " initial public reply", [], "initial")
            initial.write_text("published")
            if role == "cashpoint":
                deadline = time.monotonic() + 10
                while not (gate / "pay-initial").exists():
                    assert time.monotonic() < deadline, "pay initial reply"
                    time.sleep(0.005)
                publish("Is payment_ref the payment identifier?", ["pay"], "question")
        elif role == "pay":
            publish("Use reference_id for the payment identifier.", ["cashpoint"], "answer")
        elif role == "cashpoint":
            publish("Confirmed: cashpoint will use reference_id.", [], "final")
        else:
            raise AssertionError("idle agent must not activate")
"#;
    std::fs::write(&fixture, source.replace(marker, &format!("{hook}{marker}"))).unwrap();
    let cashpoint = workspace.seed_acp_agent("cashpoint", &["--no-permission"]);
    let pay = workspace.seed_acp_agent("pay", &["--no-permission"]);
    let idle = workspace.seed_acp_agent("idle", &["--no-permission"]);
    let connection = Connection::open(&workspace.database).unwrap();
    for agent in [&cashpoint, &pay, &idle] {
        workspace.add_member(&room, agent);
        let mut config = agent.transport_config.clone();
        config["arguments"][0] = json!(fixture);
        config["environment"] = json!({
            "ROOM_TEST_ROLE": agent.name,
            "ROOM_TEST_JULY": env!("CARGO_BIN_EXE_july"),
            "ROOM_TEST_GATE": workspace.root,
            "ACP_PROMPT_LOG": workspace.root.join(format!("{}.prompts", agent.name)),
        });
        connection
            .execute(
                "UPDATE agents SET transport_config_json = ?1 WHERE id = ?2",
                [config.to_string(), agent.id.to_string()],
            )
            .unwrap();
    }
    let output = workspace.repl("/room vna\n@cashpoint @pay review payment contract\n/quit\n");
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stderr(&output).is_empty(), "{}", stderr(&output));
    let transcript = stdout(&output);
    assert!(!transcript.contains("fixture reply"), "{transcript}");
    let store = SqliteStore::open(&workspace.database).unwrap();
    let messages = store.list_recent_room_messages(room.id, 20).unwrap().0;
    assert_eq!(messages.len(), 6, "{transcript}");
    assert_eq!(messages[0].sender_type, MemberType::User);
    assert_eq!(messages[0].mentions, vec![cashpoint.id, pay.id]);
    for message in &messages[1..] {
        assert_eq!(message.room_id, room.id);
        assert_eq!(message.sender_type, MemberType::Agent);
        assert_eq!(transcript.matches(&message.body).count(), 1, "{transcript}");
        assert!(!message.body.contains("fixture reply"));
    }
    for agent in [&cashpoint, &pay] {
        let initial = messages[1..3]
            .iter()
            .find(|m| m.sender_id == agent.id.to_string())
            .unwrap();
        assert_eq!(initial.body, format!("{} initial public reply", agent.name));
        assert!(initial.mentions.is_empty());
        assert_eq!(initial.reply_to, Some(messages[0].id));
        let prompts =
            std::fs::read_to_string(workspace.root.join(format!("{}.prompts", agent.name)))
                .unwrap();
        let prompts: Vec<String> = prompts
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(prompts.len(), 2, "{} prompts", agent.name);
        let received = if agent.id == pay.id {
            &messages[3]
        } else {
            &messages[4]
        };
        let current = prompts[1].split_once("Current message:\n").unwrap().1;
        assert!(current.contains(&format!("Message: {}", received.id)));
        assert!(current.contains(&received.body), "{current}");
        assert!(!prompts[1].contains("fixture reply"));
        let bindings: (i64, i64, i64) = connection.query_row(
            "SELECT COUNT(*), MIN(generation), MAX(generation) FROM session_bindings WHERE agent_id = ?1",
            [agent.id.to_string()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).unwrap();
        assert_eq!(
            bindings,
            (1, 1, 1),
            "{} must reuse its Room session",
            agent.name
        );
    }
    let question = &messages[3];
    assert_eq!(question.body, "Is payment_ref the payment identifier?");
    assert_eq!(question.sender_id, cashpoint.id.to_string());
    assert_eq!(question.mentions, vec![pay.id]);
    let answer = &messages[4];
    assert_eq!(answer.body, "Use reference_id for the payment identifier.");
    assert_eq!(answer.sender_id, pay.id.to_string());
    assert_eq!(answer.mentions, vec![cashpoint.id]);
    assert_eq!(answer.reply_to, Some(question.id));
    let final_reply = &messages[5];
    assert_eq!(
        final_reply.body,
        "Confirmed: cashpoint will use reference_id."
    );
    assert_eq!(final_reply.sender_id, cashpoint.id.to_string());
    assert_eq!(final_reply.reply_to, Some(answer.id));
    assert!(final_reply.mentions.is_empty());
    assert!(!workspace.root.join("idle.prompts").exists());
    let activations: (i64, i64) = connection
        .query_row(
            "SELECT COUNT(*), SUM(status = 'completed') FROM room_message_activations",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(activations, (4, 4));
    for table in [
        "conversations",
        "messages",
        "work_items",
        "room_a2a_task_bindings",
    ] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "Q&A must not create {table}");
    }
}
