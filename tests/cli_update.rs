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
fn internal_update_rejects_missing_lock() {
    let output = run(&["--update-finalize", env!("CARGO_PKG_VERSION"), "198"]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("invalid update handoff"),
        "{}",
        stderr(&output)
    );
}

#[cfg(unix)]
#[test]
fn update_uses_isolated_release_and_hands_off_with_lock() {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!("july-cli-update-{}", ulid::Ulid::generate()));
    std::fs::create_dir_all(&root).unwrap();
    let binary = root.join("july");
    let original = std::fs::read(env!("CARGO_BIN_EXE_july")).unwrap();
    std::fs::write(&binary, &original).unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(
        root.join(".july-install-receipt"),
        format!(
            "{}\n{:x}\n",
            binary.canonicalize().unwrap().display(),
            Sha256::digest(&original)
        ),
    )
    .unwrap();
    let payload = root.join("payload");
    std::fs::create_dir(&payload).unwrap();
    std::fs::write(
        payload.join("july"),
        r#"#!/usr/bin/python3
import sys, os, fcntl
if sys.argv[1:] == ['--version']:
    print('july 9.0.0'); sys.exit(0)
assert sys.argv[1:3] == ['--update-finalize', '9.0.0'], sys.argv
fd = int(sys.argv[3])
lock = os.path.join(os.environ['JULY_HOME'], 'updates', '.update-lock')
assert os.fstat(fd).st_ino == os.stat(lock).st_ino
with open(lock) as contender:
    try: fcntl.flock(contender, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError: pass
    else: raise AssertionError('lock lost during handoff')
fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
print('NEW BINARY HOLDS UPDATE LOCK')
print('migrations and runtime reconciliation are not implemented yet', file=sys.stderr)
sys.exit(1)
"#,
    )
    .unwrap();
    let archive = root.join("release.tar.gz");
    assert!(
        Command::new("/usr/bin/tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&payload)
            .arg("july")
            .status()
            .unwrap()
            .success()
    );
    let tools = root.join("tools");
    std::fs::create_dir(&tools).unwrap();
    std::os::unix::fs::symlink("/usr/bin/tar", tools.join("tar")).unwrap();
    std::fs::write(tools.join("curl"), r#"#!/usr/bin/python3
import sys, os, shutil
if '--output' in sys.argv:
    shutil.copyfile(os.environ['TEST_ARCHIVE'], sys.argv[sys.argv.index('--output') + 1]); print('200', end='')
else:
    print(open(os.environ['TEST_METADATA']).read()); print('200', end='')
"#).unwrap();
    std::fs::set_permissions(tools.join("curl"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let name = format!(
        "july-9.0.0-{}.tar.gz",
        july_workspace::update::target_triple().unwrap()
    );
    let staging = root.join("home/updates");
    std::fs::create_dir_all(&staging).unwrap();
    let sentinel = root.join("download-sentinel");
    std::fs::write(&sentinel, "preserve download target").unwrap();
    let old_partial = staging.join(format!("{name}.part"));
    std::os::unix::fs::symlink(&sentinel, &old_partial).unwrap();
    for valid in [false, true] {
        let digest = if valid {
            format!("{:x}", Sha256::digest(std::fs::read(&archive).unwrap()))
        } else {
            "0".repeat(64)
        };
        let metadata = serde_json::json!({"tag_name":"v9.0.0", "draft":false,"prerelease":false,"assets":[{"name":name,"browser_download_url":format!("https://github.com/tranngoc8x/july_workspace/releases/download/v9.0.0/{name}"),"digest":format!("sha256:{digest}")}]});
        std::fs::write(root.join("metadata"), metadata.to_string()).unwrap();
        let output = Command::new(&binary)
            .arg("update")
            .env("PATH", &tools)
            .env("HOME", &root)
            .env("JULY_HOME", root.join("home"))
            .env("JULY_WORKSPACE_DB", root.join("db"))
            .env("TEST_ARCHIVE", &archive)
            .env("TEST_METADATA", root.join("metadata"))
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert_eq!(
            std::fs::read(&sentinel).unwrap(),
            b"preserve download target"
        );
        assert!(
            std::fs::symlink_metadata(&old_partial)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let out = String::from_utf8_lossy(&output.stdout);
        let err = stderr(&output);
        if valid {
            assert!(out.contains("NEW BINARY HOLDS UPDATE LOCK"), "{out}\n{err}");
            assert!(
                err.contains("migrations and runtime reconciliation"),
                "{err}"
            );
        } else {
            assert!(err.contains("failed checksum verification"), "{err}");
            assert_eq!(std::fs::read(&binary).unwrap(), original);
        }
    }
    assert!(!root.join("db").exists());
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn internal_update_requires_matching_version_and_inherited_lock() {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;
    let root = std::env::temp_dir().join(format!("july-handoff-{}", ulid::Ulid::generate()));
    let staging = root.join("updates");
    std::fs::create_dir_all(&staging).unwrap();
    let path = july_workspace::update::lock_path(&staging);
    let file = std::fs::File::create(&path).unwrap();
    let fd = file.as_raw_fd();
    for (locked, version, expected) in [
        (false, env!("CARGO_PKG_VERSION"), "update lock was not held"),
        (true, "99.0.0", "binary version mismatch"),
        (
            true,
            env!("CARGO_PKG_VERSION"),
            "migrations and runtime reconciliation",
        ),
    ] {
        if locked {
            assert_eq!(unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) }, 0);
        }
        let mut command = Command::new(env!("CARGO_BIN_EXE_july"));
        command
            .args(["--update-finalize", version, &fd.to_string()])
            .env("JULY_HOME", &root)
            .env("JULY_WORKSPACE_DB", root.join("db"));
        unsafe {
            command.pre_exec(move || {
                if libc::fcntl(fd, libc::F_SETFD, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let output = command.output().unwrap();
        assert!(!output.status.success());
        assert!(stderr(&output).contains(expected), "{}", stderr(&output));
        if locked && version == env!("CARGO_PKG_VERSION") {
            assert!(
                String::from_utf8_lossy(&output.stdout).contains("Continuing update with July")
            );
        }
    }
    drop(file);
    let sentinel = root.join("sentinel");
    std::fs::write(&sentinel, "preserve").unwrap();
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(&sentinel, &path).unwrap();
    assert!(july_workspace::update::UpdateLock::acquire(&staging).is_err());
    assert_eq!(std::fs::read_to_string(&sentinel).unwrap(), "preserve");
    assert!(!root.join("db").exists());
    std::fs::remove_dir_all(root).unwrap();
}
