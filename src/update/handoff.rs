//! Replace this process with the installed binary while retaining its update lock.
use super::{UpdateLock, lock_path};
use semver::Version;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::{
    fs::{MetadataExt, OpenOptionsExt},
    process::CommandExt,
};
use std::path::Path;

pub(crate) fn exec(executable: &Path, version: &Version, lock: &UpdateLock) -> io::Error {
    let fd = lock.raw_fd();
    let mut command = std::process::Command::new(executable);
    command.args(["--update-finalize", &version.to_string(), &fd.to_string()]);
    // SAFETY: only async-signal-safe fcntl runs here; the lock remains alive.
    unsafe {
        command.pre_exec(move || {
            if libc::fcntl(fd, libc::F_SETFD, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let error = command.exec();
    // Restore close-on-exec if replacement failed.
    unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    error
}

pub(crate) fn validate(fd: i32, staging: &Path) -> io::Result<File> {
    if fd < 3 {
        return Err(io::Error::other("missing lock descriptor"));
    }
    // Duplicate first: arbitrary CLI input never becomes an owned Rust fd.
    let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
    if duplicate == -1 {
        return Err(io::Error::last_os_error());
    }
    let inherited = unsafe { File::from_raw_fd(duplicate) };
    let candidate = File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(lock_path(staging))?;
    let actual = inherited.metadata()?;
    let expected = candidate.metadata()?;
    if !actual.is_file() || (actual.dev(), actual.ino()) != (expected.dev(), expected.ino()) {
        return Err(io::Error::other("lock identity mismatch"));
    }
    // A separate open must be blocked, while the inherited description must
    // already own (or be able to retain) the exclusive lock.
    if unsafe { libc::flock(candidate.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Err(io::Error::other("update lock was not held"));
    }
    if io::Error::last_os_error().raw_os_error() != Some(libc::EWOULDBLOCK)
        || unsafe { libc::flock(duplicate, libc::LOCK_EX | libc::LOCK_NB) } != 0
    {
        return Err(io::Error::other(
            "inherited descriptor does not own update lock",
        ));
    }
    // Only the owned duplicate remains, with CLOEXEC restored for later tools.
    unsafe { libc::close(fd) };
    Ok(inherited)
}
