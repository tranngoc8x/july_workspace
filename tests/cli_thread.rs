use rusqlite::Connection;
use serde_json::Value;
use std::path::PathBuf;
use std::process::{Command, Output};

struct TestWorkspace {
    root: PathBuf,
    database: PathBuf,
}

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
