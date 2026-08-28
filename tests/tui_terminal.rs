#![cfg(unix)]

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::process::CommandExt;
use std::panic::{self, AssertUnwindSafe};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use july_workspace::tui::{run_app, run_inactive_shell, with_terminal};

const CHILD_MODE: &str = "JULY_TUI_TEST_CHILD";
const ENTER_SCREEN: &[u8] = b"\x1b[?1049h";
const LEAVE_SCREEN: &[u8] = b"\x1b[?1049l";
const HIDE_CURSOR: &[u8] = b"\x1b[?25l";
const SHOW_CURSOR: &[u8] = b"\x1b[?25h";

#[test]
fn terminal_child() {
    let Ok(mode) = std::env::var(CHILD_MODE) else {
        return;
    };

    match mode.as_str() {
        "normal" => with_terminal(|terminal| {
            terminal.draw(|frame| frame.render_widget("July", frame.area()))?;
            Ok(())
        })
        .unwrap(),
        "error" => {
            let error =
                with_terminal(|_| Err::<(), _>(io::Error::other("expected failure"))).unwrap_err();
            assert!(error.to_string().contains("expected failure"));
        }
        "panic" => {
            let result = panic::catch_unwind(AssertUnwindSafe(|| {
                let _ = with_terminal(|_| -> io::Result<()> { panic!("expected panic") });
            }));
            assert!(result.is_err());
        }
        "inactive" => run_inactive_shell().unwrap(),
        "active" => tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(run_app(|_| Ok(()), || Ok(None)))
            .unwrap(),
        mode => panic!("unknown child mode: {mode}"),
    }
}

#[test]
fn pty_restores_after_normal_return() {
    assert_restored(spawn_pty("normal"), None);
}

#[test]
fn pty_restores_after_error_return() {
    assert_restored(spawn_pty("error"), None);
}

#[test]
fn pty_restores_after_unwind() {
    assert_restored(spawn_pty("panic"), None);
}

#[test]
fn pty_ctrl_c_restores_terminal() {
    let mut child = spawn_pty("inactive");
    child.wait_for(ENTER_SCREEN);
    child.master.write_all(b"\x03").unwrap();
    assert_restored(child, None);
}

#[test]
fn active_pty_ctrl_c_restores_terminal() {
    let mut child = spawn_pty("active");
    child.wait_for(ENTER_SCREEN);
    child.master.write_all(b"\x03").unwrap();
    assert_restored(child, None);
}

#[test]
fn active_pty_sigterm_restores_terminal() {
    let mut child = spawn_pty("active");
    child.wait_for(ENTER_SCREEN);
    send_signal(&child.child, libc::SIGTERM);
    assert_restored(child, None);
}

#[test]
fn active_pty_sighup_restores_terminal() {
    let mut child = spawn_pty("active");
    child.wait_for(ENTER_SCREEN);
    send_signal(&child.child, libc::SIGHUP);
    assert_restored(child, None);
}

#[test]
fn no_argument_binary_uses_tui_when_both_streams_are_terminals() {
    let database =
        std::env::temp_dir().join(format!("july-tui-dispatch-{}.db", ulid::Ulid::generate()));
    let mut command = Command::new(env!("CARGO_BIN_EXE_july"));
    command.env("JULY_WORKSPACE_DB", &database);
    let mut child = spawn_pty_command(command);
    child.wait_for(ENTER_SCREEN);
    child.master.write_all(b"\x03").unwrap();
    assert_restored(child, None);
    let _ = std::fs::remove_file(database);
}

#[test]
fn pty_sigterm_restores_terminal() {
    let mut child = spawn_pty("inactive");
    child.wait_for(ENTER_SCREEN);
    let pid = child.child.id();
    send_signal(&child.child, libc::SIGTERM);
    assert_restored(child, Some(format!("SIGTERM child {pid}")));
}

#[test]
fn pty_sighup_restores_terminal() {
    let mut child = spawn_pty("inactive");
    child.wait_for(ENTER_SCREEN);
    let pid = child.child.id();
    send_signal(&child.child, libc::SIGHUP);
    assert_restored(child, Some(format!("SIGHUP child {pid}")));
}

#[test]
fn dropping_pty_child_kills_and_reaps_it() {
    let pid = {
        let mut child = spawn_pty("inactive");
        child.wait_for(ENTER_SCREEN);
        child.child.id() as libc::pid_t
    };

    let mut status = 0;
    let waited = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
    if waited == 0 {
        unsafe {
            libc::kill(pid, libc::SIGKILL);
            libc::waitpid(pid, &mut status, 0);
        }
    }
    assert_eq!(waited, -1, "dropped PTY child was not reaped");
    assert_eq!(
        io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
}

struct PtyChild {
    child: Child,
    master: File,
    initial_termios: libc::termios,
    output: Vec<u8>,
    reaped: bool,
}

impl PtyChild {
    fn wait_for(&mut self, needle: &[u8]) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !contains(&self.output, needle) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {needle:?}"
            );
            self.read_once();
            if let Some(status) = self.child.try_wait().unwrap() {
                panic!(
                    "child exited early with {status}: {:?}",
                    String::from_utf8_lossy(&self.output)
                );
            }
        }
    }

    fn read_once(&mut self) {
        self.try_read_once().unwrap();
    }

    fn try_read_once(&mut self) -> io::Result<()> {
        let mut descriptor = libc::pollfd {
            fd: self.master.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut descriptor, 1, 25) };
        if ready < 0 {
            return Err(io::Error::last_os_error());
        }
        if ready == 0 {
            return Ok(());
        }

        let mut bytes = [0; 4096];
        match self.master.read(&mut bytes) {
            Ok(0) => Ok(()),
            Ok(read) => {
                self.output.extend_from_slice(&bytes[..read]);
                Ok(())
            }
            Err(error) if error.raw_os_error() == Some(libc::EIO) => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn finish(mut self) -> (std::process::ExitStatus, Vec<u8>, libc::termios) {
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            self.read_once();
            if let Some(status) = self.child.try_wait().unwrap() {
                self.reaped = true;
                break status;
            }
            assert!(Instant::now() < deadline, "child did not exit");
            thread::sleep(Duration::from_millis(10));
        };
        for _ in 0..4 {
            self.read_once();
        }

        let mut termios = unsafe { std::mem::zeroed() };
        let result = unsafe { libc::tcgetattr(self.master.as_raw_fd(), &mut termios) };
        assert_eq!(
            result,
            0,
            "tcgetattr failed: {}",
            io::Error::last_os_error()
        );
        (status, std::mem::take(&mut self.output), termios)
    }
}

impl Drop for PtyChild {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        unsafe {
            libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            let _ = self.try_read_once();
            match self.child.try_wait() {
                Ok(Some(_)) => {
                    self.reaped = true;
                    return;
                }
                Ok(None) => {}
                Err(_) => return,
            }
        }

        let _ = self.child.kill();
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            let _ = self.try_read_once();
            match self.child.try_wait() {
                Ok(Some(_)) => {
                    self.reaped = true;
                    return;
                }
                Ok(None) => {}
                Err(_) => return,
            }
        }
    }
}

fn spawn_pty(mode: &str) -> PtyChild {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .arg("--exact")
        .arg("terminal_child")
        .arg("--nocapture")
        .env(CHILD_MODE, mode);
    spawn_pty_command(command)
}

fn spawn_pty_command(mut command: Command) -> PtyChild {
    let mut master = -1;
    let mut slave = -1;
    let mut size = libc::winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let result = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        )
    };
    assert_eq!(result, 0, "openpty failed: {}", io::Error::last_os_error());

    let master = unsafe { File::from_raw_fd(master) };
    let slave = unsafe { File::from_raw_fd(slave) };
    let mut initial_termios = unsafe { std::mem::zeroed() };
    let result = unsafe { libc::tcgetattr(master.as_raw_fd(), &mut initial_termios) };
    assert_eq!(
        result,
        0,
        "tcgetattr failed: {}",
        io::Error::last_os_error()
    );

    command
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave));
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            if libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY.into(), 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn().unwrap();

    PtyChild {
        child,
        master,
        initial_termios,
        output: Vec::new(),
        reaped: false,
    }
}

fn send_signal(child: &Child, signal: libc::c_int) {
    let result = unsafe { libc::kill(child.id() as libc::pid_t, signal) };
    assert_eq!(result, 0, "kill failed: {}", io::Error::last_os_error());
}

fn assert_restored(child: PtyChild, context: Option<String>) {
    let initial = child.initial_termios;
    let (status, output, restored) = child.finish();
    let context = context.unwrap_or_else(|| "PTY child".into());

    assert!(
        status.success(),
        "{context} exited with {status}: {:?}",
        String::from_utf8_lossy(&output)
    );
    assert!(
        contains(&output, ENTER_SCREEN),
        "{context} never entered alternate screen"
    );
    assert!(contains(&output, HIDE_CURSOR), "{context} never hid cursor");
    assert!(
        contains(&output, SHOW_CURSOR),
        "{context} never restored cursor"
    );
    assert!(
        contains(&output, LEAVE_SCREEN),
        "{context} never left alternate screen"
    );
    assert_eq!(restored.c_iflag, initial.c_iflag, "{context} input flags");
    assert_eq!(restored.c_oflag, initial.c_oflag, "{context} output flags");
    assert_eq!(restored.c_cflag, initial.c_cflag, "{context} control flags");
    assert_eq!(restored.c_lflag, initial.c_lflag, "{context} local flags");
    assert_eq!(restored.c_cc, initial.c_cc, "{context} control characters");
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}
