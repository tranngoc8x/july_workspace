//! `july init` validates scripting input before selecting or installing adapters.

use std::path::PathBuf;
use std::process::{Command, Output};

struct TestWorkspace {
    root: PathBuf,
    home: PathBuf,
}

impl TestWorkspace {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("july-cli-init-{}", ulid::Ulid::generate()));
        let home = root.join("home");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&home).unwrap();
        Self { root, home }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_july"))
            .args(args)
            .env("JULY_HOME", &self.home)
            .current_dir(&self.root)
            .output()
            .unwrap()
    }
}

impl Drop for TestWorkspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

#[test]
fn init_rejects_an_unknown_adapter_id_before_touching_the_network() {
    let workspace = TestWorkspace::new();
    let output = workspace.run(&["init", "--adapters", "khong-ton-tai"]);

    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("khong-ton-tai"),
        "got: {}",
        stderr(&output)
    );
    assert!(
        !workspace.home.join("adapters/node_modules").exists(),
        "không được cài gì khi id sai"
    );
}

#[test]
fn init_rejects_an_empty_adapter_list() {
    let workspace = TestWorkspace::new();

    assert!(!workspace.run(&["init", "--adapters", ""]).status.success());
}

#[test]
fn init_usage_error_when_the_flag_has_no_value() {
    let workspace = TestWorkspace::new();

    assert!(!workspace.run(&["init", "--adapters"]).status.success());
}
