//! Thay thế binary July đã xác minh, và khoá để hai lần update không chồng nhau.
//!
//! Mọi bước tốn kém chạy trên bản sao trong thư mục staging; file thực thi
//! đang dùng chỉ bị đụng tới đúng một lần bằng `rename`, sau khi bản mới đã tự
//! báo đúng phiên bản. Hỏng ở bất kỳ bước nào trước đó thì July cũ vẫn chạy.

use semver::Version;
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs::File;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// Quyền thực thi của một binary do July đặt xuống.
const EXECUTABLE_MODE: u32 = 0o755;

/// Ai sở hữu file thực thi July đang chạy.
///
/// July chỉ ghi đè file do chính nó đặt xuống. File thuộc một package manager
/// vẫn do package manager đó quản lý, nên July báo lệnh đúng thay vì tạo ra
/// một bản cài đặt mà công cụ kia không còn hiểu.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InstallOwnership {
    Standalone,
    Unknown,
    Managed {
        manager: &'static str,
        command: &'static str,
    },
}

pub fn classify_install(executable: &Path) -> InstallOwnership {
    let path = executable.to_string_lossy();
    for (needle, manager, command) in [
        (".cargo/bin/", "cargo", "cargo install --path . --force"),
        ("/Cellar/", "Homebrew", "brew upgrade july"),
        ("/homebrew/", "Homebrew", "brew upgrade july"),
        ("/node_modules/", "npm", "npm update -g july"),
    ] {
        if path.contains(needle) {
            return InstallOwnership::Managed { manager, command };
        }
    }
    if receipt_contents(executable).ok().is_some_and(|expected| {
        std::fs::read_to_string(receipt_path(executable))
            .ok()
            .as_ref()
            == Some(&expected)
    }) {
        InstallOwnership::Standalone
    } else {
        InstallOwnership::Unknown
    }
}

fn receipt_path(executable: &Path) -> PathBuf {
    executable.with_file_name(".july-install-receipt")
}

fn receipt_contents(executable: &Path) -> std::io::Result<String> {
    let path = executable.canonicalize()?;
    let digest = Sha256::digest(std::fs::read(&path)?);
    Ok(format!("{}\n{digest:x}\n", path.display()))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InstallError {
    /// Một tiến trình July khác đang update cùng lúc.
    Locked,
    Extract(String),
    /// Bản vừa bung ra không tự nhận đúng phiên bản, nên không được cài.
    Verify(String),
    Io(String),
    ReceiptRenewal(String),
}

impl fmt::Display for InstallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Locked => formatter.write_str("Another July update is in progress."),
            Self::Extract(reason) => {
                write!(formatter, "release archive could not be opened: {reason}")
            }
            Self::Verify(reason) => write!(formatter, "downloaded July did not verify: {reason}"),
            Self::ReceiptRenewal(reason) => write!(
                formatter,
                "July was replaced but its standalone receipt could not be renewed: {reason}"
            ),
            Self::Io(reason) => write!(formatter, "July could not be replaced: {reason}"),
        }
    }
}

pub fn lock_path(staging: &Path) -> PathBuf {
    staging.join(".update-lock")
}

/// Khoá update giữ bằng `flock`, nên kernel tự nhả khi tiến trình kết thúc;
/// một lần update bị kill không bao giờ để lại khoá kẹt cho lần sau.
pub struct UpdateLock {
    file: File,
}

impl Drop for UpdateLock {
    /// Nhả khoá tường minh thay vì trông vào lúc `close` xảy ra, để lần update
    /// ngay sau đó trong cùng tiến trình lấy được khoá.
    fn drop(&mut self) {
        // SAFETY: fd còn sống tới hết lời gọi vì `self.file` chưa bị đóng.
        unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

impl UpdateLock {
    pub(crate) fn raw_fd(&self) -> std::os::fd::RawFd {
        self.file.as_raw_fd()
    }

    pub fn acquire(staging: &Path) -> Result<Self, InstallError> {
        std::fs::create_dir_all(staging).map_err(|error| InstallError::Io(error.to_string()))?;
        use std::os::unix::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(lock_path(staging))
            .map_err(|error| InstallError::Io(error.to_string()))?;
        if !file
            .metadata()
            .map_err(|error| InstallError::Io(error.to_string()))?
            .is_file()
        {
            return Err(InstallError::Io("update lock is not a regular file".into()));
        }
        loop {
            // SAFETY: fd hợp lệ và còn sống hết lời gọi; LOCK_NB nên không chặn.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Ok(Self { file });
            }
            let error = std::io::Error::last_os_error();
            match error.raw_os_error() {
                // Một signal cắt ngang lời gọi không nói gì về chủ khoá.
                Some(libc::EINTR) => continue,
                Some(libc::EWOULDBLOCK) => return Err(InstallError::Locked),
                _ => return Err(InstallError::Io(error.to_string())),
            }
        }
    }
}

/// Bung tarball đã xác minh, kiểm tra bản mới rồi thay `executable` bằng đúng
/// một lần `rename`.
///
/// File tạm nằm cùng thư mục với đích nên `rename` là thao tác nguyên tử trên
/// cùng filesystem. Trên Unix, đổi tên đè lên file thực thi đang chạy là an
/// toàn: tiến trình hiện tại giữ inode cũ cho tới khi nó thoát.
pub async fn install_release(
    archive: &Path,
    version: &Version,
    executable: &Path,
) -> Result<(), InstallError> {
    if classify_install(executable) != InstallOwnership::Standalone {
        return Err(InstallError::Io(
            "installation has no matching standalone receipt; use its installer".into(),
        ));
    }
    let workspace = archive.with_extension("unpacked");
    let _ = std::fs::remove_dir_all(&workspace);
    std::fs::create_dir_all(&workspace).map_err(|error| InstallError::Io(error.to_string()))?;
    let outcome = unpack_and_swap(archive, version, executable, &workspace).await;
    let _ = std::fs::remove_dir_all(&workspace);
    outcome
}

async fn unpack_and_swap(
    archive: &Path,
    version: &Version,
    executable: &Path,
    workspace: &Path,
) -> Result<(), InstallError> {
    extract(archive, workspace).await?;
    let unpacked = workspace.join("july");
    if !unpacked.is_file() {
        return Err(InstallError::Extract(
            "release archive contains no july binary".into(),
        ));
    }
    set_executable(&unpacked)?;
    verify_reported_version(&unpacked, version).await?;
    swap(&unpacked, executable)?;
    let receipt = receipt_contents(executable)
        .map_err(|error| InstallError::ReceiptRenewal(error.to_string()))?;
    let staged_receipt = executable.with_file_name(format!(".july-receipt-{}", std::process::id()));
    let mut file = File::options()
        .write(true)
        .create_new(true)
        .open(&staged_receipt)
        .map_err(|error| InstallError::ReceiptRenewal(error.to_string()))?;
    let result = (|| {
        file.write_all(receipt.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&staged_receipt, receipt_path(executable))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(staged_receipt);
    }
    result.map_err(|error| {
        InstallError::ReceiptRenewal(format!(
            "binary replaced but standalone receipt could not be renewed: {error}"
        ))
    })
}

async fn extract(archive: &Path, into: &Path) -> Result<(), InstallError> {
    let output = tokio::process::Command::new("tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(into)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|error| InstallError::Extract(format!("tar could not start: {error}")))?;
    if output.status.success() {
        return Ok(());
    }
    let reason = String::from_utf8_lossy(&output.stderr);
    let reason = reason.trim();
    Err(InstallError::Extract(if reason.is_empty() {
        output.status.to_string()
    } else {
        reason.to_owned()
    }))
}

/// Bản mới phải tự nhận đúng phiên bản trước khi được cài; July không suy ra
/// thành công từ việc bung file xong.
async fn verify_reported_version(binary: &Path, version: &Version) -> Result<(), InstallError> {
    let output = tokio::process::Command::new(binary)
        .arg("--version")
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|error| InstallError::Verify(format!("new binary could not start: {error}")))?;
    if !output.status.success() {
        return Err(InstallError::Verify(format!(
            "new binary exited with {}",
            output.status
        )));
    }
    let reported = String::from_utf8_lossy(&output.stdout);
    let reported = reported.trim();
    let expected = format!("july {version}");
    if reported != expected {
        return Err(InstallError::Verify(format!(
            "expected {expected}, got {reported}"
        )));
    }
    Ok(())
}

fn set_executable(path: &Path) -> Result<(), InstallError> {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(EXECUTABLE_MODE))
        .map_err(|error| InstallError::Io(error.to_string()))
}

/// Đặt bản mới cạnh đích rồi `rename` đè lên. Hỏng trước `rename` chỉ để lại
/// một file tạm, và file tạm đó bị dọn ngay.
fn swap(new_binary: &Path, executable: &Path) -> Result<(), InstallError> {
    let directory = executable.parent().ok_or_else(|| {
        InstallError::Io(format!("{} has no parent directory", executable.display()))
    })?;
    let staged = directory.join(format!(".july-update-{}", std::process::id()));
    place(new_binary, &staged)?;
    std::fs::rename(&staged, executable).map_err(|error| {
        let _ = std::fs::remove_file(&staged);
        InstallError::Io(error.to_string())
    })
}

fn place(new_binary: &Path, staged: &Path) -> Result<(), InstallError> {
    let bytes = std::fs::read(new_binary).map_err(|error| InstallError::Io(error.to_string()))?;
    let mut file = File::options()
        .write(true)
        .create_new(true)
        .open(staged)
        .map_err(|error| InstallError::Io(error.to_string()))?;
    let outcome = (|| {
        file.write_all(&bytes)?;
        file.set_permissions(std::fs::Permissions::from_mode(EXECUTABLE_MODE))?;
        file.sync_all()
    })();
    if outcome.is_err() {
        let _ = std::fs::remove_file(staged);
    }
    outcome.map_err(|error| InstallError::Io(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let path = std::env::temp_dir().join(format!("july-install-{}", ulid::Ulid::generate()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    /// Tarball chứa đúng một file `july` in ra phiên bản được yêu cầu.
    fn archive_reporting(root: &Path, reported: &str) -> PathBuf {
        let contents = root.join("contents");
        std::fs::create_dir_all(&contents).unwrap();
        std::fs::write(
            contents.join("july"),
            format!("#!/bin/sh\necho 'july {reported}'\n"),
        )
        .unwrap();
        set_executable(&contents.join("july")).unwrap();
        pack(&contents, root, "july")
    }

    fn pack(from: &Path, root: &Path, entry: &str) -> PathBuf {
        let archive = root.join("release.tar.gz");
        let status = std::process::Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(from)
            .arg(entry)
            .status()
            .unwrap();
        assert!(status.success());
        archive
    }

    fn installed(root: &Path) -> PathBuf {
        let executable = root.join("bin/july");
        std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
        std::fs::write(&executable, b"old july").unwrap();
        set_executable(&executable).unwrap();
        std::fs::write(
            receipt_path(&executable),
            receipt_contents(&executable).unwrap(),
        )
        .unwrap();
        executable
    }

    #[test]
    fn package_manager_installs_are_reported_not_overwritten() {
        for (path, manager) in [
            ("/Users/tony/.cargo/bin/july", "cargo"),
            ("/opt/homebrew/Cellar/july/0.9.0/bin/july", "Homebrew"),
            ("/home/tony/.linuxbrew/homebrew/bin/july", "Homebrew"),
            ("/usr/lib/node_modules/july/bin/july", "npm"),
        ] {
            assert!(
                matches!(
                    classify_install(Path::new(path)),
                    InstallOwnership::Managed { manager: found, .. } if found == manager
                ),
                "{path}"
            );
        }
        for path in [
            "/Users/tony/.local/bin/july",
            "/usr/local/bin/july",
            "/opt/july/july",
        ] {
            assert_eq!(
                classify_install(Path::new(path)),
                InstallOwnership::Unknown,
                "{path}"
            );
        }
    }

    #[test]
    fn staging_collision_does_not_follow_or_remove_an_existing_symlink() {
        let root = scratch();
        let executable = installed(&root);
        let incoming = root.join("incoming");
        std::fs::write(&incoming, b"new july").unwrap();
        let staged = executable
            .parent()
            .unwrap()
            .join(format!(".july-update-{}", std::process::id()));
        std::os::unix::fs::symlink(&executable, &staged).unwrap();
        assert!(swap(&incoming, &executable).is_err());
        assert_eq!(std::fs::read(&executable).unwrap(), b"old july");
        assert!(
            std::fs::symlink_metadata(&staged)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unrecognized_install_paths_are_not_authority_to_overwrite() {
        assert_ne!(
            classify_install(Path::new("/opt/cargo/bin/july")),
            InstallOwnership::Standalone
        );
    }

    #[test]
    fn receipt_must_match_current_binary_and_path() {
        let root = scratch();
        let executable = installed(&root);
        assert_eq!(classify_install(&executable), InstallOwnership::Standalone);
        std::fs::write(&executable, "package manager replacement").unwrap();
        assert_eq!(classify_install(&executable), InstallOwnership::Unknown);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_second_update_cannot_hold_the_lock() {
        let root = scratch();
        let staging = root.join("updates");
        let first = UpdateLock::acquire(&staging).unwrap();
        assert!(matches!(
            UpdateLock::acquire(&staging),
            Err(InstallError::Locked)
        ));
        assert!(lock_path(&staging).exists());
        // Nhả khoá rồi lần update sau lấy lại được; khoá không kẹt qua lượt.
        drop(first);
        let second = UpdateLock::acquire(&staging).unwrap();
        drop(second);
        assert!(UpdateLock::acquire(&staging).is_ok());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn a_verified_archive_replaces_the_running_binary_in_place() {
        let root = scratch();
        let archive = archive_reporting(&root, "0.9.0");
        let executable = installed(&root);
        let before = std::fs::metadata(&executable).unwrap();

        install_release(&archive, &Version::new(0, 9, 0), &executable)
            .await
            .unwrap();

        assert_eq!(classify_install(&executable), InstallOwnership::Standalone);
        let after = std::fs::read_to_string(&executable).unwrap();
        assert!(after.contains("echo 'july 0.9.0'"), "{after}");
        assert_eq!(
            std::fs::metadata(&executable).unwrap().permissions().mode() & 0o777,
            EXECUTABLE_MODE
        );
        // Thay bằng rename nên file mới là inode khác, không phải ghi đè tại chỗ.
        assert_ne!(before.len(), std::fs::metadata(&executable).unwrap().len());
        // Không còn rác staging cạnh binary hay cạnh archive.
        let leftovers: Vec<_> = std::fs::read_dir(executable.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name != "july" && name != ".july-install-receipt")
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        assert!(!archive.with_extension("unpacked").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn unknown_install_is_refused_before_reading_archive() {
        let root = scratch();
        let executable = installed(&root);
        std::fs::remove_file(receipt_path(&executable)).unwrap();
        let error = install_release(
            &root.join("absent.tar.gz"),
            &Version::new(0, 9, 0),
            &executable,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, InstallError::Io(_)));
        assert_eq!(std::fs::read(&executable).unwrap(), b"old july");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn receipt_failure_reports_that_binary_was_replaced() {
        let root = scratch();
        let archive = archive_reporting(&root, "0.9.0");
        let executable = installed(&root);
        let collision = executable.with_file_name(format!(".july-receipt-{}", std::process::id()));
        std::fs::write(&collision, "existing file").unwrap();
        let error = install_release(&archive, &Version::new(0, 9, 0), &executable)
            .await
            .unwrap_err();
        assert!(matches!(error, InstallError::ReceiptRenewal(_)));
        assert!(
            std::fs::read_to_string(&executable)
                .unwrap()
                .contains("july 0.9.0")
        );
        assert_eq!(std::fs::read_to_string(collision).unwrap(), "existing file");
        assert_eq!(classify_install(&executable), InstallOwnership::Unknown);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn a_failing_archive_leaves_the_current_binary_usable() {
        for (reported, entry) in [("0.8.0", "july"), ("0.9.0", "not-july")] {
            let root = scratch();
            let archive = if entry == "july" {
                archive_reporting(&root, reported)
            } else {
                let contents = root.join("contents");
                std::fs::create_dir_all(&contents).unwrap();
                std::fs::write(contents.join(entry), b"wrong payload").unwrap();
                pack(&contents, &root, entry)
            };
            let executable = installed(&root);

            let error = install_release(&archive, &Version::new(0, 9, 0), &executable)
                .await
                .unwrap_err();
            assert!(
                matches!(error, InstallError::Verify(_) | InstallError::Extract(_)),
                "{error}"
            );
            assert_eq!(std::fs::read(&executable).unwrap(), b"old july");
            let leftovers: Vec<_> = std::fs::read_dir(executable.parent().unwrap())
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .filter(|name| name != "july" && name != ".july-install-receipt")
                .collect();
            assert!(leftovers.is_empty(), "{leftovers:?}");
            std::fs::remove_dir_all(&root).unwrap();
        }
    }
}
