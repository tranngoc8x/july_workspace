use std::fmt;
use std::io::{self, Write};

use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers, MouseEvent,
    MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures_util::StreamExt;
use ratatui::Terminal;
use ratatui::backend::{Backend, CrosstermBackend};

#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Transcript rows moved per wheel notch.
const WHEEL_ROWS: u16 = 3;

pub mod app;
mod markdown;
pub mod ui;

use app::{App, Context};

/// Failure from the terminal operation, restoration, or both.
#[derive(Debug)]
pub enum ShellError {
    Operation(io::Error),
    Restore(io::Error),
    OperationAndRestore {
        operation: io::Error,
        restore: io::Error,
    },
}

impl fmt::Display for ShellError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Operation(error) => write!(formatter, "terminal operation failed: {error}"),
            Self::Restore(error) => write!(formatter, "terminal restoration failed: {error}"),
            Self::OperationAndRestore { operation, restore } => write!(
                formatter,
                "terminal operation failed: {operation}; terminal restoration also failed: {restore}"
            ),
        }
    }
}

impl std::error::Error for ShellError {}

trait RawMode {
    fn enable(&mut self) -> io::Result<()>;
    fn disable(&mut self) -> io::Result<()>;
}

struct CrosstermRawMode;

impl RawMode for CrosstermRawMode {
    fn enable(&mut self) -> io::Result<()> {
        enable_raw_mode()
    }

    fn disable(&mut self) -> io::Result<()> {
        disable_raw_mode()
    }
}

struct TerminalGuard<W: Write, R: RawMode> {
    writer: W,
    raw_mode: R,
    active: bool,
}

impl<W: Write, R: RawMode> TerminalGuard<W, R> {
    fn enter(writer: W, mut raw_mode: R) -> Result<Self, ShellError> {
        raw_mode.enable().map_err(ShellError::Operation)?;
        let mut guard = Self {
            writer,
            raw_mode,
            active: true,
        };
        if let Err(operation) =
            execute!(guard.writer, EnterAlternateScreen, EnableMouseCapture, Hide)
        {
            return match guard.restore() {
                Ok(()) => Err(ShellError::Operation(operation)),
                Err(restore) => Err(ShellError::OperationAndRestore { operation, restore }),
            };
        }
        Ok(guard)
    }

    fn restore(&mut self) -> io::Result<()> {
        if !self.active {
            return Ok(());
        }
        self.active = false;

        let show = execute!(self.writer, Show);
        let leave = execute!(self.writer, DisableMouseCapture, LeaveAlternateScreen);
        let raw = self.raw_mode.disable();
        let mut errors = Vec::new();
        if let Err(error) = show {
            errors.push(format!("show cursor failed: {error}"));
        }
        if let Err(error) = leave {
            errors.push(format!("leave alternate screen failed: {error}"));
        }
        if let Err(error) = raw {
            errors.push(format!("disable raw mode failed: {error}"));
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(io::Error::other(errors.join("; ")))
        }
    }
}

impl<W, R> Drop for TerminalGuard<W, R>
where
    W: Write,
    R: RawMode,
{
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

fn run_with_terminal<W, R, T>(
    writer: W,
    raw_mode: R,
    operation: impl FnOnce(&mut W) -> io::Result<T>,
) -> Result<T, ShellError>
where
    W: Write,
    R: RawMode,
{
    let mut guard = TerminalGuard::enter(writer, raw_mode)?;
    let operation = operation(&mut guard.writer);
    let restore = guard.restore();
    match (operation, restore) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(operation), Ok(())) => Err(ShellError::Operation(operation)),
        (Ok(_), Err(restore)) => Err(ShellError::Restore(restore)),
        (Err(operation), Err(restore)) => {
            Err(ShellError::OperationAndRestore { operation, restore })
        }
    }
}

/// Ratatui terminal borrowed from the stdout lifecycle bracket.
pub type TuiTerminal<'writer> = Terminal<CrosstermBackend<&'writer mut io::Stdout>>;

/// Own the raw/alternate-screen bracket while an operation uses Ratatui.
pub fn with_terminal<T, F>(operation: F) -> Result<T, ShellError>
where
    F: for<'writer> FnOnce(&mut TuiTerminal<'writer>) -> io::Result<T>,
{
    run_with_terminal(io::stdout(), CrosstermRawMode, |writer| {
        let mut terminal = Terminal::new(CrosstermBackend::new(writer))?;
        operation(&mut terminal)
    })
}

/// Run the inactive Phase 10 shell without changing CLI dispatch.
pub fn run_inactive_shell() -> Result<(), ShellError> {
    let _signals = ExitSignals::install().map_err(ShellError::Operation)?;
    with_terminal(|terminal| {
        let mut app = App::new(Context::root());
        seed_viewport(terminal, &mut app);
        draw_inactive(terminal, &app)?;

        loop {
            if ExitSignals::requested() {
                return Ok(());
            }
            if !event::poll(Duration::from_millis(50))? {
                continue;
            }
            if handle_event(terminal, &mut app, event::read()?)? {
                return Ok(());
            }
        }
    })
}

/// Run the active TUI while keeping all application I/O outside the reducer.
pub async fn run_app(
    mut dispatch: impl FnMut(app::AppCommand) -> io::Result<()>,
    mut next_application_event: impl FnMut() -> io::Result<Option<app::AppEvent>>,
) -> Result<(), ShellError> {
    let _signals = ExitSignals::install().map_err(ShellError::Operation)?;
    let mut guard = TerminalGuard::enter(io::stdout(), CrosstermRawMode)?;
    let operation = async {
        let mut terminal = Terminal::new(CrosstermBackend::new(&mut guard.writer))?;
        let mut app = App::new(Context::root());
        let mut terminal_events = event::EventStream::new();
        let mut frames = tokio::time::interval(Duration::from_millis(33));
        frames.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        seed_viewport(&mut terminal, &mut app);
        draw_inactive(&mut terminal, &app)?;
        let mut dirty = false;

        loop {
            if ExitSignals::requested() || app.exit_requested() {
                break;
            }

            tokio::select! {
                terminal_event = terminal_events.next() => match terminal_event {
                    Some(Ok(Event::Resize(width, height))) => {
                        terminal.autoresize()?;
                        dispatch_all(
                            app.reduce(app::AppEvent::Resize { width, height }),
                            &mut dispatch,
                        )?;
                        dirty = true;
                    }
                    Some(Ok(Event::Mouse(mouse))) => {
                        if let Some(event) = scroll_event(mouse) {
                            dispatch_all(app.reduce(event), &mut dispatch)?;
                            dirty = true;
                        }
                    }
                    Some(Ok(Event::Key(key))) => {
                        dispatch_all(app.reduce(app::AppEvent::Key(key)), &mut dispatch)?;
                        dirty = true;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(error)) => return Err(error),
                    None => break,
                },
                _ = frames.tick() => {
                    if app.turn_active() {
                        // Advance the working spinner while the agent is busy.
                        app.reduce(app::AppEvent::Tick);
                        dirty = true;
                    }
                    if dirty {
                        draw_inactive(&mut terminal, &app)?;
                        dirty = false;
                    }
                }
            }

            for _ in 0..app::CHAT_BATCH_LIMIT {
                let Some(event) = next_application_event()? else {
                    break;
                };
                dispatch_all(app.reduce(event), &mut dispatch)?;
                dirty = true;
                if app.exit_requested() {
                    break;
                }
            }
        }
        Ok(())
    }
    .await;
    let restore = guard.restore();
    match (operation, restore) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(operation), Ok(())) => Err(ShellError::Operation(operation)),
        (Ok(()), Err(restore)) => Err(ShellError::Restore(restore)),
        (Err(operation), Err(restore)) => {
            Err(ShellError::OperationAndRestore { operation, restore })
        }
    }
}

/// Map a wheel event onto a transcript scroll, ignoring other mouse input.
fn scroll_event(mouse: MouseEvent) -> Option<app::AppEvent> {
    let up = match mouse.kind {
        MouseEventKind::ScrollUp => true,
        MouseEventKind::ScrollDown => false,
        _ => return None,
    };
    Some(app::AppEvent::Scroll {
        up,
        rows: WHEEL_ROWS,
    })
}

fn dispatch_all(
    commands: Vec<app::AppCommand>,
    dispatch: &mut impl FnMut(app::AppCommand) -> io::Result<()>,
) -> io::Result<()> {
    for command in commands {
        dispatch(command)?;
    }
    Ok(())
}

fn seed_viewport<B: Backend>(terminal: &mut Terminal<B>, app: &mut App) {
    let area = terminal.get_frame().area();
    app.reduce(app::AppEvent::Resize {
        width: area.width,
        height: area.height,
    });
}

fn handle_event<B: Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    event: Event,
) -> Result<bool, B::Error> {
    match event {
        Event::Mouse(mouse) => {
            if let Some(scroll) = scroll_event(mouse) {
                app.reduce(scroll);
                draw_inactive(terminal, app)?;
            }
            Ok(false)
        }
        Event::Resize(width, height) => {
            terminal.autoresize()?;
            app.reduce(app::AppEvent::Resize { width, height });
            draw_inactive(terminal, app)?;
            Ok(false)
        }
        Event::Key(key)
            if key.kind.is_press()
                && (key.code == KeyCode::Esc
                    || key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL)) =>
        {
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn draw_inactive<B: Backend>(terminal: &mut Terminal<B>, app: &App) -> Result<(), B::Error> {
    terminal.draw(|frame| ui::render(frame, app)).map(|_| ())
}

#[cfg(unix)]
// ponytail: one interactive shell owns process signals; add fanout only if shells become concurrent.
static EXIT_REQUESTED: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn request_exit(_: libc::c_int) {
    EXIT_REQUESTED.store(true, Ordering::Relaxed);
}

#[cfg(unix)]
struct ExitSignals {
    old_term: libc::sigaction,
    old_hup: libc::sigaction,
}

#[cfg(unix)]
impl ExitSignals {
    fn install() -> io::Result<Self> {
        EXIT_REQUESTED.store(false, Ordering::Relaxed);
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = request_exit as *const () as usize;
        unsafe { libc::sigemptyset(&mut action.sa_mask) };

        let mut old_term = unsafe { std::mem::zeroed() };
        if unsafe { libc::sigaction(libc::SIGTERM, &action, &mut old_term) } != 0 {
            return Err(io::Error::last_os_error());
        }

        let mut old_hup = unsafe { std::mem::zeroed() };
        if unsafe { libc::sigaction(libc::SIGHUP, &action, &mut old_hup) } != 0 {
            let error = io::Error::last_os_error();
            unsafe { libc::sigaction(libc::SIGTERM, &old_term, std::ptr::null_mut()) };
            return Err(error);
        }
        Ok(Self { old_term, old_hup })
    }

    fn requested() -> bool {
        EXIT_REQUESTED.load(Ordering::Relaxed)
    }
}

#[cfg(unix)]
impl Drop for ExitSignals {
    fn drop(&mut self) {
        unsafe {
            libc::sigaction(libc::SIGTERM, &self.old_term, std::ptr::null_mut());
            libc::sigaction(libc::SIGHUP, &self.old_hup, std::ptr::null_mut());
        }
    }
}

#[cfg(not(unix))]
struct ExitSignals;

#[cfg(not(unix))]
impl ExitSignals {
    fn install() -> io::Result<Self> {
        Ok(Self)
    }

    fn requested() -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::io::{self, Write};
    use std::panic::{self, AssertUnwindSafe};
    use std::rc::Rc;

    use crossterm::event::Event;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::{RawMode, app::Context, handle_event, run_with_terminal, seed_viewport};

    #[derive(Clone)]
    struct FakeRawMode {
        calls: Rc<RefCell<Vec<&'static str>>>,
        disable_error: bool,
    }

    impl RawMode for FakeRawMode {
        fn enable(&mut self) -> io::Result<()> {
            self.calls.borrow_mut().push("enable");
            Ok(())
        }

        fn disable(&mut self) -> io::Result<()> {
            self.calls.borrow_mut().push("disable");
            if self.disable_error {
                Err(io::Error::other("restore failed"))
            } else {
                Ok(())
            }
        }
    }

    #[derive(Clone, Default)]
    struct SharedWriter(Rc<RefCell<Vec<u8>>>);

    impl Write for SharedWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[derive(Clone, Default)]
    struct FailingRestoreWriter(Rc<RefCell<Vec<u8>>>);

    impl Write for FailingRestoreWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(bytes);
            if bytes == b"\x1b[?25l" || bytes == b"\x1b[?25h" || bytes == b"\x1b[?1049l" {
                Err(io::Error::other("screen write failed"))
            } else {
                Ok(bytes.len())
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn setup(disable_error: bool) -> (SharedWriter, FakeRawMode) {
        let calls = Rc::new(RefCell::new(Vec::new()));
        (
            SharedWriter::default(),
            FakeRawMode {
                calls,
                disable_error,
            },
        )
    }

    #[test]
    fn normal_return_restores_terminal() {
        let (writer, raw_mode) = setup(false);
        let output = writer.0.clone();
        let calls = raw_mode.calls.clone();

        run_with_terminal(writer, raw_mode, |_| Ok(())).unwrap();

        let output = output.borrow();
        assert!(output.windows(8).any(|bytes| bytes == b"\x1b[?1049h"));
        assert!(output.windows(6).any(|bytes| bytes == b"\x1b[?25l"));
        assert!(output.windows(6).any(|bytes| bytes == b"\x1b[?25h"));
        assert!(output.windows(8).any(|bytes| bytes == b"\x1b[?1049l"));
        assert_eq!(*calls.borrow(), ["enable", "disable"]);
    }

    #[test]
    fn operation_and_restore_errors_are_both_reported() {
        let (writer, raw_mode) = setup(true);

        let error = run_with_terminal(writer, raw_mode, |_| {
            Err::<(), _>(io::Error::other("operation failed"))
        })
        .unwrap_err();

        let message = error.to_string();
        assert!(message.contains("operation failed"), "{message}");
        assert!(message.contains("restore failed"), "{message}");
    }

    #[test]
    fn partial_init_and_cursor_restore_failures_still_leave_screen_and_disable_raw_mode() {
        let writer = FailingRestoreWriter::default();
        let output = writer.0.clone();
        let calls = Rc::new(RefCell::new(Vec::new()));
        let raw_mode = FakeRawMode {
            calls: calls.clone(),
            disable_error: true,
        };

        let error = run_with_terminal(writer, raw_mode, |_| Ok(())).unwrap_err();

        let message = error.to_string();
        assert!(message.contains("screen write failed"), "{message}");
        assert!(
            message.contains("leave alternate screen failed"),
            "{message}"
        );
        assert!(message.contains("restore failed"), "{message}");
        let output = output.borrow();
        assert!(output.windows(6).any(|bytes| bytes == b"\x1b[?25h"));
        assert!(output.windows(8).any(|bytes| bytes == b"\x1b[?1049l"));
        assert_eq!(*calls.borrow(), ["enable", "disable"]);
    }

    #[test]
    fn unwind_restores_terminal() {
        let (writer, raw_mode) = setup(false);
        let output = writer.0.clone();
        let calls = raw_mode.calls.clone();

        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            let _ = run_with_terminal(writer, raw_mode, |_| -> io::Result<()> {
                panic!("shell panic")
            });
        }));

        assert!(result.is_err());
        let output = output.borrow();
        assert!(output.windows(6).any(|bytes| bytes == b"\x1b[?25h"));
        assert!(output.windows(8).any(|bytes| bytes == b"\x1b[?1049l"));
        assert_eq!(*calls.borrow(), ["enable", "disable"]);
    }

    #[test]
    fn resize_event_updates_the_terminal_area() {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.backend_mut().resize(120, 40);
        let mut app = super::App::new(Context::root());

        let should_exit = handle_event(&mut terminal, &mut app, Event::Resize(120, 40)).unwrap();

        assert!(!should_exit);
        assert_eq!(terminal.get_frame().area().width, 120);
        assert_eq!(terminal.get_frame().area().height, 40);
        assert_eq!(app.viewport(), super::app::Viewport::new(120, 40));
    }

    #[test]
    fn initial_viewport_reaches_app_before_first_render() {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut app = super::App::new(Context::root());

        seed_viewport(&mut terminal, &mut app);
        assert_eq!(app.viewport(), super::app::Viewport::new(80, 24));
        terminal
            .draw(|frame| super::ui::render(frame, &app))
            .unwrap();
    }
}
