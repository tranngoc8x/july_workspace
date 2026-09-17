use july_workspace::domain::{
    Agent, AgentId, Conversation, ConversationId, ConversationKind, DeliveryStatus, MemberType,
    Message, MessageId, Room, RoomId, WorkItemId,
};
use july_workspace::storage::SqliteStore;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::{Command, Output};

const NOW: &str = "2026-09-04T00:00:00Z";
const FAILED_AT: &str = "2026-09-04T00:01:00Z";

struct TestWorkspace {
    root: PathBuf,
    database: PathBuf,
}

impl TestWorkspace {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("july-cli-delivery-{}", ulid::Ulid::generate()));
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

    fn seed_failed_dm(&self, body: &str) -> (MessageId, Agent) {
        let source = agent("source", &self.root);
        let target = agent("target", &self.root);
        let message_id = MessageId::new();
        let mut store = SqliteStore::open(&self.database).unwrap();
        store.insert_agent(&source).unwrap();
        store.insert_agent(&target).unwrap();
        store
            .persist_agent_direct_message(message_id, source.id, target.id, body, NOW)
            .unwrap();
        assert!(
            store
                .mark_delivery_failed(message_id, target.id, FAILED_AT)
                .unwrap()
        );
        (message_id, target)
    }

    fn seed_pending_dm(&self, body: &str) -> (MessageId, Agent) {
        let source = agent("pending-source", &self.root);
        let target = agent("pending-target", &self.root);
        let message_id = MessageId::new();
        let mut store = SqliteStore::open(&self.database).unwrap();
        store.insert_agent(&source).unwrap();
        store.insert_agent(&target).unwrap();
        store
            .persist_agent_direct_message(message_id, source.id, target.id, body, NOW)
            .unwrap();
        (message_id, target)
    }

    fn seed_failed_thread(&self, body: &str, arguments: &[&str]) -> (MessageId, Agent, PathBuf) {
        let source = agent("thread-source", &self.root);
        let mut target = agent("thread-target", &self.root);
        let prompt_log = self.root.join("thread-prompts.jsonl");
        target.transport_config["arguments"]
            .as_array_mut()
            .unwrap()
            .extend(arguments.iter().map(|argument| json!(argument)));
        target.transport_config["environment"] = json!({
            "ACP_PROMPT_LOG": prompt_log.to_string_lossy(),
        });
        let room = Room {
            id: RoomId::new(),
            name: "delivery retry".into(),
            description: None,
            status: "active".into(),
            created_at: NOW.into(),
            updated_at: NOW.into(),
        };
        let thread = Conversation {
            id: ConversationId::new(),
            kind: ConversationKind::Thread,
            room_id: Some(room.id),
            title: Some("retry".into()),
            goal: None,
            parent_conversation_id: None,
            origin_conversation_id: None,
            status: "open".into(),
            created_at: NOW.into(),
            updated_at: NOW.into(),
        };
        let message = Message {
            id: MessageId::new(),
            conversation_id: thread.id,
            sender_type: MemberType::Agent,
            sender_id: source.id.to_string(),
            body: body.into(),
            reply_to: None,
            metadata: json!({"mention": target.id.to_string()}),
            created_at: NOW.into(),
        };
        let mut store = SqliteStore::open(&self.database).unwrap();
        store.insert_agent(&source).unwrap();
        store.insert_agent(&target).unwrap();
        store.insert_room(&room).unwrap();
        store
            .add_room_member(room.id, source.id, None, NOW)
            .unwrap();
        store
            .add_room_member(room.id, target.id, None, NOW)
            .unwrap();
        store
            .create_thread_with_primary_work(
                &thread,
                WorkItemId::new(),
                "july",
                &[source.id, target.id],
            )
            .unwrap();
        store
            .insert_message_with_pending_delivery(&message, target.id, Some("retry capsule"))
            .unwrap();
        assert!(
            store
                .mark_delivery_failed(message.id, target.id, FAILED_AT)
                .unwrap()
        );
        (message.id, target, prompt_log)
    }
}

impl Drop for TestWorkspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn agent(name: &str, root: &std::path::Path) -> Agent {
    Agent {
        id: AgentId::new(),
        name: name.into(),
        project_root: root.to_string_lossy().into_owned(),
        transport_type: "acp".into(),
        transport_config: json!({
            "executable": "/usr/bin/python3",
            "arguments": [
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/acp_agent.py")
                    .to_string_lossy()
                    .into_owned()
            ],
            "environment": {},
            "state_directory": root,
            "expected_agent_name": "test-acp-agent",
            "expected_agent_version": "1.0.0",
        }),
        status: "active".into(),
        metadata: json!({}),
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

#[test]
fn delivery_list_reports_only_failed_rows_without_starting_transport() {
    let workspace = TestWorkspace::new();
    let body = "line one\nline two\tquoted: \"yes\"";
    let (message_id, target) = workspace.seed_failed_dm(body);
    let (pending_id, pending_target) = workspace.seed_pending_dm("still pending");

    let human = workspace.run(&["delivery", "list"]);
    assert!(human.status.success(), "stderr: {}", stderr(&human));
    let human = stdout(&human);
    assert!(human.contains("MESSAGE ID"));
    assert!(human.contains("TARGET AGENT ID"));
    assert!(human.contains(&message_id.to_string()));
    assert!(human.contains(&target.id.to_string()));
    assert!(human.contains(r#"line one\nline two\tquoted: \"yes\""#));
    assert!(!human.contains(&pending_id.to_string()));
    assert_eq!(human.lines().count(), 3);
    assert_eq!(
        SqliteStore::open(&workspace.database)
            .unwrap()
            .get_message_delivery(pending_id, pending_target.id)
            .unwrap()
            .unwrap()
            .status,
        DeliveryStatus::Pending
    );

    let json_output = workspace.run(&["delivery", "list", "--json"]);
    assert!(
        json_output.status.success(),
        "stderr: {}",
        stderr(&json_output)
    );
    let rows: Vec<Value> = serde_json::from_str(&stdout(&json_output)).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0],
        json!({
            "message_id": message_id.to_string(),
            "conversation_id": rows[0]["conversation_id"],
            "conversation_kind": "dm",
            "sender_id": rows[0]["sender_id"],
            "target_agent_id": target.id.to_string(),
            "status": DeliveryStatus::Failed.to_string(),
            "capsule_delivered_at": null,
            "updated_at": FAILED_AT,
            "body": body,
        })
    );
}

#[test]
fn delivery_list_has_explicit_empty_outputs() {
    let workspace = TestWorkspace::new();

    let human = workspace.run(&["delivery", "list"]);
    assert!(human.status.success(), "stderr: {}", stderr(&human));
    assert_eq!(stdout(&human), "No failed deliveries.\n");

    let json_output = workspace.run(&["--json", "delivery", "list"]);
    assert!(
        json_output.status.success(),
        "stderr: {}",
        stderr(&json_output)
    );
    assert_eq!(stdout(&json_output), "[]\n");
}

#[test]
fn delivery_retry_delivers_once_and_then_reports_not_retryable() {
    let workspace = TestWorkspace::new();
    let (message_id, target) = workspace.seed_failed_dm("retry this exact body");

    let retry = workspace.run(&[
        "delivery",
        "retry",
        &message_id.to_string(),
        "--agent",
        &target.name,
        "--json",
    ]);
    assert!(retry.status.success(), "stderr: {}", stderr(&retry));
    assert_eq!(
        serde_json::from_str::<Value>(&stdout(&retry)).unwrap(),
        json!({
            "message_id": message_id.to_string(),
            "target_agent_id": target.id.to_string(),
            "status": "delivered",
        })
    );
    assert_eq!(
        SqliteStore::open(&workspace.database)
            .unwrap()
            .get_message_delivery(message_id, target.id)
            .unwrap()
            .unwrap()
            .status,
        DeliveryStatus::Delivered
    );

    let duplicate = workspace.run(&[
        "delivery",
        "retry",
        &message_id.to_string(),
        "--agent",
        &target.id.to_string(),
        "--json",
    ]);
    assert!(!duplicate.status.success());
    assert!(stdout(&duplicate).is_empty());
    let error: Value = serde_json::from_str(&stderr(&duplicate)).unwrap();
    assert_eq!(error["error"]["code"], "delivery_not_retryable");
}

#[test]
fn delivery_retry_does_not_promote_a_pending_delivery_into_retry_work() {
    let workspace = TestWorkspace::new();
    let (message_id, target) = workspace.seed_pending_dm("not failed");

    let retry = workspace.run(&[
        "delivery",
        "retry",
        &message_id.to_string(),
        "--agent",
        &target.name,
        "--json",
    ]);

    assert!(!retry.status.success());
    let error: Value = serde_json::from_str(&stderr(&retry)).unwrap();
    assert_eq!(error["error"]["code"], "delivery_not_retryable");
    assert_eq!(
        SqliteStore::open(&workspace.database)
            .unwrap()
            .get_message_delivery(message_id, target.id)
            .unwrap()
            .unwrap()
            .status,
        DeliveryStatus::Pending
    );
}

#[test]
fn delivery_retry_routes_thread_mentions_through_the_thread_engine() {
    let workspace = TestWorkspace::new();
    let (message_id, target, prompt_log) =
        workspace.seed_failed_thread("retry thread body", &["--no-permission", "--slow-prompt"]);

    let retry = workspace.run(&[
        "delivery",
        "retry",
        &message_id.to_string(),
        "--agent",
        &target.name,
    ]);

    assert!(retry.status.success(), "stderr: {}", stderr(&retry));
    assert_eq!(
        stdout(&retry),
        format!("delivered\t{message_id}\t{}\n", target.id)
    );
    assert_eq!(
        SqliteStore::open(&workspace.database)
            .unwrap()
            .get_message_delivery(message_id, target.id)
            .unwrap()
            .unwrap()
            .status,
        DeliveryStatus::Delivered
    );
    let prompts = std::fs::read_to_string(prompt_log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<String>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(prompts, ["retry capsule", "retry thread body"]);
}

#[test]
fn delivery_retry_stops_before_the_body_when_the_capsule_requests_permission() {
    let workspace = TestWorkspace::new();
    let (message_id, target, prompt_log) = workspace.seed_failed_thread("must not be sent", &[]);

    let retry = workspace.run(&[
        "delivery",
        "retry",
        &message_id.to_string(),
        "--agent",
        &target.name,
    ]);

    assert!(!retry.status.success());
    assert!(
        stderr(&retry).contains("Thread mention capsule requested permission"),
        "stderr: {}",
        stderr(&retry)
    );
    let delivery = SqliteStore::open(&workspace.database)
        .unwrap()
        .get_message_delivery(message_id, target.id)
        .unwrap()
        .unwrap();
    assert_eq!(delivery.status, DeliveryStatus::Failed);
    assert_eq!(delivery.capsule_delivered_at, None);
    let prompts = std::fs::read_to_string(prompt_log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<String>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(prompts, ["retry capsule"]);
}
