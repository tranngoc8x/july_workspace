//! Resolve and probe the executable selected for a future adapter launch.

use super::AdapterSpec;
use semver::Version;
use std::{
    collections::BTreeMap,
    ffi::{CString, OsStr},
    io,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{io::AsyncReadExt, process::Command};

/// Resolution origin only; this does not grant authority to overwrite config or files.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutableSource {
    Explicit,
    Managed,
    Path,
}

/// Absolute launch path. Preserve symlinks because shims can depend on argv[0].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedExecutable {
    pub path: PathBuf,
    pub source: ExecutableSource,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VersionStatus {
    Detected(Version),
    UnknownVersion,
    Broken(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VersionDetection {
    pub executable: ResolvedExecutable,
    pub status: VersionStatus,
}

/// Select once, then use the returned path for probing, identity recording and launch.
/// `None` means missing. A selected explicit/managed path never falls through on failure.
/// Pass the launch environment's PATH override, or inherited `std::env::var_os("PATH")`.
/// An absent PATH disables lookup; relative/empty PATH entries use the current directory,
/// matching the existing launch context (which does not override current_dir).
pub fn resolve_executable(
    spec: &AdapterSpec,
    explicit: Option<&Path>,
    managed: &Path,
    search_path: Option<&OsStr>,
) -> io::Result<Option<ResolvedExecutable>> {
    if let Some(path) = explicit {
        if !path.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "configured executable must be absolute",
            ));
        }
        return Ok(Some(ResolvedExecutable {
            path: path.to_owned(),
            source: ExecutableSource::Explicit,
        }));
    }
    // symlink_metadata also detects a dangling managed link. Permission errors must
    // remain attached to the managed candidate instead of silently selecting PATH.
    if !matches!(std::fs::symlink_metadata(managed), Err(error) if error.kind() == io::ErrorKind::NotFound)
    {
        return Ok(Some(ResolvedExecutable {
            path: std::path::absolute(managed)?,
            source: ExecutableSource::Managed,
        }));
    }
    if let Some(search_path) = search_path {
        for directory in std::env::split_paths(search_path) {
            let path = directory.join(spec.bin);
            if let Ok(metadata) = std::fs::metadata(&path)
                && metadata.is_file()
                && can_execute(&path)
            {
                return Ok(Some(ResolvedExecutable {
                    path: std::path::absolute(path)?,
                    source: ExecutableSource::Path,
                }));
            }
        }
    }
    Ok(None)
}

fn can_execute(path: &Path) -> bool {
    let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: path is a valid NUL-terminated string, and this call only checks access.
    unsafe { libc::faccessat(libc::AT_FDCWD, path.as_ptr(), libc::X_OK, libc::AT_EACCESS) == 0 }
}

// Version commands may be wrappers. Their descendants must not survive timeout,
// excess output, or cancellation of the probe future.
struct ProbeProcessGroup(u32);
impl Drop for ProbeProcessGroup {
    fn drop(&mut self) {
        // SAFETY: spawn creates a separate process group whose ID is this child's PID.
        unsafe {
            libc::kill(-(self.0 as i32), libc::SIGKILL);
        }
    }
}

// Accept a bare SemVer or exactly the catalog binary/package name followed by SemVer.
// Multiple lines/tokens (including unrelated version logs) are deliberately unknown.
fn parse_version(spec: &AdapterSpec, output: &str) -> Option<Version> {
    let tokens: Vec<_> = output.split_whitespace().collect();
    let raw = match tokens.as_slice() {
        [version] => *version,
        [name, version] if *name == spec.bin || *name == spec.package => *version,
        _ => return None,
    };
    Version::parse(raw.strip_prefix('v').unwrap_or(raw)).ok()
}

/// Probe package SemVer independently of ACP initialize identity. Uses launch arguments
/// and environment overrides, then appends --version; never invokes a shell.
/// Bounds combined stdout/stderr to 64 KiB and the whole probe to five seconds.
pub async fn probe_version(
    spec: &AdapterSpec,
    executable: ResolvedExecutable,
    arguments: &[String],
    environment: &BTreeMap<String, String>,
) -> VersionDetection {
    probe_with_limits(
        spec,
        executable,
        arguments,
        environment,
        Duration::from_secs(5),
        64 * 1024,
    )
    .await
}

async fn probe_with_limits(
    spec: &AdapterSpec,
    executable: ResolvedExecutable,
    arguments: &[String],
    environment: &BTreeMap<String, String>,
    timeout: Duration,
    limit: usize,
) -> VersionDetection {
    let status = probe_output(&executable.path, arguments, environment, timeout, limit).await;
    let status = match status {
        Ok(output) => parse_version(spec, &output)
            .map_or(VersionStatus::UnknownVersion, VersionStatus::Detected),
        Err(reason) => VersionStatus::Broken(reason),
    };
    VersionDetection { executable, status }
}

async fn probe_output(
    path: &Path,
    arguments: &[String],
    environment: &BTreeMap<String, String>,
    timeout: Duration,
    limit: usize,
) -> Result<String, String> {
    if !path.is_absolute() {
        return Err("configured executable must be absolute".into());
    }
    let metadata =
        std::fs::metadata(path).map_err(|error| format!("executable unavailable: {error}"))?;
    if !metadata.is_file() {
        return Err("executable is not a file".into());
    }
    let mut child = Command::new(path)
        .args(arguments)
        .arg("--version")
        .envs(environment)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .process_group(0)
        .spawn()
        .map_err(|error| format!("version command could not start: {error}"))?;
    let group = ProbeProcessGroup(child.id().expect("newly spawned process"));
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let result = tokio::time::timeout(timeout, async {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let (mut out_done, mut err_done) = (false, false);
        let (mut out_buffer, mut err_buffer) = ([0; 4096], [0; 4096]);
        while !out_done || !err_done {
            tokio::select! {
                read = stdout.read(&mut out_buffer), if !out_done => {
                    let count = read.map_err(|error| format!("stdout read failed: {error}"))?;
                    out_done = count == 0;
                    if out.len() + err.len() + count > limit { return Err("version output limit exceeded".into()); }
                    out.extend_from_slice(&out_buffer[..count]);
                }
                read = stderr.read(&mut err_buffer), if !err_done => {
                    let count = read.map_err(|error| format!("stderr read failed: {error}"))?;
                    err_done = count == 0;
                    if out.len() + err.len() + count > limit { return Err("version output limit exceeded".into()); }
                    err.extend_from_slice(&err_buffer[..count]);
                }
            }
        }
        let status = child.wait().await.map_err(|error| format!("version command wait failed: {error}"))?;
        if !status.success() { return Err(format!("version command failed: {status}")); }
        out.push(b'\n');
        out.extend(err);
        // Invalid UTF-8 is unparseable output rather than a process failure.
        Ok(String::from_utf8_lossy(&out).into_owned())
    }).await.unwrap_or_else(|_| Err("version command timed out".into()));
    drop(group);
    if result.is_err() {
        // Kill/reap the direct child after stopping its group and closing both pipes.
        // No blocking reader threads survive the probe deadline.
        drop(stdout);
        drop(stderr);
        child
            .kill()
            .await
            .map_err(|error| format!("version command cleanup failed: {error}"))?;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::find;
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
    };

    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("july-detection-{}", ulid::Ulid::generate()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn script(&self, name: &str, body: &str) -> PathBuf {
            let path = self.0.join(name);
            fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            path
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn resolution_preserves_precedence_and_selected_symlinks() {
        let scratch = Scratch::new();
        let spec = find("codex").unwrap();
        let path_bin = scratch.script(spec.bin, "echo 9.0.0");
        let managed = scratch.script("managed", "echo 1.10.0");
        let explicit = scratch.0.join("explicit");
        symlink(&managed, &explicit).unwrap();
        let search = Some(scratch.0.as_os_str());
        let selected = resolve_executable(spec, Some(&explicit), &managed, search)
            .unwrap()
            .unwrap();
        assert_eq!(
            selected,
            ResolvedExecutable {
                path: explicit.clone(),
                source: ExecutableSource::Explicit
            }
        );
        fs::remove_file(&managed).unwrap();
        assert_eq!(
            resolve_executable(spec, Some(&explicit), &managed, search)
                .unwrap()
                .unwrap()
                .path,
            explicit
        );
        symlink(scratch.0.join("missing"), &managed).unwrap();
        assert_eq!(
            resolve_executable(spec, None, &managed, search)
                .unwrap()
                .unwrap()
                .source,
            ExecutableSource::Managed
        );
        fs::remove_file(&managed).unwrap();
        assert_eq!(
            resolve_executable(spec, None, &managed, search)
                .unwrap()
                .unwrap()
                .path,
            path_bin
        );
        assert!(
            resolve_executable(spec, None, &managed, None)
                .unwrap()
                .is_none()
        );
        assert!(resolve_executable(spec, Some(Path::new("relative")), &managed, search).is_err());
    }

    #[test]
    fn parser_accepts_only_unambiguous_adapter_version_output() {
        let spec = find("codex").unwrap();
        for output in [
            "1.10.0",
            "v1.10.0",
            "codex-acp 1.10.0",
            "@agentclientprotocol/codex-acp v1.10.0\n",
        ] {
            assert_eq!(
                parse_version(spec, output),
                Some(Version::new(1, 10, 0)),
                "{output}"
            );
        }
        assert_eq!(
            parse_version(spec, "codex-acp 1.10.0-beta.1+build.2"),
            Some(Version::parse("1.10.0-beta.1+build.2").unwrap())
        );
        for output in [
            "",
            "node 22.0.0",
            "log 1.10.0 ready",
            "1.10",
            "1.10.0\n2.0.0",
            "codex-acp 1.10.0\nnode 22.0.0",
        ] {
            assert_eq!(parse_version(spec, output), None, "{output}");
        }
    }

    #[tokio::test]
    async fn probe_uses_selected_path_arguments_environment_and_stderr() {
        let scratch = Scratch::new();
        let path = scratch.script("custom", "[ \"$1\" = '--custom' ] && [ \"$2\" = '--version' ] || exit 4\nprintf 'codex-acp %s\\n' \"$TEST_VERSION\" >&2");
        let selected = ResolvedExecutable {
            path: path.clone(),
            source: ExecutableSource::Explicit,
        };
        let result = probe_version(
            find("codex").unwrap(),
            selected,
            &["--custom".into()],
            &BTreeMap::from([("TEST_VERSION".into(), "1.12.0".into())]),
        )
        .await;
        assert_eq!(result.executable.path, path);
        assert_eq!(
            result.status,
            VersionStatus::Detected(Version::new(1, 12, 0))
        );
    }

    #[tokio::test]
    async fn probe_distinguishes_failure_unknown_timeout_and_excess_output() {
        let scratch = Scratch::new();
        let spec = find("codex").unwrap();
        for (name, body, expected) in [
            ("failure", "echo 1.10.0; exit 7", "failed"),
            ("unknown", "echo node 22.0.0", "unknown"),
            ("timeout", "while :; do :; done", "timed out"),
            (
                "overflow",
                "while :; do printf '0123456789012345678901234567890123456789'; done",
                "output limit",
            ),
        ] {
            let selected = ResolvedExecutable {
                path: scratch.script(name, body),
                source: ExecutableSource::Managed,
            };
            let result = probe_with_limits(
                spec,
                selected,
                &[],
                &BTreeMap::new(),
                if name == "timeout" {
                    Duration::from_millis(300)
                } else {
                    Duration::from_secs(5)
                },
                128,
            )
            .await;
            match result.status {
                VersionStatus::UnknownVersion => assert_eq!(expected, "unknown"),
                VersionStatus::Broken(reason) => {
                    assert!(reason.contains(expected), "{name}: {reason}")
                }
                other => panic!("unexpected: {other:?}"),
            }
        }
        let selected = ResolvedExecutable {
            path: scratch.0.join("missing"),
            source: ExecutableSource::Explicit,
        };
        assert!(matches!(
            probe_version(spec, selected, &[], &BTreeMap::new())
                .await
                .status,
            VersionStatus::Broken(_)
        ));
    }
    #[tokio::test]
    async fn timeout_stops_a_wrapper_and_its_descendant() {
        let scratch = Scratch::new();
        let pid_file = scratch.0.join("child.pid");
        let selected = ResolvedExecutable {
            path: scratch.script("wrapper", "/bin/sleep 30 &\necho $! > \"$PID_FILE\"\nwait"),
            source: ExecutableSource::Explicit,
        };
        let started = std::time::Instant::now();
        let result = probe_with_limits(
            find("codex").unwrap(),
            selected,
            &[],
            &BTreeMap::from([("PID_FILE".into(), pid_file.to_string_lossy().into_owned())]),
            Duration::from_secs(2),
            128,
        )
        .await;
        assert!(
            matches!(result.status, VersionStatus::Broken(reason) if reason.contains("timed out"))
        );
        assert!(started.elapsed() < Duration::from_secs(5));
        let pid: i32 = fs::read_to_string(pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        struct Cleanup(i32);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                // SAFETY: terminate only the test-owned sleep process.
                unsafe {
                    libc::kill(self.0, libc::SIGKILL);
                }
            }
        }
        let _cleanup = Cleanup(pid);
        for _ in 0..100 {
            // SAFETY: signal zero only checks whether the test child remains alive.
            if unsafe { libc::kill(pid, 0) } == -1 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("probe left descendant {pid} alive");
    }

    #[test]
    fn path_skips_files_the_current_user_cannot_execute() {
        let first = Scratch::new();
        let second = Scratch::new();
        let spec = find("codex").unwrap();
        let denied = first.script(spec.bin, "echo 1.10.0");
        // Root can execute any file with an execute bit; keep the fixture denied
        // there too, while non-root runs catch the owner-vs-other permission bug.
        // SAFETY: geteuid only reads the effective user ID.
        let denied_mode = if unsafe { libc::geteuid() } == 0 {
            0o000
        } else {
            0o001
        };
        fs::set_permissions(&denied, fs::Permissions::from_mode(denied_mode)).unwrap();
        let expected = second.script(spec.bin, "echo 1.12.0");
        let search = std::env::join_paths([&first.0, &second.0]).unwrap();
        assert_eq!(
            resolve_executable(spec, None, &first.0.join("missing"), Some(&search))
                .unwrap()
                .unwrap()
                .path,
            expected
        );
    }
    #[tokio::test]
    async fn detected_path_survives_identity_recording_into_launch_config() {
        use crate::adapter::{AdapterIdentity, AdapterStore};
        let scratch = Scratch::new();
        let spec = find("codex").unwrap();
        let executable = scratch.script(spec.bin, "echo 1.12.0");
        let selected = resolve_executable(
            spec,
            None,
            &scratch.0.join("missing"),
            Some(scratch.0.as_os_str()),
        )
        .unwrap()
        .unwrap();
        let detected = probe_version(spec, selected, &[], &BTreeMap::new()).await;
        assert_eq!(
            detected.status,
            VersionStatus::Detected(Version::new(1, 12, 0))
        );
        let store = AdapterStore::new(scratch.0.join("july"));
        store
            .record_identity(
                spec.id,
                AdapterIdentity {
                    name: "ACP fixture identity".into(),
                    version: "identity-build-42".into(),
                    bin: detected.executable.path,
                },
            )
            .unwrap();
        let config =
            crate::runtime::parse_acp_config(&store.config_for(spec.id, "test-agent").unwrap())
                .unwrap();
        assert_eq!(config.executable, executable);
        assert_eq!(config.expected_agent_name, "ACP fixture identity");
        assert_eq!(config.expected_agent_version, "identity-build-42");
    }

    #[tokio::test]
    async fn cancelling_probe_stops_its_process_group() {
        let scratch = Scratch::new();
        let pid_file = scratch.0.join("cancel.pid");
        let selected = ResolvedExecutable {
            path: scratch.script("cancel", "/bin/sleep 30 &\necho $! > \"$PID_FILE\"\nwait"),
            source: ExecutableSource::Explicit,
        };
        let environment =
            BTreeMap::from([("PID_FILE".into(), pid_file.to_string_lossy().into_owned())]);
        let task = tokio::spawn(async move {
            probe_version(find("codex").unwrap(), selected, &[], &environment).await
        });
        for _ in 0..150 {
            if fs::read_to_string(&pid_file).is_ok_and(|text| text.trim().parse::<i32>().is_ok()) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        task.abort();
        let _ = task.await;
        let pid: i32 = fs::read_to_string(pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        for _ in 0..100 {
            // SAFETY: signal zero only checks the test-owned child.
            if unsafe { libc::kill(pid, 0) } == -1 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // SAFETY: cleanup is limited to the test-owned child even if the assertion fails.
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
        panic!("cancelled probe left descendant {pid} alive");
    }
}
