//! `july update` chỉ nhận đúng một dạng lời gọi, luôn chạy migration và
//! reconciliation bằng spec của binary đang chạy, và không bao giờ báo thành
//! công khi còn một phần chưa xong.

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
        (
            false,
            env!("CARGO_PKG_VERSION"),
            Some("update lock was not held"),
        ),
        (true, "99.0.0", Some("binary version mismatch")),
        // Khoá hợp lệ: binary mới chạy nốt migration và reconciliation.
        (true, env!("CARGO_PKG_VERSION"), None),
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
        let out = String::from_utf8_lossy(&output.stdout);
        match expected {
            Some(reason) => {
                assert!(!output.status.success());
                assert!(stderr(&output).contains(reason), "{}", stderr(&output));
            }
            None => {
                assert!(output.status.success(), "{out}\n{}", stderr(&output));
                assert!(out.contains("Continuing update with July"), "{out}");
                assert!(out.contains("Workspace schema"), "{out}");
                assert!(
                    out.contains(&format!("July {} is ready.", env!("CARGO_PKG_VERSION"))),
                    "{out}"
                );
            }
        }
    }
    drop(file);
    let sentinel = root.join("sentinel");
    std::fs::write(&sentinel, "preserve").unwrap();
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(&sentinel, &path).unwrap();
    assert!(july_workspace::update::UpdateLock::acquire(&staging).is_err());
    assert_eq!(std::fs::read_to_string(&sentinel).unwrap(), "preserve");
    // Migration của handoff hợp lệ đã tạo workspace database.
    assert!(root.join("db").exists());
    std::fs::remove_dir_all(root).unwrap();
}

/// Môi trường update khép kín: curl giả trả metadata từ file, PATH chỉ có công
/// cụ do test đặt, và mọi trạng thái nằm trong một thư mục tạm.
#[cfg(unix)]
struct Sandbox {
    root: std::path::PathBuf,
    tools: std::path::PathBuf,
}

#[cfg(unix)]
impl Sandbox {
    fn new(tag: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("july-{tag}-{}", ulid::Ulid::generate()));
        let tools = root.join("tools");
        std::fs::create_dir_all(&tools).unwrap();
        std::fs::write(
            tools.join("curl"),
            "#!/usr/bin/python3\nimport sys, os\nprint(open(os.environ['TEST_METADATA']).read()); print('200', end='')\n",
        )
        .unwrap();
        std::fs::set_permissions(tools.join("curl"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        Self { root, tools }
    }

    /// Release ổn định duy nhất mà July nhìn thấy, không kèm asset nào vì các
    /// đường không thay binary không bao giờ chọn asset.
    fn publishes(&self, tag: &str) -> &Self {
        std::fs::write(
            self.root.join("metadata"),
            serde_json::json!({"tag_name": tag, "draft": false, "prerelease": false, "assets": []})
                .to_string(),
        )
        .unwrap();
        self
    }

    fn database(&self) -> std::path::PathBuf {
        self.root.join("db")
    }

    /// Adapter đã được ghi nhận nhưng chưa có receipt cài đặt của July.
    fn records_adapter(&self, id: &str, bin: &std::path::Path) -> &Self {
        let adapters = self.root.join("home/adapters");
        std::fs::create_dir_all(&adapters).unwrap();
        std::fs::write(
            adapters.join("identities.json"),
            serde_json::json!({id: {"name": "test-acp-agent", "version": "1.0.0", "bin": bin}})
                .to_string(),
        )
        .unwrap();
        self
    }

    fn update(&self) -> Output {
        Command::new(env!("CARGO_BIN_EXE_july"))
            .arg("update")
            .env("PATH", &self.tools)
            .env("HOME", &self.root)
            .env("JULY_HOME", self.root.join("home"))
            .env("JULY_WORKSPACE_DB", self.database())
            .env("TEST_METADATA", self.root.join("metadata"))
            .output()
            .unwrap()
    }
}

#[cfg(unix)]
impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[cfg(unix)]
#[test]
fn already_latest_still_migrates_and_reconciles() {
    let sandbox = Sandbox::new("cli-update-latest");
    let output = sandbox
        .publishes(&format!("v{}", env!("CARGO_PKG_VERSION")))
        .update();
    let out = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success(), "{out}\n{}", stderr(&output));
    assert!(out.contains("is already up to date"), "{out}");
    // Binary không đổi nhưng spec của nó vẫn được áp lên hệ thống.
    assert!(out.contains("Workspace schema"), "{out}");
    assert!(out.contains("System is up to date."), "{out}");
    assert!(sandbox.database().exists());
}

#[cfg(unix)]
#[test]
fn a_newer_local_july_reconciles_without_downgrading() {
    let sandbox = Sandbox::new("cli-update-newer");
    let output = sandbox.publishes("v0.0.1").update();
    let out = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success(), "{out}\n{}", stderr(&output));
    assert!(out.contains("No downgrade was performed."), "{out}");
    assert!(out.contains("Workspace schema"), "{out}");
    assert!(out.contains("System is up to date."), "{out}");
}

#[cfg(unix)]
#[test]
fn a_failed_runtime_is_reported_instead_of_reported_as_success() {
    use std::os::unix::fs::PermissionsExt;
    let sandbox = Sandbox::new("cli-update-runtime");
    // Trình cài duy nhất July tìm thấy luôn hỏng, nên adapter thiếu không thể
    // được đưa về trạng thái tương thích.
    std::fs::write(sandbox.tools.join("npm"), "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(
        sandbox.tools.join("npm"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let missing = sandbox
        .root
        .join("home/adapters/node_modules/.bin/codex-acp");
    let output = sandbox
        .publishes(&format!("v{}", env!("CARGO_PKG_VERSION")))
        .records_adapter("codex", &missing)
        .update();
    let out = String::from_utf8_lossy(&output.stdout);
    let err = stderr(&output);

    assert!(!output.status.success(), "{out}");
    assert!(out.contains("✗ codex"), "{out}");
    assert!(err.contains("1 runtime requires attention"), "{err}");
    assert!(err.contains("- codex:"), "{err}");
    // Partial failure không bao giờ được in ra như một lần update trọn vẹn.
    assert!(!out.contains("System is up to date."), "{out}");
    assert!(!out.contains("is ready."), "{out}");
}

#[cfg(unix)]
#[test]
fn reconciliation_keeps_a_compatible_runtime_and_every_user_owned_record() {
    use july_workspace::domain::{Agent, AgentId};
    use july_workspace::storage::SqliteStore;
    use std::os::unix::fs::PermissionsExt;
    let sandbox = Sandbox::new("cli-update-preserve");
    let adapter = sandbox.tools.join("codex-acp");
    std::fs::write(
        &adapter,
        format!(
            "#!/bin/sh\nif [ \"$1\" = --version ]; then echo '1.10.0'; exit; fi\nexec /usr/bin/python3 '{}/tests/fixtures/acp_agent.py'\n",
            env!("CARGO_MANIFEST_DIR")
        ),
    )
    .unwrap();
    std::fs::set_permissions(&adapter, std::fs::Permissions::from_mode(0o755)).unwrap();

    let agent = Agent {
        id: AgentId::new(),
        name: "cashpoint".into(),
        project_root: "/tmp/cashpoint".into(),
        transport_type: "acp".into(),
        transport_config: serde_json::json!({"model": "user-chosen"}),
        status: "active".into(),
        metadata: serde_json::json!({"instructions": "user-owned"}),
        created_at: "2026-01-01T00:00:00Z".into(),
        updated_at: "2026-01-01T00:00:00Z".into(),
    };
    let store = SqliteStore::open(sandbox.database()).unwrap();
    store.insert_agent(&agent).unwrap();
    let schema_before = store.schema_version().unwrap();
    drop(store);

    let output = sandbox
        .publishes(&format!("v{}", env!("CARGO_PKG_VERSION")))
        .records_adapter("codex", &adapter)
        .update();
    let out = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success(), "{out}\n{}", stderr(&output));
    assert!(out.contains("✓ codex 1.10.0"), "{out}");
    assert!(out.contains("System is up to date."), "{out}");

    let store = SqliteStore::open(sandbox.database()).unwrap();
    assert_eq!(store.schema_version().unwrap(), schema_before);
    let preserved = store.get_agent(agent.id).unwrap().expect("agent survives");
    assert_eq!(preserved.name, agent.name);
    assert_eq!(preserved.project_root, agent.project_root);
    assert_eq!(preserved.transport_config, agent.transport_config);
    assert_eq!(preserved.metadata, agent.metadata);
}
