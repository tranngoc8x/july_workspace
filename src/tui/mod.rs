use std::fmt;
use std::io::{self, Write};

use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyModifiers,
};
#[cfg(not(windows))]
use crossterm::event::{
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::style::Print;
use crossterm::terminal::{Clear, ClearType, disable_raw_mode, enable_raw_mode, size};
use futures_util::StreamExt;
use ratatui::backend::{Backend, ClearType as BufferClear, CrosstermBackend};
use ratatui::layout::{Position, Rect, Size};
use ratatui::{Terminal, TerminalOptions, Viewport, style::Color};

#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub mod app;
mod markdown;
/// Writing finished transcript rows into the terminal's scrollback.
mod scrollback;
/// The composer and everything that can take the bottom of the screen from it.
pub(crate) mod bottom_pane;
/// Finding workspace files for the composer's `@` popup.
pub mod file_search;
/// Rendering, wrapping, and key-binding primitives the composer is built on.
pub(crate) mod support;
pub mod ui;
/// Byte-range markers the composer overlays on its text buffer.
pub mod user_input;

use app::{App, Context};

pub(crate) const AGENT_COLOR: Color = Color::Rgb(208, 215, 222);
pub(crate) const USER_COLOR: Color = Color::Rgb(121, 192, 255);
pub(crate) const COMMAND_OUTPUT_COLOR: Color = Color::Rgb(126, 231, 135);
pub(crate) const SYSTEM_COLOR: Color = Color::Rgb(227, 179, 65);
pub(crate) const ERROR_COLOR: Color = Color::Rgb(255, 123, 114);
pub(crate) const CODE_COLOR: Color = Color::Rgb(13, 205, 205);

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
    #[cfg(not(windows))]
    keyboard_enhanced: bool,
}

impl<W: Write, R: RawMode> TerminalGuard<W, R> {
    fn enter(writer: W, mut raw_mode: R) -> Result<Self, ShellError> {
        raw_mode.enable().map_err(ShellError::Operation)?;
        let mut guard = Self {
            writer,
            raw_mode,
            active: true,
            #[cfg(not(windows))]
            keyboard_enhanced: false,
        };
        // No alternate screen and no mouse capture: the transcript lives in the terminal's own
        // scrollback, so scrolling and selecting text stay the terminal's job. Capture would eat
        // drag-select and force the user to hold Shift.
        if let Err(operation) = execute!(
            guard.writer,
            MoveTo(0, 0),
            Clear(ClearType::Purge),
            Clear(ClearType::All),
            // Without this a paste arrives as individual key events, and the composer has to guess
            // from timing which of them were typed.
            EnableBracketedPaste,
            Hide
        ) {
            return match guard.restore() {
                Ok(()) => Err(ShellError::Operation(operation)),
                Err(restore) => Err(ShellError::OperationAndRestore { operation, restore }),
            };
        }
        #[cfg(not(windows))]
        {
            guard.keyboard_enhanced = true;
            if let Err(operation) = execute!(
                guard.writer,
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
            ) {
                return match guard.restore() {
                    Ok(()) => Err(ShellError::Operation(operation)),
                    Err(restore) => Err(ShellError::OperationAndRestore { operation, restore }),
                };
            }
        }
        Ok(guard)
    }

    fn restore(&mut self) -> io::Result<()> {
        if !self.active {
            return Ok(());
        }
        self.active = false;

        #[cfg(not(windows))]
        let keyboard = if self.keyboard_enhanced {
            self.keyboard_enhanced = false;
            execute!(self.writer, PopKeyboardEnhancementFlags)
        } else {
            Ok(())
        };
        let show = execute!(self.writer, Show);
        // The viewport sits on the last rows, so the shell prompt needs a line of its own below
        // whatever july left on screen.
        let leave = execute!(
            self.writer,
            MoveTo(0, size().map(|(_, rows)| rows.saturating_sub(1)).unwrap_or(0)),
            Print("\r\n"),
            DisableBracketedPaste
        );
        let raw = self.raw_mode.disable();
        let mut errors = Vec::new();
        #[cfg(not(windows))]
        if let Err(error) = keyboard {
            errors.push(format!("restore keyboard input failed: {error}"));
        }
        if let Err(error) = show {
            errors.push(format!("show cursor failed: {error}"));
        }
        if let Err(error) = leave {
            errors.push(format!("release terminal failed: {error}"));
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
        let mut terminal = open_terminal(writer, &App::new(Context::root()))?;
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
    initial_context: Context,
    agents: Vec<String>,
    mut dispatch: impl FnMut(app::AppCommand) -> io::Result<()>,
    mut next_application_event: impl FnMut() -> io::Result<Option<app::AppEvent>>,
) -> Result<(), ShellError> {
    let _signals = ExitSignals::install().map_err(ShellError::Operation)?;
    let mut guard = TerminalGuard::enter(io::stdout(), CrosstermRawMode)?;
    let operation = async {
        let mut app = App::new(initial_context);
        app.reduce(app::AppEvent::Agents(agents));
        let mut terminal = open_terminal(&mut guard.writer, &app)?;
        let mut terminal_events = event::EventStream::new();
        let mut frames = tokio::time::interval(Duration::from_millis(33));
        frames.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        seed_viewport(&mut terminal, &mut app);
        draw(&mut terminal, &mut app)?;
        let mut dirty = false;

        loop {
            if ExitSignals::requested() || app.exit_requested() {
                break;
            }

            tokio::select! {
                terminal_event = terminal_events.next() => match terminal_event {
                    Some(Ok(Event::Resize(width, height))) => {
                        dispatch_all(
                            app.reduce(app::AppEvent::Resize { width, height }),
                            &mut dispatch,
                        )?;
                        dirty = true;
                    }
                    Some(Ok(Event::Key(key))) => {
                        dispatch_all(app.reduce(app::AppEvent::Key(key)), &mut dispatch)?;
                        dirty = true;
                    }
                    Some(Ok(Event::Paste(pasted))) => {
                        dispatch_all(app.reduce(app::AppEvent::Paste(pasted)), &mut dispatch)?;
                        dirty = true;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(error)) => return Err(error),
                    None => break,
                },
                _ = frames.tick() => {
                    // Every frame, not only while a turn runs: the composer rides this tick to
                    // sync popups and release keystrokes it was holding as a suspected paste.
                    dispatch_all(app.reduce(app::AppEvent::Tick), &mut dispatch)?;
                    if app.turn_active() || app.take_pane_redraw() {
                        dirty = true;
                    }
                    if dirty {
                        draw(&mut terminal, &mut app)?;
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
    // The app reasons about the whole screen; the viewport is only the band it draws into.
    if let Ok(screen) = terminal.size() {
        app.reduce(app::AppEvent::Resize {
            width: screen.width,
            height: screen.height,
        });
    }
}

fn handle_event<B: Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    event: Event,
) -> Result<bool, B::Error> {
    match event {
        Event::Resize(width, height) => {
            app.reduce(app::AppEvent::Resize { width, height });
            draw_inactive(terminal, app)?;
            Ok(false)
        }
        Event::Key(key)
            if key.kind.is_press()
                && key.code == KeyCode::Char('c')
                && key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// The rows july owns: header, whatever is still being streamed, and the composer, pinned to the
/// bottom of the screen. Everything above the band belongs to the terminal's scrollback.
///
/// The live region is capped at half the screen so a long unfinished block cannot push the
/// scrollback off; the band then shows its tail, which is where the new text is.
fn viewport_band(screen: Size, app: &App) -> Rect {
    let live = scrollback::rendered_rows(app.transcript_text(), screen.width)
        .min(screen.height.saturating_sub(app.input_height().saturating_add(1)) / 2);
    let height = live
        .saturating_add(app.input_height())
        .saturating_add(1)
        .clamp(1, screen.height.max(1));
    Rect::new(
        0,
        screen.height.saturating_sub(height),
        screen.width,
        height,
    )
}

fn open_terminal<W: Write>(writer: W, app: &App) -> io::Result<Terminal<CrosstermBackend<W>>> {
    let backend = CrosstermBackend::new(writer);
    let band = viewport_band(backend.size()?, app);
    Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Fixed(band),
        },
    )
}

/// Keeps the viewport on the bottom rows as the composer grows and the window resizes.
///
/// A fixed viewport is never autoresized (that is what keeps ratatui from repainting over
/// scrollback), so the band is recomputed here before every frame.
fn sync_viewport<B: Backend>(terminal: &mut Terminal<B>, app: &App) -> Result<(), B::Error> {
    let band = viewport_band(terminal.size()?, app);
    let current = terminal.get_frame().area();
    if current == band {
        return Ok(());
    }
    // A shrinking band leaves its old top rows behind, and resizing only clears the new rect, so
    // the rows the band is giving up are erased here.
    let backend = terminal.backend_mut();
    backend.set_cursor_position(Position::new(0, current.y.min(band.y)))?;
    backend.clear_region(BufferClear::AfterCursor)?;
    terminal.resize(band)
}

fn draw_inactive<B: Backend>(terminal: &mut Terminal<B>, app: &App) -> Result<(), B::Error> {
    sync_viewport(terminal, app)?;
    terminal.draw(|frame| ui::render(frame, app)).map(|_| ())
}

/// Moves everything july has finished into the terminal's scrollback, then repaints the band.
///
/// Writing above the band scrolls the screen, so the band has to be repainted afterwards - which
/// this does by ending in the ordinary draw.
fn draw<W: Write>(
    terminal: &mut Terminal<CrosstermBackend<W>>,
    app: &mut App,
) -> io::Result<()> {
    if app.take_scope_changed() {
        // Everything on screen and in scrollback belongs to the scope being left. The new scope's
        // history is already rebuilt in the app, so the loop below writes it into a clean terminal.
        scrollback::purge(terminal.backend_mut())?;
        // Unconditionally, because the screen is now blank while ratatui still believes it painted
        // the band: resizing a fixed viewport is what forces the next draw to repaint in full.
        let band = viewport_band(terminal.size()?, app);
        terminal.resize(band)?;
    }
    let band = terminal.get_frame().area();
    while let Some(block) = app.take_finished_block() {
        scrollback::write_above(terminal.backend_mut(), band, block)?;
        // The rows above the band moved up, so nothing ratatui remembers about the band is true
        // any more. Resizing to the same rect is how a fixed viewport is told to repaint in full.
        terminal.resize(band)?;
    }
    draw_inactive(terminal, app)
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

    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use ratatui::layout::Size;

    use crate::application::ChatEvent;

    use super::app::{App, Context};
    use super::{RawMode, app, handle_event, run_with_terminal, seed_viewport, viewport_band};

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
            // Show/hide cursor and the teardown's bracketed-paste reset, which is the last
            // thing july writes on the way out.
            if bytes == b"\x1b[?25l" || bytes == b"\x1b[?25h" || bytes == b"\x1b[?2004l" {
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
        // July owns the bottom rows of the ordinary screen, never the alternate one: that is what
        // leaves scrollback and mouse selection to the terminal.
        assert!(!output.windows(8).any(|bytes| bytes == b"\x1b[?1049h"));
        assert!(output.windows(4).any(|bytes| bytes == b"\x1b[3J"));
        #[cfg(not(windows))]
        assert!(output.windows(5).any(|bytes| bytes == b"\x1b[>1u"));
        assert!(output.windows(6).any(|bytes| bytes == b"\x1b[?25l"));
        #[cfg(not(windows))]
        assert!(output.windows(5).any(|bytes| bytes == b"\x1b[<1u"));
        assert!(output.windows(6).any(|bytes| bytes == b"\x1b[?25h"));
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
        assert!(message.contains("release terminal failed"), "{message}");
        assert!(message.contains("restore failed"), "{message}");
        let output = output.borrow();
        assert!(output.windows(6).any(|bytes| bytes == b"\x1b[?25h"));
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
        #[cfg(not(windows))]
        assert!(output.windows(5).any(|bytes| bytes == b"\x1b[<1u"));
        assert!(output.windows(6).any(|bytes| bytes == b"\x1b[?25h"));
        assert_eq!(*calls.borrow(), ["enable", "disable"]);
    }

    #[test]
    fn the_band_sits_on_the_bottom_rows_and_never_takes_the_whole_screen() {
        let screen = Size::new(80, 24);
        let mut app = App::new(Context::root());
        app.reduce(app::AppEvent::Resize {
            width: screen.width,
            height: screen.height,
        });

        let idle = viewport_band(screen, &app);
        assert_eq!(idle.x, 0);
        assert_eq!(idle.width, screen.width);
        assert_eq!(idle.bottom(), screen.height, "the band is pinned to the bottom");
        assert!(idle.height < screen.height, "scrollback keeps the rows above");

        // A block still being streamed grows the band, because it has nowhere else to be drawn.
        app.reduce(app::AppEvent::Chat(ChatEvent::TextDelta(
            "still typing".repeat(40),
        )));
        let streaming = viewport_band(screen, &app);
        assert!(streaming.height > idle.height, "{streaming:?} vs {idle:?}");
        assert_eq!(streaming.bottom(), screen.height);
        assert!(
            streaming.height < screen.height,
            "a long unfinished block must not swallow the scrollback: {streaming:?}"
        );
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
    fn escape_does_not_exit_the_inactive_shell() {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut app = super::App::new(Context::root());

        let should_exit = handle_event(
            &mut terminal,
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        )
        .unwrap();

        assert!(!should_exit);
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
