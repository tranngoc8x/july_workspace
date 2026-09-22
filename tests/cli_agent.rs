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
    home: PathBuf,
    database: PathBuf,
}

impl TestWorkspace {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("july-cli-agent-{}", ulid::Ulid::generate()));
        std::fs::create_dir(&root).unwrap();
        let home = root.join("home");
        std::fs::create_dir(&home).unwrap();
        let database = root.join("workspace.db");
        Self {
            root,
            home,
            database,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_july"))
            .args(args)
            .env("JULY_WORKSPACE_DB", self.database.file_name().unwrap())
            .env("JULY_HOME", &self.home)
            .current_dir(&self.root)
            .output()
            .unwrap()
    }

    fn verify_adapter(&self, id: &str, name: &str, version: &str) -> PathBuf {
        let executable = self.root.join(format!("{id}-acp"));
        std::fs::write(&executable, "#!/bin/sh\n").unwrap();
        let identities = self.home.join("adapters/identities.json");
        std::fs::create_dir_all(identities.parent().unwrap()).unwrap();
        std::fs::write(
            identities,
            json!({
                id: {
                    "name": name,
                    "version": version,
                    "bin": executable,
                }
            })
            .to_string(),
        )
        .unwrap();
        executable
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
fn agent_add_generates_acp_config_from_a_verified_adapter_without_a_session_or_room_membership() {
    let workspace = TestWorkspace::new();
    workspace.seed_room("Operations");
    let executable = workspace.verify_adapter("codex", "Codex", "1.0.0");

    let added = workspace.run(&[
        "agent",
        "add",
        "cashpoint",
        "--project",
        "/work/cashpoint",
        "--runtime",
        "codex",
        "--adapter",
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

    let transport: String = Connection::open(&workspace.database)
        .unwrap()
        .query_row(
            "SELECT transport_config_json FROM agents WHERE name = 'cashpoint'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let transport: Value = serde_json::from_str(&transport).unwrap();
    assert_eq!(
        transport["executable"],
        executable.to_string_lossy().as_ref()
    );
    assert_eq!(transport["arguments"], json!([]));
    assert_eq!(transport["environment"], json!({}));
    assert_eq!(
        transport["state_directory"],
        workspace
            .home
            .join("state/cashpoint")
            .to_string_lossy()
            .as_ref()
    );
    assert_eq!(transport["expected_agent_name"], "Codex");
    assert_eq!(transport["expected_agent_version"], "1.0.0");
}

#[test]
fn agent_add_rejects_a_duplicate_name_and_a_missing_project() {
    let workspace = TestWorkspace::new();
    let config = workspace.root.join("config.json");
    std::fs::write(&config, "{}").unwrap();
    let added = workspace.run(&[
        "agent",
        "add",
        "cashpoint",
        "--project",
        "/work/cashpoint",
        "--transport",
        "custom",
        "--config",
        config.to_str().unwrap(),
    ]);
    assert!(added.status.success(), "stderr: {}", stderr(&added));

    let duplicate = workspace.run(&[
        "agent",
        "add",
        "cashpoint",
        "--project",
        "/work/other",
        "--transport",
        "custom",
        "--config",
        config.to_str().unwrap(),
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
        json!({
            "executable": "/usr/bin/codex",
            "arguments": ["acp"],
            "environment": {},
            "state_directory": "/work/state/codex",
            "expected_agent_name": "Codex",
            "expected_agent_version": "1.0.0",
        })
        .to_string(),
    )
    .unwrap();
    let cashpoint = workspace.run(&[
        "agent",
        "add",
        "cashpoint",
        "--project",
        "/work/cashpoint",
        "--runtime",
        "codex",
        "--transport",
        "acp",
        "--config",
        config.to_str().unwrap(),
    ]);
    assert!(cashpoint.status.success(), "stderr: {}", stderr(&cashpoint));
    let pay = workspace.run(&[
        "agent",
        "add",
        "pay",
        "--project",
        "/work/pay",
        "--transport",
        "acp",
        "--config",
        config.to_str().unwrap(),
    ]);
    assert!(pay.status.success(), "stderr: {}", stderr(&pay));

    let listed = workspace.run(&["agent", "list", "--json"]);
    assert!(listed.status.success(), "stderr: {}", stderr(&listed));
    let agents: Vec<Value> = serde_json::from_str(stdout(&listed).trim_end()).unwrap();
    assert_eq!(agents.len(), 2);
    assert_eq!(agents[0]["name"], "cashpoint");
    assert_eq!(agents[0]["runtime"], "codex");
    assert_eq!(agents[1]["name"], "pay");
    assert_eq!(agents[1]["runtime"], "");

    let listed = workspace.run(&["agent", "list"]);
    let listed = stdout(&listed);
    assert!(listed.starts_with("AGENT ID                    NAME"));
    assert!(listed.contains("PROJECT"));
    assert!(listed.contains("TRANSPORT"));
    assert!(listed.contains("RUNTIME"));
    assert!(listed.contains("STATUS\n"));
    assert!(!listed.contains('\t'));

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

#[test]
fn agent_add_rejects_an_acp_agent_without_an_adapter_or_config() {
    let workspace = TestWorkspace::new();
    let output = workspace.run(&[
        "agent",
        "add",
        "cashpoint",
        "--project",
        "/work/cashpoint",
        "--json",
    ]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("\"code\":\"missing_adapter\""));
    assert!(stderr(&output).contains(
        "agent dùng transport acp cần --adapter <id> hoặc --config <file>; chạy july setup để xem adapter đã cài"
    ));
    assert_eq!(workspace.count("SELECT COUNT(*) FROM agents"), 0);
}

#[test]
fn agent_add_rejects_adapter_and_config_together() {
    let workspace = TestWorkspace::new();
    let output = workspace.run(&[
        "agent",
        "add",
        "cashpoint",
        "--project",
        "/work/cashpoint",
        "--adapter",
        "codex",
        "--config",
        "config.json",
    ]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("usage"));
}

#[test]
fn agent_add_option_errors_show_catalog_and_custom_transport_usage() {
    let workspace = TestWorkspace::new();
    let output = workspace.run(&[
        "agent",
        "add",
        "cashpoint",
        "--project",
        "/work/cashpoint",
        "--adapter",
        "codex",
        "--config",
        "config.json",
    ]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains(
        "usage: july agent add <name> --project <path> --adapter <id> [--runtime <runtime>]"
    ));
    assert!(stderr(&output).contains(
        "usage: july agent add <name> --project <path> --transport <type> --config <file> [--runtime <runtime>]"
    ));
}

#[test]
fn agent_add_rejects_adapter_and_explicit_transport_together() {
    let workspace = TestWorkspace::new();
    let output = workspace.run(&[
        "agent",
        "add",
        "cashpoint",
        "--project",
        "/work/cashpoint",
        "--adapter",
        "codex",
        "--transport",
        "custom",
    ]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("usage"));
}

#[test]
fn agent_add_rejects_an_adapter_that_was_never_verified() {
    let workspace = TestWorkspace::new();
    let output = workspace.run(&[
        "agent",
        "add",
        "cashpoint",
        "--project",
        "/work/cashpoint",
        "--adapter",
        "codex",
    ]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("chạy lại `july setup`"));
}

#[test]
fn agent_update_rejects_an_unknown_agent_with_a_meaningful_json_error() {
    let workspace = TestWorkspace::new();
    let output = workspace.run(&[
        "agent",
        "update",
        "khong-ton-tai",
        "--adapter",
        "codex",
        "--json",
    ]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("\"code\":\"agent_not_found\""));
    assert!(stderr(&output).contains("khong-ton-tai"));
}

#[test]
fn agent_update_requires_an_adapter_or_a_config_with_agent_usage() {
    let workspace = TestWorkspace::new();
    let output = workspace.run(&["agent", "update", "cashpoint", "--json"]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("\"code\":\"usage\""));
    assert!(stderr(&output).contains("usage: july agent update <agent> --adapter <id>"));
    assert!(stderr(&output).contains("usage: july agent update <agent> --config <file>"));
    assert!(stderr(&output).contains("usage: july agent update <agent> --description <text>"));
}

#[test]
fn agent_update_rewrites_the_routing_description_without_touching_the_transport() {
    let workspace = TestWorkspace::new();
    let executable = workspace.verify_adapter("codex", "Codex", "1.0.0");
    let added = workspace.run(&[
        "agent",
        "add",
        "cashpoint",
        "--project",
        "/work/cashpoint",
        "--runtime",
        "codex",
        "--adapter",
        "codex",
        "--description",
        "handles payments",
    ]);
    assert!(added.status.success(), "stderr: {}", stderr(&added));

    let metadata = |workspace: &TestWorkspace| -> Value {
        let stored: String = Connection::open(&workspace.database)
            .unwrap()
            .query_row(
                "SELECT metadata_json FROM agents WHERE name = 'cashpoint'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        serde_json::from_str(&stored).unwrap()
    };
    assert_eq!(metadata(&workspace)["description"], "handles payments");

    let updated = workspace.run(&[
        "agent",
        "update",
        "cashpoint",
        "--description",
        "  owns refunds and settlement  ",
    ]);
    assert!(updated.status.success(), "stderr: {}", stderr(&updated));
    let stored = metadata(&workspace);
    assert_eq!(stored["description"], "owns refunds and settlement");
    assert_eq!(stored["runtime"], "codex", "the rest of metadata survives");

    // The transport is untouched by a description-only update.
    let transport: String = Connection::open(&workspace.database)
        .unwrap()
        .query_row(
            "SELECT transport_config_json FROM agents WHERE name = 'cashpoint'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let transport: Value = serde_json::from_str(&transport).unwrap();
    assert_eq!(
        transport["executable"],
        executable.to_string_lossy().as_ref()
    );

    let cleared = workspace.run(&["agent", "update", "cashpoint", "--description", ""]);
    assert!(cleared.status.success(), "stderr: {}", stderr(&cleared));
    let stored = metadata(&workspace);
    assert!(
        stored["description"].is_null(),
        "a blank description clears the field: {stored}"
    );
    assert_eq!(stored["runtime"], "codex");
}

#[test]
fn agent_update_replaces_the_persisted_config_through_the_cli() {
    let workspace = TestWorkspace::new();
    let old_config = workspace.root.join("old.json");
    let new_config = workspace.root.join("new.json");
    let base_config = |executable: &str| {
        json!({
            "executable": executable,
            "arguments": [],
            "environment": {},
            "state_directory": "/work/state/cashpoint",
            "expected_agent_name": "Codex",
            "expected_agent_version": "1.0.0",
        })
        .to_string()
    };
    std::fs::write(&old_config, base_config("/old/bin")).unwrap();
    std::fs::write(&new_config, base_config("/new/bin")).unwrap();

    let added = workspace.run(&[
        "agent",
        "add",
        "cashpoint",
        "--project",
        "/work/cashpoint",
        "--runtime",
        "codex",
        "--transport",
        "acp",
        "--config",
        old_config.to_str().unwrap(),
    ]);
    assert!(added.status.success(), "stderr: {}", stderr(&added));
    let id: String = Connection::open(&workspace.database)
        .unwrap()
        .query_row(
            "SELECT id FROM agents WHERE name = 'cashpoint'",
            [],
            |row| row.get(0),
        )
        .unwrap();

    let updated = workspace.run(&[
        "agent",
        "update",
        "cashpoint",
        "--config",
        new_config.to_str().unwrap(),
        "--json",
    ]);
    assert!(updated.status.success(), "stderr: {}", stderr(&updated));

    let (updated_id, config): (String, String) = Connection::open(&workspace.database)
        .unwrap()
        .query_row(
            "SELECT id, transport_config_json FROM agents WHERE name = 'cashpoint'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(updated_id, id);
    assert_eq!(
        serde_json::from_str::<Value>(&config).unwrap()["executable"],
        "/new/bin"
    );
}

#[test]
fn agent_add_rejects_an_unparseable_acp_config_and_persists_nothing() {
    let workspace = TestWorkspace::new();
    let config = workspace.root.join("bad.json");
    std::fs::write(&config, json!({ "executable": "" }).to_string()).unwrap();

    let output = workspace.run(&[
        "agent",
        "add",
        "victim",
        "--project",
        "/tmp",
        "--transport",
        "acp",
        "--config",
        config.to_str().unwrap(),
    ]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("field `executable` must be a non-empty string"));
    assert_eq!(workspace.count("SELECT COUNT(*) FROM agents"), 0);
}

#[test]
fn agent_update_rejects_an_unparseable_acp_config_and_leaves_the_stored_config_unchanged() {
    let workspace = TestWorkspace::new();
    let good_config = workspace.root.join("good.json");
    let bad_config = workspace.root.join("bad.json");
    std::fs::write(
        &good_config,
        json!({
            "executable": "/old/bin",
            "arguments": [],
            "environment": {},
            "state_directory": "/work/state/cashpoint",
            "expected_agent_name": "Codex",
            "expected_agent_version": "1.0.0",
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(&bad_config, json!({ "executable": "" }).to_string()).unwrap();

    let added = workspace.run(&[
        "agent",
        "add",
        "cashpoint",
        "--project",
        "/work/cashpoint",
        "--transport",
        "acp",
        "--config",
        good_config.to_str().unwrap(),
    ]);
    assert!(added.status.success(), "stderr: {}", stderr(&added));

    let updated = workspace.run(&[
        "agent",
        "update",
        "cashpoint",
        "--config",
        bad_config.to_str().unwrap(),
    ]);
    assert!(!updated.status.success());
    assert!(stderr(&updated).contains("field `executable` must be a non-empty string"));

    let config: String = Connection::open(&workspace.database)
        .unwrap()
        .query_row(
            "SELECT transport_config_json FROM agents WHERE name = 'cashpoint'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&config).unwrap()["executable"],
        "/old/bin"
    );
}

#[test]
fn agent_add_rejects_a_verified_adapter_whose_binary_was_removed() {
    let workspace = TestWorkspace::new();
    let executable = workspace.verify_adapter("codex", "Codex", "1.0.0");
    std::fs::remove_file(&executable).unwrap();

    let output = workspace.run(&[
        "agent",
        "add",
        "cashpoint",
        "--project",
        "/work/cashpoint",
        "--adapter",
        "codex",
        "--json",
    ]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("chưa được cài"));
    assert_eq!(workspace.count("SELECT COUNT(*) FROM agents"), 0);
}

#[test]
fn agent_add_rejects_an_agent_name_that_escapes_the_state_directory() {
    let workspace = TestWorkspace::new();
    workspace.verify_adapter("codex", "Codex", "1.0.0");

    let output = workspace.run(&[
        "agent",
        "add",
        "../../tmp/escape",
        "--project",
        "/work/cashpoint",
        "--adapter",
        "codex",
    ]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("không hợp lệ"));
    assert_eq!(workspace.count("SELECT COUNT(*) FROM agents"), 0);
    assert!(!workspace.home.join("tmp/escape").exists());
}

#[test]
fn agent_update_through_config_keeps_a_custom_transport_type() {
    let workspace = TestWorkspace::new();
    let config = workspace.root.join("custom.json");
    std::fs::write(&config, json!({ "host": "localhost" }).to_string()).unwrap();

    let added = workspace.run(&[
        "agent",
        "add",
        "cashpoint",
        "--project",
        "/work/cashpoint",
        "--transport",
        "custom",
        "--config",
        config.to_str().unwrap(),
    ]);
    assert!(added.status.success(), "stderr: {}", stderr(&added));

    let new_config = workspace.root.join("custom-2.json");
    std::fs::write(&new_config, json!({ "host": "example.com" }).to_string()).unwrap();
    let updated = workspace.run(&[
        "agent",
        "update",
        "cashpoint",
        "--config",
        new_config.to_str().unwrap(),
    ]);
    assert!(updated.status.success(), "stderr: {}", stderr(&updated));

    let (transport_type, config): (String, String) = Connection::open(&workspace.database)
        .unwrap()
        .query_row(
            "SELECT transport_type, transport_config_json FROM agents WHERE name = 'cashpoint'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(transport_type, "custom");
    assert_eq!(
        serde_json::from_str::<Value>(&config).unwrap()["host"],
        "example.com"
    );
}
