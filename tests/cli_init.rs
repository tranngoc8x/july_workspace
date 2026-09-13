//! `july setup` validates scripting input before selecting or installing adapters.

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
fn setup_rejects_an_unknown_adapter_id_before_touching_the_network() {
    let workspace = TestWorkspace::new();
    let output = workspace.run(&["setup", "--adapters", "khong-ton-tai"]);

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
fn setup_rejects_an_empty_adapter_list() {
    let workspace = TestWorkspace::new();

    let output = workspace.run(&["setup", "--adapters", ""]);
    assert!(!output.status.success());
}

#[test]
fn setup_usage_error_when_the_flag_has_no_value() {
    let workspace = TestWorkspace::new();

    let output = workspace.run(&["setup", "--adapters"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("usage: july setup"));
}

#[test]
fn init_no_longer_accepts_adapter_setup_flags() {
    let workspace = TestWorkspace::new();
    let output = workspace.run(&["init", "--adapters", "codex"]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("usage: july init"));
    assert!(!workspace.home.join("adapters/node_modules").exists());
}

#[test]
fn init_requires_an_interactive_terminal() {
    let workspace = TestWorkspace::new();
    let output = workspace.run(&["init"]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("july init cần terminal tương tác"));
}

#[cfg(unix)]
fn versioned_adapter(workspace: &TestWorkspace, version_output: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = workspace.root.join("codex-acp");
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/acp_agent.py");
    // JSON strings are also valid Python string literals for these paths.
    std::fs::write(
        &bin,
        format!(
            "#!/usr/bin/python3\nimport sys, runpy\nif sys.argv[1:] == ['--version']:\n    print({})\nelse:\n    runpy.run_path({}, run_name='__main__')\n",
            serde_json::to_string(version_output).unwrap(),
            serde_json::to_string(&fixture).unwrap(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

#[cfg(unix)]
#[test]
fn setup_keeps_compatible_path_binary_and_records_its_acp_identity() {
    let workspace = TestWorkspace::new();
    let bin = versioned_adapter(&workspace, "1.12.0");
    let before = std::fs::read(&bin).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_july"))
        .args(["setup", "--adapters", "codex"])
        .env("JULY_HOME", &workspace.home)
        .env("JULY_WORKSPACE_DB", workspace.root.join("unused.db"))
        .env("PATH", &workspace.root)
        .current_dir(&workspace.root)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let store = july_workspace::adapter::AdapterStore::new(workspace.home.clone());
    let config = store.config_for("codex", "example").unwrap();
    assert_eq!(config["executable"], bin.to_str().unwrap());
    assert_eq!(config["expected_agent_name"], "test-acp-agent");
    assert_eq!(config["expected_agent_version"], "1.0.0");
    assert_eq!(std::fs::read(&bin).unwrap(), before);
    assert!(!workspace.root.join("unused.db").exists());
}

#[cfg(unix)]
#[test]
fn setup_reports_unknown_path_version_without_replacing_verified_identity() {
    let workspace = TestWorkspace::new();
    let bin = versioned_adapter(&workspace, "unknown build");
    let store = july_workspace::adapter::AdapterStore::new(workspace.home.clone());
    store
        .record_identity(
            "codex",
            july_workspace::adapter::AdapterIdentity {
                name: "previous identity".into(),
                version: "previous version".into(),
                bin: bin.clone(),
            },
        )
        .unwrap();
    let identities = workspace.home.join("adapters/identities.json");
    let before = std::fs::read(&identities).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_july"))
        .args(["setup", "--adapters", "codex"])
        .env("JULY_HOME", &workspace.home)
        .env("PATH", &workspace.root)
        .current_dir(&workspace.root)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("UnknownVersion"));
    assert_eq!(std::fs::read(identities).unwrap(), before);
}
