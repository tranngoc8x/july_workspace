//! A packaged binary must create and migrate its workspace database on first
//! use, with no separate setup step.
use rusqlite::Connection;
use std::path::PathBuf;
use std::process::Command;

struct TempHome(PathBuf);

impl TempHome {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("july-bootstrap-{}", ulid::Ulid::generate()));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }

    fn database(&self) -> PathBuf {
        self.0.join(".july/workspace.db")
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(home: &TempHome, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_july"))
        .args(args)
        .env("HOME", &home.0)
        .env_remove("JULY_WORKSPACE_DB")
        .current_dir(&home.0)
        .output()
        .unwrap()
}

fn schema_version(connection: &Connection) -> i64 {
    connection
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn first_command_creates_and_migrates_the_default_workspace_database() {
    let home = TempHome::new();
    assert!(!home.database().exists());

    let output = run(&home, &["room", "list"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(home.database().exists());

    let connection = Connection::open(home.database()).unwrap();
    let version = schema_version(&connection);
    assert!(version > 0, "migrations did not run");

    // A second run reuses the migrated database instead of re-creating it.
    let again = run(&home, &["room", "list"]);
    assert!(again.status.success());
    assert_eq!(version, schema_version(&connection));
}
