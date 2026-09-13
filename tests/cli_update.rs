//! `july update` chỉ nhận đúng một dạng lời gọi và không bao giờ báo thành công
//! khi bước cài đặt chưa tồn tại.

use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_july"))
        .args(args)
        .output()
        .unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

#[test]
fn update_rejects_extra_arguments_and_json_framing() {
    for args in [
        vec!["update", "extra"],
        vec!["update", "--json"],
        vec!["update", "--channel", "beta"],
    ] {
        let output = run(&args);
        assert!(!output.status.success(), "{args:?}");
        assert!(stderr(&output).contains("usage: july update"), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
    }
}

#[test]
fn update_never_reports_success_without_installing() {
    let output = run(&["update"]);
    let stdout = String::from_utf8(output.stdout.clone()).unwrap();
    let stderr = stderr(&output);
    // Không có mạng, không có release, hoặc có bản mới đều không được exit 0 kèm
    // thông báo đã cập nhật. Chỉ "already up to date" và "no downgrade" là 0.
    if output.status.success() {
        assert!(
            stdout.contains("already up to date") || stdout.contains("No downgrade"),
            "{stdout}"
        );
    } else {
        assert!(
            stderr.contains("Current installation was not changed.")
                || stderr.contains("cannot install releases yet"),
            "{stderr}"
        );
        assert!(
            stderr.contains("Unable to check for July updates")
                || stderr.contains("cannot install releases yet")
                || stderr.contains("no stable release")
                || stderr.contains("publishes no asset"),
            "{stderr}"
        );
    }
    assert!(!stdout.contains("is ready."), "{stdout}");
}
