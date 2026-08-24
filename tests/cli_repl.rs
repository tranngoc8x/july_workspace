use rusqlite::Connection;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

struct TestWorkspace {
    root: PathBuf,
    database: PathBuf,
}

impl TestWorkspace {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("july-cli-repl-{}", ulid::Ulid::generate()));
        std::fs::create_dir(&root).unwrap();
        let database = root.join("workspace.db");
        Self { root, database }
    }

    fn repl(&self, input: &str) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_july"))
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
