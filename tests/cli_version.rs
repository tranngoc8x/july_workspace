use serde_json::Value;
use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_july"))
        .args(args)
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

#[test]
fn version_flags_print_the_package_version() {
    for flag in ["--version", "-V"] {
        let output = run(&[flag]);
        assert!(output.status.success());
        assert_eq!(
            stdout(&output).trim(),
            format!("july {}", env!("CARGO_PKG_VERSION"))
        );
    }
}

#[test]
fn version_json_frames_name_and_version() {
    let output = run(&["--version", "--json"]);
    assert!(output.status.success());
    let value: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(value["name"], "july");
    assert_eq!(value["version"], env!("CARGO_PKG_VERSION"));
}

#[test]
fn version_flag_rejects_extra_arguments() {
    let output = run(&["--version", "extra"]);
    assert!(!output.status.success());
    assert!(stdout(&output).is_empty());
}
