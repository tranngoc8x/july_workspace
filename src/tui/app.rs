use crate::runtime::AgentActivity;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fmt;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::style::Style;
use ratatui::text::{Line, Span, Text};

use super::bottom_pane::events::{NoticeLevel, PaneEvent};
use super::bottom_pane::slash_commands::commands_from_names;
use super::bottom_pane::{BottomPane, BottomPaneParams, CancellationEvent, InputResult};
use super::followups::{Followups, prepare as prepare_followup};
use super::markdown::{MarkdownStream, render as render_markdown};
use super::support::render::renderable::Renderable;
use super::{AGENT_COLOR, CODE_COLOR, COMMAND_OUTPUT_COLOR, ERROR_COLOR, SYSTEM_COLOR, USER_COLOR};
use crate::application::{ChatEvent, ChatFailureKind, ChatPermissionRequestId};
use crate::domain::{AgentId, PermissionOutcome, RoomMessageId};

pub const CHAT_BATCH_LIMIT: usize = 32;
/// Rows the composer may not take: the header, and one row of transcript.
const NON_INPUT_ROWS: u16 = 2;

/// Braille frames for the "agent is working" indicator.
const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
/// Frame ticks held per spinner step so the animation stays readable at 30fps.
const SPINNER_TICKS_PER_FRAME: usize = 3;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ContextId(String);

impl ContextId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn root() -> Self {
        Self::new("root")
    }
}

impl fmt::Display for ContextId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Context {
    id: ContextId,
    label: String,
    commands: Vec<String>,
}

impl Context {
    pub fn new(id: ContextId, label: impl Into<String>) -> Self {
        Self {
            id,
            label: label.into(),
            commands: Vec::new(),
        }
    }

    pub fn root() -> Self {
        Self::new(ContextId::root(), "july")
    }

    pub fn id(&self) -> &ContextId {
        &self.id
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn with_commands(mut self, commands: Vec<String>) -> Self {
        self.commands = commands;
        self
    }

    pub fn commands(&self) -> &[String] {
        &self.commands
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Viewport {
    pub width: u16,
    pub height: u16,
}

impl Viewport {
    pub const fn new(width: u16, height: u16) -> Self {
        Self { width, height }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AppCommand {
    Submit {
        context: ContextId,
        text: String,
    },
    Execute {
        context: ContextId,
        input: String,
    },
    RespondPermission {
        request_id: ChatPermissionRequestId,
        outcome: PermissionOutcome,
    },
    CancelTurn,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandResult {
    Submitted,
    SubmittedWithContext(ContextSnapshot),
    Context(Context),
    ContextWithHistory(ContextSnapshot),
    Output {
        context: Context,
        output: String,
    },
    Failed(String),
    FailedInContext {
        error: String,
        snapshot: ContextSnapshot,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HistoryAuthor {
    System,
    User,
    Agent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryEntry {
    pub author: HistoryAuthor,
    pub body: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct History {
    pub entries: Vec<HistoryEntry>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextSnapshot {
    pub context: Context,
    pub history: Result<History, String>,
    pub history_fallback: Option<HistoryEntry>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AppEvent {
    Key(KeyEvent),
    /// A bracketed paste delivered whole by the terminal.
    Paste(String),
    Resize {
        width: u16,
        height: u16,
    },
    Tick,
    /// Agent names for `@` completion, sent once when the session opens.
    Agents(Vec<String>),
    /// July Room activation status, never a private runtime transcript.
    RoomStatus(String),
    /// An agent in the current Room began producing output; opens its live cell.
    AgentStreamStarted {
        agent: AgentId,
        /// Agent name, used as the cell header.
        label: String,
    },
    /// More output for one agent. A delta for an agent with no open cell is dropped, so one agent
    /// can never write into another's.
    AgentStreamDelta {
        agent: AgentId,
        delta: String,
    },
    AgentActivity {
        agent: AgentId,
        activity: AgentActivity,
    },
    /// An agent finished; its cell is committed to the transcript and closed.
    AgentStreamFinished {
        agent: AgentId,
    },
    /// An agent failed or was cancelled; whatever streamed is kept and the reason is appended.
    AgentStreamFailed {
        agent: AgentId,
        reason: String,
    },
    /// An explicitly published canonical shared Room message.
    RoomMessage(String),
    Chat(ChatEvent),
    ChatBatch(Vec<ChatEvent>),
    CommandFinished {
        context: ContextId,
        result: CommandResult,
    },
    PermissionFinished(Result<(), String>),
    RoomPermissionDismissed(ChatPermissionRequestId),
    CancelFinished(Result<(), String>),
    Exit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TurnState {
    Idle,
    Active,
    Cancelling,
    CancelAcknowledged,
}

/// What the composer shows before anything is typed.
const COMPOSER_PLACEHOLDER: &str = "Ask anything, / for commands, @ for agents and files";

/// How many file-search results the `@` popup is offered.
const FILE_SEARCH_LIMIT: usize = 24;

/// The part of the composer worth carrying across a scope switch.
///
/// Only the visible draft: attachments and mention bindings belong to the submission being written,
/// and July's mentions round-trip as plain `@name` text.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ComposerDraft {
    pub text: String,
    /// Byte offset of the caret within `text`.
    pub cursor: usize,
}

/// One conversation scope's transient view state.
///
/// Kept in memory only. Nothing here belongs in SQLite: it is what the user had half-typed, which
/// is meaningless once the process exits. Scroll position is the terminal's business now.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoomUiState {
    pub draft: ComposerDraft,
    /// ponytail: always `None`. July's transcript has no per-message selection or reply UI yet, so
    /// there is nothing to capture; the fields are here so that when those land, the save/restore
    /// path already carries them.
    pub reply_to: Option<RoomMessageId>,
    pub selected_message: Option<RoomMessageId>,
}

impl Default for RoomUiState {
    /// A scope opened for the first time starts with an empty draft.
    fn default() -> Self {
        Self {
            draft: ComposerDraft::default(),
            reply_to: None,
            selected_message: None,
        }
    }
}

/// One agent's in-progress output, shown under the committed transcript until the agent finishes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LiveCell {
    /// Agent name, as the header above the streaming body.
    pub label: String,
    /// What has streamed so far.
    pub body: String,
    pub activity: AgentActivity,
    pub last_update: Instant,
}

impl LiveCell {
    fn activity_label(&self, now: Instant) -> String {
        let label = match self.activity {
            AgentActivity::Working => "working",
            AgentActivity::ToolRunning => "tool running",
            AgentActivity::Writing => "writing",
            AgentActivity::WaitingPermission => "waiting permission",
        };
        let seconds = now.saturating_duration_since(self.last_update).as_secs();
        if seconds >= 60 {
            format!("{label} · no update for {seconds}s")
        } else {
            format!("{label} · {seconds}s")
        }
    }
}

pub struct App {
    context: Context,
    /// The composer, and whatever modal view is covering it.
    bottom_pane: BottomPane,
    viewport: Viewport,
    markdown: MarkdownStream,
    pending: Option<ContextId>,
    followups: HashMap<ContextId, Followups>,
    active_recipients: Option<String>,
    turn: TurnState,
    /// The permission request whose prompt is open in the pane, so a resolved request can close it.
    pending_permission: Option<ChatPermissionRequestId>,
    error: Option<String>,
    /// Per-scope view state, so leaving a room and coming back lands where the user left off.
    room_ui: HashMap<ContextId, RoomUiState>,
    /// Agents streaming right now, one cell each. Ordered by agent id so the list does not reshuffle
    /// between frames.
    live_cells: BTreeMap<AgentId, LiveCell>,
    /// Set when the bottom pane changed outside a key press, so the frame loop redraws.
    pane_redraw: bool,
    scope_changed: bool,
    tick: usize,
    exit_requested: bool,
}

impl App {
    pub fn new(context: Context) -> Self {
        let mut bottom_pane = BottomPane::new(BottomPaneParams {
            has_input_focus: true,
            // July's terminal guard asks for key disambiguation, so modified Enter is reported.
            enhanced_keys_supported: cfg!(not(windows)),
            placeholder_text: COMPOSER_PLACEHOLDER.to_string(),
            // The terminal guard turns on bracketed paste, so a paste arrives whole as
            // `AppEvent::Paste` and the composer never has to guess a paste from keystroke timing.
            disable_paste_burst: true,
        });
        bottom_pane.set_slash_commands(commands_from_names(context.commands()));
        // The v2 `@` popup is the one that offers agents alongside files; without it `@` searches
        // files only.
        bottom_pane.set_mentions_v2_enabled(true);
        Self {
            context,
            bottom_pane,
            viewport: Viewport::new(0, 0),
            markdown: MarkdownStream::default(),
            pending: None,
            followups: HashMap::new(),
            active_recipients: None,
            turn: TurnState::Idle,
            pending_permission: None,
            error: None,
            room_ui: HashMap::new(),
            live_cells: BTreeMap::new(),
            pane_redraw: false,
            scope_changed: false,
            tick: 0,
            exit_requested: false,
        }
    }

    pub fn context(&self) -> &Context {
        &self.context
    }

    pub fn input(&self) -> String {
        self.bottom_pane.composer_text()
    }

    pub(crate) fn bottom_pane(&self) -> &BottomPane {
        &self.bottom_pane
    }

    /// Rows the bottom pane gets, capped so the transcript keeps at least one row.
    pub(crate) fn input_height(&self) -> u16 {
        self.bottom_pane
            .desired_height(self.viewport.width.max(1))
            .max(1)
            .min(self.viewport.height.saturating_sub(NON_INPUT_ROWS).max(1))
    }

    /// Switches to `context`, saving the outgoing scope's view state and returning the incoming
    /// scope's.
    ///
    /// The caller applies the returned state, because a switch that also reloads the transcript has
    /// to rebuild it first.
    #[must_use]
    fn set_context(&mut self, context: Context) -> Option<RoomUiState> {
        self.bottom_pane
            .set_slash_commands(commands_from_names(context.commands()));
        // Results that refresh the same scope - a new label, a changed command list - must still
        // land, but they are not a switch: the view on screen is already the right one.
        if context.id == self.context.id {
            self.context = context;
            return None;
        }
        // A real switch, so the previous scope's rows stop being what the user is looking at.
        self.hold_followups();
        self.active_recipients = None;
        self.scope_changed = true;
        let outgoing = self.capture_room_ui();
        self.room_ui.insert(self.context.id.clone(), outgoing);
        let incoming = self.room_ui.get(&context.id).cloned().unwrap_or_default();
        self.context = context;
        Some(incoming)
    }

    /// The current scope's view state.
    ///
    /// The draft comes from what was last remembered rather than from the composer, because
    /// switching scope is itself a typed command: by the time the switch lands the composer holds
    /// that command, or nothing.
    fn capture_room_ui(&self) -> RoomUiState {
        RoomUiState {
            draft: self
                .room_ui
                .get(&self.context.id)
                .map(|state| state.draft.clone())
                .unwrap_or_default(),
            reply_to: None,
            selected_message: None,
        }
    }

    /// Remembers the draft the user is writing in this scope.
    ///
    /// A slash command is not a draft - it is how the user leaves - so typing one must not erase
    /// the half-written message they want to come back to.
    fn remember_draft(&mut self) {
        let draft = self.bottom_pane.composer_draft();
        if draft.text.trim_start().starts_with('/') {
            return;
        }
        self.room_ui
            .entry(self.context.id.clone())
            .or_default()
            .draft = draft;
    }

    /// Puts the user back where they were in this scope.
    fn restore_room_ui(&mut self, state: RoomUiState) {
        let RoomUiState {
            draft,
            reply_to: _,
            selected_message: _,
        } = state;
        self.bottom_pane.restore_composer_draft(draft);
    }

    pub fn viewport(&self) -> Viewport {
        self.viewport
    }

    pub fn stream(&self) -> &str {
        self.markdown.tail()
    }

    /// The transcript as plain text, for tests and diagnostics.
    pub fn transcript(&self) -> String {
        self.markdown
            .text()
            .lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub(crate) fn transcript_text(&self) -> Text<'static> {
        let text = self.markdown.text();
        let mut lines = spaced_rows(text.lines);
        let live = self.live_cell_lines();
        if !live.is_empty() {
            // One blank row separates the live block from the committed transcript; inside the
            // block the header, its body and its cursor stay together.
            if lines.last().is_some_and(|last| last.width() > 0) {
                lines.push(Line::default());
            }
            lines.extend(live);
        }
        if let Some(frame) = self.spinner_frame() {
            let label = match self.turn {
                TurnState::Cancelling => "cancelling",
                TurnState::CancelAcknowledged => "cancelled",
                _ => "working",
            };
            if lines.last().is_some_and(|line| line.width() > 0) {
                lines.push(Line::default());
            }
            lines.push(Line::from(Span::styled(
                format!("{frame} {label}…"),
                Style::default().fg(SYSTEM_COLOR),
            )));
        }
        Text {
            alignment: text.alignment,
            style: text.style,
            lines,
        }
    }

    pub fn turn_active(&self) -> bool {
        self.turn != TurnState::Idle
    }

    pub fn turn_state(&self) -> TurnState {
        self.turn
    }

    /// The permission request currently being asked about, if any.
    pub fn permission(&self) -> Option<&ChatPermissionRequestId> {
        self.pending_permission.as_ref()
    }

    /// Last command failure or interruption, cleared on the next submit.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Current spinner frame while a turn is in flight.
    pub fn spinner_frame(&self) -> Option<&'static str> {
        (self.turn != TurnState::Idle)
            .then(|| SPINNER_FRAMES[(self.tick / SPINNER_TICKS_PER_FRAME) % SPINNER_FRAMES.len()])
    }

    pub fn exit_requested(&self) -> bool {
        self.exit_requested
    }

    /// Puts the current error on the composer's footer row, and hands the row back to the composer
    /// once the error clears so its own hints show through.
    fn sync_footer_hint(&mut self) {
        let queue = self
            .followups
            .get(&self.context.id)
            .filter(|queue| queue.len() > 0);
        let queue_hint = queue.map(|queue| {
            if queue.held {
                format!(
                    "{} queued (held): /queue resume or /queue discard",
                    queue.len()
                )
            } else {
                format!(
                    "{} queued · Tab: supplement · Enter: new request",
                    queue.len()
                )
            }
        });
        let hint = match (self.error(), queue_hint) {
            (Some(error), Some(queue)) => Some(format!("{error} · {queue}")),
            (Some(error), None) => Some(error.to_owned()),
            (None, queue) => queue,
        };
        self.bottom_pane
            .set_footer_hint(hint.map(|hint| vec![("!".into(), hint)]));
    }

    /// Whether the bottom pane changed since the last draw, and clears the flag.
    pub fn take_pane_redraw(&mut self) -> bool {
        std::mem::take(&mut self.pane_redraw)
    }

    /// Whether the scope changed since this was last asked.
    ///
    /// A room shows that room and nothing else, so the render layer answers this by erasing the
    /// terminal's scrollback before writing the new scope's history into it.
    pub fn take_scope_changed(&mut self) -> bool {
        std::mem::take(&mut self.scope_changed)
    }

    pub fn reduce(&mut self, event: AppEvent) -> Vec<AppCommand> {
        let mut commands = self.reduce_inner(event);
        commands.extend(self.release_followup());
        // One place to keep the composer's footer in step with the app, rather than a call beside
        // each of the dozen assignments to `turn` and `error`.
        self.bottom_pane.set_task_running(self.turn_active());
        self.sync_footer_hint();
        commands
    }

    fn reduce_inner(&mut self, event: AppEvent) -> Vec<AppCommand> {
        match event {
            AppEvent::Key(key) => self.reduce_key(key),
            AppEvent::Resize { width, height } => {
                self.viewport = Viewport::new(width, height);
                Vec::new()
            }
            AppEvent::Agents(agents) => {
                self.bottom_pane.set_agents(agents);
                Vec::new()
            }
            AppEvent::RoomMessage(body) => {
                self.freeze_stream();
                self.push_history_entry(&HistoryEntry {
                    author: HistoryAuthor::Agent,
                    body,
                });
                Vec::new()
            }
            AppEvent::AgentStreamStarted { agent, label } => {
                self.live_cells.insert(
                    agent,
                    LiveCell {
                        label,
                        body: String::new(),
                        activity: AgentActivity::Working,
                        last_update: Instant::now(),
                    },
                );
                Vec::new()
            }
            AppEvent::AgentStreamDelta { agent, delta } => {
                if let Some(cell) = self.live_cells.get_mut(&agent) {
                    cell.body.push_str(&delta);
                    cell.last_update = Instant::now();
                    if matches!(
                        cell.activity,
                        AgentActivity::Working | AgentActivity::Writing
                    ) {
                        cell.activity = AgentActivity::Writing;
                    }
                }
                Vec::new()
            }
            AppEvent::AgentActivity { agent, activity } => {
                if let Some(cell) = self.live_cells.get_mut(&agent) {
                    cell.activity = activity;
                    cell.last_update = Instant::now();
                }
                Vec::new()
            }
            AppEvent::AgentStreamFinished { agent } => {
                self.commit_live_cell(&agent, None);
                Vec::new()
            }
            AppEvent::AgentStreamFailed { agent, reason } => {
                self.hold_followups();
                self.commit_live_cell(&agent, Some(reason));
                Vec::new()
            }
            AppEvent::RoomStatus(status) => {
                self.freeze_stream();
                self.markdown.push_plain(status, COMMAND_OUTPUT_COLOR);
                Vec::new()
            }
            AppEvent::Chat(event) => self.reduce_chat_content(std::iter::once(event)),
            AppEvent::ChatBatch(events) => self.reduce_chat_content(events),
            AppEvent::CommandFinished { context, result } => {
                self.reduce_command_result(context, result);
                Vec::new()
            }
            AppEvent::Exit => {
                self.exit_requested = true;
                Vec::new()
            }
            AppEvent::CancelFinished(result) => {
                if self.turn != TurnState::Cancelling {
                    return Vec::new();
                }
                match result {
                    Ok(()) => self.turn = TurnState::CancelAcknowledged,
                    Err(error) => {
                        self.turn = TurnState::Active;
                        self.error = Some(error);
                    }
                }
                Vec::new()
            }
            AppEvent::RoomPermissionDismissed(request_id) => {
                if self.pending_permission.as_ref() == Some(&request_id) {
                    self.pending_permission = None;
                    self.bottom_pane
                        .dismiss_view_by_id(BottomPane::PERMISSION_VIEW_ID);
                }
                Vec::new()
            }
            AppEvent::PermissionFinished(result) => {
                if let Err(error) = result {
                    self.error = Some(error);
                }
                Vec::new()
            }
            AppEvent::Paste(pasted) => {
                self.bottom_pane.handle_paste(pasted);
                self.drain_pane_events()
            }
            AppEvent::Tick => {
                self.tick = self.tick.wrapping_add(1);
                // The composer has no frame scheduler; it rides this tick to sync popups and to
                // release keystrokes it was holding as a suspected paste.
                let ticked = self.bottom_pane.pre_draw_tick();
                let flushed = self.bottom_pane.flush_paste_burst_if_due();
                self.pane_redraw |= ticked || flushed;
                self.drain_pane_events()
            }
        }
    }

    /// Routes one key press.
    ///
    /// Order matters: the permission modal is exclusive, then Ctrl+C and transcript scrolling stay
    /// with the app, and everything else belongs to the bottom pane - including Up/Down, which the
    /// composer uses for popup selection and prompt recall.
    fn reduce_key(&mut self, key: KeyEvent) -> Vec<AppCommand> {
        if key.kind == KeyEventKind::Release {
            return Vec::new();
        }

        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            // Once a cancel is under way, Ctrl+C means quit and nothing may swallow it - otherwise
            // a prompt arriving late would trap the user behind an extra keypress.
            if matches!(
                self.turn,
                TurnState::Cancelling | TurnState::CancelAcknowledged
            ) {
                return self.cancel_or_exit();
            }
            // Otherwise the pane gets first refusal, so Ctrl+C can dismiss a view or clear the
            // draft; only a pane with nothing left to cancel lets it mean interrupt-or-quit.
            return match self.bottom_pane.on_ctrl_c() {
                CancellationEvent::Handled => self.drain_pane_events(),
                CancellationEvent::NotHandled => self.cancel_or_exit(),
            };
        }

        match key.code {
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.turn == TurnState::Idle && self.bottom_pane.composer_is_empty() {
                    self.exit_requested = true;
                }
                return Vec::new();
            }
            _ => {}
        }

        let result = self.bottom_pane.handle_key_event(key);
        // A submission empties the composer, and that emptiness is not a draft edit: `submit`
        // decides what this scope should remember afterwards.
        let submitted = !matches!(result, InputResult::None);
        let mut commands = self.apply_input_result(result);
        commands.extend(self.drain_pane_events());
        if !submitted {
            self.remember_draft();
        }
        commands
    }

    /// Turns what the composer produced into work for the runtime.
    fn apply_input_result(&mut self, result: InputResult) -> Vec<AppCommand> {
        match result {
            InputResult::Submitted {
                text,
                text_elements,
            } => {
                if self.room_busy() && !text.trim_start().starts_with('/') {
                    if !self.enqueue_followup(&text, false) {
                        self.bottom_pane.set_composer_text(text, text_elements);
                    }
                    return Vec::new();
                }
                let local_queue_command = self.queue_command(&text);
                let commands = self.submit(text.clone());
                // The composer clears the draft as it submits, so a submission July refuses - a
                // turn is already in flight - has to be handed back or the text is lost.
                if commands.is_empty() && !local_queue_command {
                    self.bottom_pane.set_composer_text(text, text_elements);
                }
                commands
            }
            InputResult::Command(command) => {
                self.dispatch_composer_command(format!("/{}", command.command()))
            }
            InputResult::CommandWithArgs(command, args, _) => {
                self.dispatch_composer_command(format!("/{} {args}", command.command()))
            }
            InputResult::Queued {
                text,
                text_elements,
                pending_pastes,
                ..
            } => {
                let (text, text_elements) = super::bottom_pane::ChatComposer::expand_pending_pastes(
                    &text,
                    text_elements,
                    &pending_pastes,
                );
                if !self.room_busy() || !self.enqueue_followup(&text, true) {
                    self.bottom_pane.set_composer_text(text, text_elements);
                }
                Vec::new()
            }
            InputResult::None => Vec::new(),
        }
    }

    fn dispatch_composer_command(&mut self, input: String) -> Vec<AppCommand> {
        let local = self.queue_command(&input);
        let commands = self.submit(input.clone());
        if !commands.is_empty() || local {
            self.bottom_pane.finish_command_submission();
        } else {
            self.bottom_pane.set_composer_text(input, Vec::new());
            self.error = Some("Commands cannot be queued; wait until the turn finishes".into());
        }
        commands
    }

    /// Carries out the side effects the pane queued while handling input.
    fn drain_pane_events(&mut self) -> Vec<AppCommand> {
        let mut commands = Vec::new();
        for event in self.bottom_pane.drain_events() {
            match event {
                PaneEvent::Interrupt => commands.extend(self.cancel_or_exit()),
                PaneEvent::Notice { level, message } => match level {
                    NoticeLevel::Error => self.error = Some(message),
                    NoticeLevel::Info => {
                        self.freeze_stream();
                        self.markdown.push_plain(message, SYSTEM_COLOR);
                    }
                },
                PaneEvent::StartFileSearch(query) => {
                    let matches = if query.is_empty() {
                        Vec::new()
                    } else {
                        crate::tui::file_search::search(
                            std::path::Path::new("."),
                            &query,
                            FILE_SEARCH_LIMIT,
                        )
                    };
                    self.bottom_pane.on_file_search_result(query, matches);
                }
                PaneEvent::PermissionResponse {
                    request_id,
                    outcome,
                } => {
                    self.pending_permission = None;
                    commands.push(AppCommand::RespondPermission {
                        request_id,
                        outcome,
                    });
                }
                // July has no persistent prompt log and no agent question flow wired yet, so the
                // composer never asks for these.
                PaneEvent::LookupHistoryEntry { .. }
                | PaneEvent::LookupHistoryBatch { .. }
                | PaneEvent::UserInputAnswer { .. } => {}
            }
        }
        commands
    }

    fn cancel_or_exit(&mut self) -> Vec<AppCommand> {
        match self.turn {
            TurnState::Active => {
                self.hold_followups();
                self.turn = TurnState::Cancelling;
                vec![AppCommand::CancelTurn]
            }
            TurnState::Cancelling | TurnState::CancelAcknowledged => {
                self.exit_requested = true;
                Vec::new()
            }
            TurnState::Idle => {
                self.exit_requested = true;
                Vec::new()
            }
        }
    }
    /// Sends `text` as a prompt or a command, if a turn is not already in flight.
    fn submit(&mut self, text: String) -> Vec<AppCommand> {
        if self.pending.is_some() || self.turn != TurnState::Idle {
            return Vec::new();
        }
        if text.trim().is_empty() {
            return Vec::new();
        }
        if self.queue_command(&text) {
            self.bottom_pane.finish_command_submission();
            match text.trim() {
                "/queue resume" => {
                    if let Some(queue) = self.followups.get_mut(&self.context.id) {
                        queue.held = false;
                    }
                    self.error = None;
                    return self.release_followup();
                }
                "/queue discard" => {
                    self.followups.remove(&self.context.id);
                }
                _ => {
                    self.error = Some("Use /queue resume or /queue discard".into());
                }
            }
            return Vec::new();
        }
        self.error = None;
        // Leading blanks must not turn a command into chat.
        let command = text.trim_start().starts_with('/');

        if command {
            self.active_recipients = text
                .trim_start()
                .strip_prefix("/continue ")
                .and_then(crate::cli::mention::parse)
                .map(|mentions| {
                    mentions
                        .agents
                        .iter()
                        .map(|name| format!("@{name}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                });
        }

        if !command {
            self.active_recipients = crate::cli::mention::parse(&text)
                .map(|mentions| {
                    mentions
                        .agents
                        .iter()
                        .map(|name| format!("@{name}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .or_else(|| self.context_recipients());
            // A prompt consumes the draft; a command does not, so only a prompt clears what this
            // scope remembers.
            self.room_ui
                .entry(self.context.id.clone())
                .or_default()
                .draft = ComposerDraft::default();
        }
        self.bottom_pane.record_submission_history(text.clone());
        self.freeze_stream();
        self.push_user_entry(&text);
        self.turn = TurnState::Active;
        let context = self.context.id.clone();
        self.pending = Some(context.clone());
        vec![if command {
            AppCommand::Execute {
                context,
                input: text.trim_start().to_owned(),
            }
        } else {
            AppCommand::Submit { context, text }
        }]
    }

    fn room_busy(&self) -> bool {
        self.context.id.0.starts_with("room:")
            && self.active_recipients.is_some()
            && (self.pending.is_some() || self.turn != TurnState::Idle)
    }

    fn queue_command(&self, text: &str) -> bool {
        self.context.id.0.starts_with("room:")
            && self.pending.is_none()
            && self.turn == TurnState::Idle
            && (text.trim() == "/queue" || text.trim_start().starts_with("/queue "))
    }

    fn context_recipients(&self) -> Option<String> {
        self.context
            .label
            .rsplit_once(" → ")
            .map(|(_, names)| names)
            .filter(|names| names.starts_with('@'))
            .map(str::to_owned)
    }

    fn enqueue_followup(&mut self, text: &str, supplement: bool) -> bool {
        let mut recipients = self.active_recipients.clone();
        if recipients.as_deref() == Some("@auto") {
            recipients = if self.pending.is_none() {
                self.context_recipients()
            } else {
                None
            };
            if recipients.is_none() && crate::cli::mention::parse(text).is_none() {
                self.error =
                    Some("Wait until @auto selects an agent, or name an agent explicitly".into());
                return false;
            }
        }
        let text = match prepare_followup(text, supplement, recipients.as_deref()) {
            Ok(text) => text,
            Err(error) => {
                self.error = Some(error.into());
                return false;
            }
        };
        self.followups
            .entry(self.context.id.clone())
            .or_default()
            .push(text);
        self.room_ui
            .entry(self.context.id.clone())
            .or_default()
            .draft = ComposerDraft::default();
        self.error = None;
        self.freeze_stream();
        self.markdown.push_plain(
            if supplement {
                "Supplement queued; applies after the active turn."
            } else {
                "New request queued; sends after the active turn."
            }
            .into(),
            SYSTEM_COLOR,
        );
        true
    }

    fn hold_followups(&mut self) {
        self.followups
            .entry(self.context.id.clone())
            .or_default()
            .held = true;
    }

    fn release_followup(&mut self) -> Vec<AppCommand> {
        if self.turn != TurnState::Idle
            || self.pending.is_some()
            || self.pending_permission.is_some()
        {
            return Vec::new();
        }
        let Some(text) = self
            .followups
            .get_mut(&self.context.id)
            .and_then(Followups::pop)
        else {
            return Vec::new();
        };
        self.submit(text)
    }

    fn reduce_command_result(&mut self, context: ContextId, result: CommandResult) {
        if self.pending.as_ref() != Some(&context) {
            self.error = Some(format!("ignored stale command result for {context}"));
            return;
        }

        self.pending = None;
        match result {
            CommandResult::Submitted => {}
            CommandResult::SubmittedWithContext(snapshot) => {
                self.error = self.apply_snapshot(snapshot);
            }
            CommandResult::Context(context) => {
                if let Some(restored) = self.set_context(context) {
                    self.restore_room_ui(restored);
                }
                self.turn = TurnState::Idle;
            }
            CommandResult::ContextWithHistory(snapshot) => {
                self.error = self.apply_snapshot(snapshot);
                self.turn = TurnState::Idle;
            }
            CommandResult::Output { context, output } => {
                if let Some(restored) = self.set_context(context) {
                    self.restore_room_ui(restored);
                }
                // Command output belongs in the transcript: it can be long,
                // and the footer is one line.
                self.freeze_stream();
                self.markdown.push_plain(output, COMMAND_OUTPUT_COLOR);
                self.turn = TurnState::Idle;
            }
            CommandResult::Failed(error) => {
                self.hold_followups();
                self.error = Some(error.trim_end().to_owned());
                self.turn = TurnState::Idle;
            }
            CommandResult::FailedInContext { error, snapshot } => {
                self.hold_followups();
                let _ = self.apply_snapshot(snapshot);
                self.error = Some(error.trim_end().to_owned());
                self.turn = TurnState::Idle;
            }
        }
    }

    /// Replays a scope's stored history, but only when the scope actually changed.
    ///
    /// Rows that have already been written to the terminal's scrollback cannot be taken back, so
    /// replaying the same scope's history would print it a second time. A result that merely
    /// refreshes the current scope - the answer to a prompt sent from inside it - keeps the
    /// transcript that is already on screen.
    fn apply_snapshot(&mut self, snapshot: ContextSnapshot) -> Option<String> {
        let Some(restored) = self.set_context(snapshot.context) else {
            return match snapshot.history {
                Ok(_) => None,
                Err(error) => Some(error.trim_end().to_owned()),
            };
        };
        self.markdown = MarkdownStream::default();
        self.live_cells.clear();

        let error = match snapshot.history {
            Ok(history) => {
                if history.truncated {
                    self.markdown
                        .push_plain("… showing 50 most recent messages …".into(), SYSTEM_COLOR);
                }
                for entry in &history.entries {
                    self.push_history_entry(entry);
                }
                None
            }
            Err(error) => {
                if let Some(entry) = snapshot.history_fallback.as_ref() {
                    self.push_history_entry(entry);
                }
                Some(error.trim_end().to_owned())
            }
        };
        self.restore_room_ui(restored);
        error
    }

    /// Closes one agent's live cell, leaving the transcript to the Room's own record.
    ///
    /// What the cell streamed is a preview, not a record: only what the agent published reaches
    /// storage, and the transcript is rebuilt from storage on every scope switch. Committing the
    /// streamed text here would put a line on screen that vanishes the next time the room is
    /// opened. A failure reason is not the agent's output and does get said out loud.
    /// Other agents' cells are untouched.
    fn commit_live_cell(&mut self, agent: &AgentId, reason: Option<String>) {
        let label = self.live_cells.remove(agent).map(|cell| cell.label);
        let Some(reason) = reason else {
            return;
        };
        self.freeze_stream();
        self.markdown.push_plain(
            match label {
                Some(label) => format!("{label}: {reason}"),
                None => reason,
            },
            ERROR_COLOR,
        );
    }

    /// What one agent has streamed so far, or `None` when it has no open cell.
    #[cfg(test)]
    pub(crate) fn live_cell_body(&self, agent: &AgentId) -> Option<&str> {
        self.live_cells.get(agent).map(|cell| cell.body.as_str())
    }

    /// The rendered transcript as plain text, including live cells.
    #[cfg(test)]
    pub(crate) fn transcript_text_for_tests(&self) -> String {
        self.transcript_text()
            .lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Replays stored history the way a scope switch does, for tests in sibling modules.
    #[cfg(test)]
    pub(crate) fn apply_history_for_tests(&mut self, entries: &[HistoryEntry]) {
        for entry in entries {
            self.push_history_entry(entry);
        }
    }

    /// The rows of one finished block, ready for the terminal's scrollback.
    ///
    /// Returns `None` once nothing is left to write; the live tail and the live cells stay in the
    /// viewport, because they are still changing.
    pub(crate) fn take_finished_block(&mut self) -> Option<Text<'static>> {
        let block = self.markdown.pop_completed()?;
        let mut lines = spaced_rows(block.lines);
        // Blocks are written one at a time, so the separator that `spaced_rows` puts *between*
        // rows has to be added after the last one too.
        if lines.last().is_some_and(|line| line.width() > 0) {
            lines.push(Line::default());
        }
        Some(Text {
            alignment: block.alignment,
            style: block.style,
            lines,
        })
    }

    /// The live cells as transcript rows: a header per agent, its streamed body, and a cursor.
    fn live_cell_lines(&self) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        for cell in self.live_cells.values() {
            if !lines.is_empty() {
                lines.push(Line::default());
            }
            lines.push(Line::from(Span::styled(
                format!("{} · {}", cell.label, cell.activity_label(Instant::now())),
                Style::default().fg(AGENT_COLOR),
            )));
            lines.extend(render_markdown(&cell.body).lines);
            lines.push(Line::from(Span::styled(
                "▌",
                Style::default().fg(SYSTEM_COLOR),
            )));
        }
        lines
    }

    fn push_history_entry(&mut self, entry: &HistoryEntry) {
        match entry.author {
            HistoryAuthor::System => self.markdown.push_plain(entry.body.clone(), SYSTEM_COLOR),
            HistoryAuthor::User => self.push_user_entry(&entry.body),
            HistoryAuthor::Agent => {
                self.markdown.push(&entry.body);
                self.markdown.finish();
            }
        }
    }

    fn push_user_entry(&mut self, body: &str) {
        self.markdown.push_plain(
            body.lines()
                .map(|line| format!("› {line}"))
                .collect::<Vec<_>>()
                .join("\n"),
            USER_COLOR,
        );
    }

    fn reduce_chat_content(
        &mut self,
        events: impl IntoIterator<Item = ChatEvent>,
    ) -> Vec<AppCommand> {
        let mut commands = Vec::new();
        for event in events {
            commands.extend(self.reduce_chat(event));
        }
        commands
    }

    fn reduce_chat(&mut self, event: ChatEvent) -> Vec<AppCommand> {
        match event {
            ChatEvent::TextDelta(text) => {
                self.markdown.push(&text);
                if self.turn == TurnState::Idle {
                    self.turn = TurnState::Active;
                }
            }
            ChatEvent::MessageCompleted(_) => self.freeze_stream(),
            ChatEvent::TurnCompleted => {
                self.freeze_stream();
                self.finish_turn();
            }
            ChatEvent::TurnFailed(failure) => {
                self.hold_followups();
                self.freeze_stream();
                self.markdown
                    .push_plain(format!("error: {}", failure_label(failure)), ERROR_COLOR);
                self.finish_turn();
            }
            ChatEvent::Disconnected(reason) => {
                self.hold_followups();
                self.freeze_stream();
                self.markdown
                    .push_plain(format!("error: {reason}"), ERROR_COLOR);
                self.finish_turn();
            }
            ChatEvent::PermissionRequested {
                request_id,
                prompt,
                options,
            } => {
                if options.is_empty() {
                    self.error = Some("permission request had no choices".into());
                    return vec![AppCommand::RespondPermission {
                        request_id,
                        outcome: PermissionOutcome::Cancelled,
                    }];
                }
                if self.turn == TurnState::Idle {
                    self.turn = TurnState::Active;
                }
                self.pending_permission = Some(request_id.clone());
                self.bottom_pane
                    .push_permission_request(request_id, prompt, options);
            }
        }
        Vec::new()
    }

    fn finish_turn(&mut self) {
        self.turn = TurnState::Idle;
        self.pending_permission = None;
        self.bottom_pane
            .dismiss_view_by_id(BottomPane::PERMISSION_VIEW_ID);
    }

    fn freeze_stream(&mut self) {
        self.markdown.finish();
    }
}

/// ponytail: terminal cells have no line-height; one spacer row between consecutive non-empty
/// lines is the closest equivalent. Code rows keep their own spacing, so they are left alone.
fn spaced_rows(rows: Vec<Line<'static>>) -> Vec<Line<'static>> {
    let mut lines = Vec::with_capacity(rows.len() * 2);
    let mut rest = rows.into_iter().peekable();
    while let Some(line) = rest.next() {
        let spaced = line.width() > 0
            && line.style.fg != Some(CODE_COLOR)
            && rest.peek().is_some_and(|next: &Line<'static>| {
                next.width() > 0 && next.style.fg != Some(CODE_COLOR)
            });
        lines.push(line);
        if spaced {
            lines.push(Line::default());
        }
    }
    lines
}

pub fn next_event(terminal: Option<AppEvent>, chat: &mut VecDeque<ChatEvent>) -> Option<AppEvent> {
    terminal.or_else(|| {
        let batch: Vec<_> = chat.drain(..chat.len().min(CHAT_BATCH_LIMIT)).collect();
        (!batch.is_empty()).then_some(AppEvent::ChatBatch(batch))
    })
}

fn failure_label(failure: ChatFailureKind) -> &'static str {
    match failure {
        ChatFailureKind::AuthenticationRequired => "authentication required",
        ChatFailureKind::Protocol => "protocol error",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
    use ratatui::style::Color;

    use super::*;
    use crate::application::{ChatEvent, ChatFailureKind, ChatPermissionRequestId};
    use crate::domain::{MemberType, Message, PermissionOption, PermissionOutcome};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn alt_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::ALT)
    }

    fn shift_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::SHIFT)
    }

    fn ctrl_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    fn repeat_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new_with_kind(code, KeyModifiers::NONE, KeyEventKind::Repeat)
    }

    fn foreground_of(text: &Text<'_>, content: &str) -> Option<Color> {
        text.lines.iter().find_map(|line| {
            line.spans.iter().find_map(|span| {
                span.content
                    .contains(content)
                    .then(|| text.style.patch(line.style).patch(span.style).fg)
                    .flatten()
            })
        })
    }

    /// Commands dispatched one after another must not pile up in the draft.
    ///
    /// The composer hands out an inline command such as `/dm codex` but leaves its text in place,
    /// so without clearing it the next command is typed onto the end of the last one.
    #[test]
    fn a_dispatched_command_leaves_the_draft_empty_for_the_next_one() {
        let mut app = App::new(Context::root().with_commands(vec![
            "/room".into(),
            "/dm".into(),
            "/back".into(),
        ]));

        for character in "/room vna".chars() {
            app.reduce(AppEvent::Key(key(KeyCode::Char(character))));
        }
        let first = app.reduce(AppEvent::Key(key(KeyCode::Enter)));
        assert_eq!(
            first,
            vec![AppCommand::Execute {
                context: ContextId::root(),
                input: "/room vna".into(),
            }]
        );
        assert!(app.input().is_empty(), "draft was {:?}", app.input());

        app.reduce(AppEvent::CommandFinished {
            context: ContextId::root(),
            result: CommandResult::Submitted,
        });
        app.reduce(AppEvent::Chat(ChatEvent::TurnCompleted));

        for character in "/back".chars() {
            app.reduce(AppEvent::Key(key(KeyCode::Char(character))));
        }
        let second = app.reduce(AppEvent::Key(key(KeyCode::Enter)));

        assert_eq!(
            second,
            vec![AppCommand::Execute {
                context: ContextId::root(),
                input: "/back".into(),
            }]
        );
        assert!(app.input().is_empty(), "draft was {:?}", app.input());
    }

    /// A command July refuses keeps its text, so the user can resend it.
    #[test]
    fn a_refused_command_stays_in_the_draft() {
        let mut app = App::new(Context::root().with_commands(vec!["/room".into()]));
        for character in "/room vna".chars() {
            app.reduce(AppEvent::Key(key(KeyCode::Char(character))));
        }
        assert_eq!(app.reduce(AppEvent::Key(key(KeyCode::Enter))).len(), 1);

        // A turn is now in flight, so the next command cannot be sent.
        for character in "/room other".chars() {
            app.reduce(AppEvent::Key(key(KeyCode::Char(character))));
        }
        let refused = app.reduce(AppEvent::Key(key(KeyCode::Enter)));

        assert!(refused.is_empty());
        assert_eq!(app.input(), "/room other");
    }

    /// Ctrl+C must still quit once a cancel is under way, even with a prompt open.
    ///
    /// The permission prompt is a pane view and views consume Ctrl+C, so without an explicit rule a
    /// prompt arriving after the cancel would trap the user behind an extra keypress.
    #[test]
    fn ctrl_c_still_quits_while_cancelling_even_with_a_permission_prompt_open() {
        let mut app = App::new(Context::root());
        app.turn = TurnState::CancelAcknowledged;
        app.reduce(AppEvent::Chat(ChatEvent::PermissionRequested {
            request_id: "late".to_owned().into(),
            prompt: "Allow?".into(),
            options: vec![PermissionOption {
                id: "once".into(),
                label: "Allow once".into(),
            }],
        }));
        assert!(app.bottom_pane.has_active_view());

        let commands = app.reduce(AppEvent::Key(ctrl_key(KeyCode::Char('c'))));

        assert!(commands.is_empty(), "{commands:?}");
        assert!(app.exit_requested());
    }

    // ---- per-scope UI state -------------------------------------------------

    fn room(name: &str) -> Context {
        Context::new(ContextId::new(format!("room:{name}")), name)
    }

    /// Switches the app to `context` the way the REPL does: a command is submitted, and its result
    /// carries the new scope back. A result with nothing pending is rejected as stale, so the
    /// submission is what makes the switch land.
    fn switch_to(app: &mut App, context: Context) {
        let from = app.context().id().clone();
        app.reduce(AppEvent::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )));
        app.reduce(AppEvent::Key(key(KeyCode::Char('/'))));
        app.reduce(AppEvent::Key(key(KeyCode::Enter)));
        app.reduce(AppEvent::CommandFinished {
            context: from,
            result: CommandResult::Context(context),
        });
    }

    /// Switches scope through a history reload, the path a Room switch takes when it repopulates
    /// the transcript.
    fn switch_to_with_history(app: &mut App, context: Context, entries: Vec<HistoryEntry>) {
        let from = app.context().id().clone();
        app.reduce(AppEvent::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )));
        app.reduce(AppEvent::Key(key(KeyCode::Char('/'))));
        app.reduce(AppEvent::Key(key(KeyCode::Enter)));
        app.reduce(AppEvent::CommandFinished {
            context: from,
            result: CommandResult::ContextWithHistory(snapshot(
                context,
                Ok(History {
                    entries,
                    truncated: false,
                }),
                None,
            )),
        });
    }

    fn fill_transcript(app: &mut App, lines: usize) {
        for index in 0..lines {
            app.reduce(AppEvent::RoomStatus(format!("line {index}")));
        }
    }

    #[test]
    fn leaving_a_room_and_coming_back_restores_its_draft() {
        let mut app = App::new(room("alpha"));
        app.reduce(AppEvent::Resize {
            width: 40,
            height: 12,
        });
        fill_transcript(&mut app, 40);
        for character in "half typed".chars() {
            app.reduce(AppEvent::Key(key(KeyCode::Char(character))));
        }

        switch_to(&mut app, room("beta"));
        assert_eq!(app.input(), "", "a fresh room starts with an empty draft");
        for character in "beta draft".chars() {
            app.reduce(AppEvent::Key(key(KeyCode::Char(character))));
        }

        switch_to(&mut app, room("alpha"));
        assert_eq!(app.input(), "half typed");

        switch_to(&mut app, room("beta"));
        assert_eq!(app.input(), "beta draft");
    }

    #[test]
    fn a_room_visited_for_the_first_time_gets_default_ui_state() {
        let mut app = App::new(room("alpha"));
        app.reduce(AppEvent::Resize {
            width: 40,
            height: 12,
        });
        fill_transcript(&mut app, 40);
        for character in "draft".chars() {
            app.reduce(AppEvent::Key(key(KeyCode::Char(character))));
        }

        switch_to(&mut app, room("never-seen"));

        assert_eq!(app.input(), "");
    }

    #[test]
    fn a_reloaded_transcript_restores_the_draft() {
        let mut app = App::new(room("alpha"));
        app.reduce(AppEvent::Resize {
            width: 40,
            height: 12,
        });
        fill_transcript(&mut app, 40);
        for character in "kept".chars() {
            app.reduce(AppEvent::Key(key(KeyCode::Char(character))));
        }

        switch_to(&mut app, room("beta"));
        // Coming back through a history reload: the transcript is rebuilt from storage and the
        // draft survives it.
        switch_to_with_history(
            &mut app,
            room("alpha"),
            vec![HistoryEntry {
                author: HistoryAuthor::Agent,
                body: "one line".into(),
            }],
        );

        assert_eq!(app.input(), "kept");
    }

    // ---- live agent cells ---------------------------------------------------

    fn agent(seed: u128) -> AgentId {
        AgentId::from(ulid::Ulid::from(seed))
    }

    fn start(app: &mut App, id: AgentId, label: &str) {
        app.reduce(AppEvent::AgentStreamStarted {
            agent: id,
            label: label.to_owned(),
        });
    }

    fn delta(app: &mut App, id: AgentId, text: &str) {
        app.reduce(AppEvent::AgentStreamDelta {
            agent: id,
            delta: text.to_owned(),
        });
    }

    #[test]
    fn two_agents_stream_side_by_side() {
        let mut app = App::new(room("alpha"));
        let agent_order = agent(1);
        let pay = agent(2);

        start(&mut app, agent_order, "agent_order");
        start(&mut app, pay, "pay");
        delta(&mut app, agent_order, "Checking callback handler...");
        delta(&mut app, pay, "Inspecting refund state...");

        let transcript = app.transcript_text_for_tests();
        assert!(transcript.contains("agent_order"), "{transcript}");
        assert!(
            transcript.contains("Checking callback handler..."),
            "{transcript}"
        );
        assert!(transcript.contains("pay"), "{transcript}");
        assert!(
            transcript.contains("Inspecting refund state..."),
            "{transcript}"
        );
    }

    #[test]
    fn a_delta_only_reaches_the_agent_it_names() {
        let mut app = App::new(room("alpha"));
        let agent_order = agent(1);
        let pay = agent(2);
        start(&mut app, agent_order, "agent_order");
        start(&mut app, pay, "pay");

        delta(&mut app, agent_order, "only mine");

        assert_eq!(app.live_cell_body(&agent_order), Some("only mine"));
        assert_eq!(app.live_cell_body(&pay), Some(""));
    }

    #[test]
    fn a_delta_for_an_agent_with_no_open_cell_is_dropped() {
        let mut app = App::new(room("alpha"));
        let agent_order = agent(1);
        start(&mut app, agent_order, "agent_order");

        delta(&mut app, agent(99), "from nowhere");

        assert_eq!(app.live_cell_body(&agent_order), Some(""));
        assert_eq!(app.live_cell_body(&agent(99)), None);
        assert!(!app.transcript_text_for_tests().contains("from nowhere"));
    }

    #[test]
    fn activity_age_is_monotonic_and_does_not_claim_a_hang() {
        let now = std::time::Instant::now();
        let cell = LiveCell {
            label: "pay".into(),
            body: String::new(),
            activity: AgentActivity::Working,
            last_update: now,
        };
        assert_eq!(cell.activity_label(now), "working · 0s");
        assert_eq!(
            cell.activity_label(now + std::time::Duration::from_secs(59)),
            "working · 59s"
        );
        assert_eq!(
            cell.activity_label(now + std::time::Duration::from_secs(60)),
            "working · no update for 60s"
        );
    }

    #[test]
    fn activity_is_agent_local_and_text_preserves_tools_and_permissions() {
        let mut app = App::new(room("alpha"));
        start(&mut app, agent(1), "pay");
        start(&mut app, agent(2), "infra");
        for activity in [AgentActivity::ToolRunning, AgentActivity::WaitingPermission] {
            app.reduce(AppEvent::AgentActivity {
                agent: agent(1),
                activity,
            });
            delta(&mut app, agent(1), "text");
            assert_eq!(app.live_cells[&agent(1)].activity, activity);
            assert_eq!(app.live_cells[&agent(2)].activity, AgentActivity::Working);
        }
        app.reduce(AppEvent::AgentActivity {
            agent: agent(99),
            activity: AgentActivity::ToolRunning,
        });
        assert!(!app.live_cells.contains_key(&agent(99)));
        app.reduce(AppEvent::AgentStreamFinished { agent: agent(1) });
        assert!(!app.live_cells.contains_key(&agent(1)));
    }

    #[test]
    fn finishing_one_agent_closes_its_cell_and_leaves_the_other_streaming() {
        let mut app = App::new(room("alpha"));
        let agent_order = agent(1);
        let pay = agent(2);
        start(&mut app, agent_order, "agent_order");
        start(&mut app, pay, "pay");
        delta(&mut app, agent_order, "done looking");
        delta(&mut app, pay, "still looking");

        app.reduce(AppEvent::AgentStreamFinished { agent: agent_order });

        assert_eq!(app.live_cell_body(&agent_order), None, "its cell is closed");
        assert_eq!(
            app.live_cell_body(&pay),
            Some("still looking"),
            "the other agent is untouched"
        );
        let transcript = app.transcript_text_for_tests();
        assert!(
            !transcript.contains("done looking"),
            "the preview goes with the cell; what the agent published is what stays:\n{transcript}"
        );
        assert!(
            transcript.contains("still looking"),
            "the other agent's preview is still on screen:\n{transcript}"
        );
    }

    #[test]
    fn a_result_that_refreshes_the_same_scope_does_not_replay_its_history() {
        let mut app = App::new(room("alpha"));
        app.reduce(AppEvent::Resize {
            width: 40,
            height: 12,
        });
        // Whatever is already on screen has been written to the terminal's scrollback and cannot
        // be taken back, so a refresh of the same scope must not hand it over a second time.
        switch_to_with_history(
            &mut app,
            room("alpha"),
            vec![HistoryEntry {
                author: HistoryAuthor::Agent,
                body: "already printed".into(),
            }],
        );

        let mut blocks = Vec::new();
        while let Some(block) = app.take_finished_block() {
            blocks.push(block.to_string());
        }

        assert!(
            blocks
                .iter()
                .all(|block| !block.contains("already printed")),
            "{blocks:?}"
        );
    }

    #[test]
    fn only_a_real_scope_switch_asks_for_the_scrollback_to_be_erased() {
        let mut app = App::new(room("alpha"));
        assert!(!app.take_scope_changed(), "opening is not a switch");

        switch_to(&mut app, room("beta"));
        assert!(app.take_scope_changed());
        assert!(
            !app.take_scope_changed(),
            "asking twice does not erase twice"
        );

        // A refresh of the same scope - a new label or command list - is not a switch.
        switch_to(&mut app, room("beta"));
        assert!(!app.take_scope_changed());
    }

    #[test]
    fn finished_blocks_leave_the_transcript_oldest_first_and_the_live_tail_stays() {
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Resize {
            width: 40,
            height: 12,
        });
        app.reduce(AppEvent::Chat(ChatEvent::TextDelta(
            "first block\n\nsecond block\n\nstill typ".into(),
        )));

        let first = app.take_finished_block().expect("first block is finished");
        let second = app.take_finished_block().expect("second block is finished");

        assert!(first.to_string().contains("first block"), "{first:?}");
        assert!(second.to_string().contains("second block"), "{second:?}");
        // The unfinished tail is still being written, so it stays in the band rather than being
        // handed to the scrollback.
        assert_eq!(app.take_finished_block(), None);
        assert!(
            app.transcript_text_for_tests().contains("still typ"),
            "{}",
            app.transcript_text_for_tests()
        );
        assert!(!app.transcript_text_for_tests().contains("first block"));
    }

    #[test]
    fn a_live_preview_is_styled_as_markdown_like_the_message_it_becomes() {
        let mut app = App::new(room("alpha"));
        let agent_order = agent(1);
        start(&mut app, agent_order, "agent_order");
        delta(&mut app, agent_order, "run `cargo test`");

        let foreground = app
            .transcript_text()
            .lines
            .iter()
            .flat_map(|line| line.spans.clone())
            .find(|span| span.content == "cargo test")
            .and_then(|span| span.style.fg);

        assert_eq!(foreground, Some(CODE_COLOR));
    }

    #[test]
    fn a_failing_agent_reports_why_and_drops_its_preview() {
        let mut app = App::new(room("alpha"));
        let agent_order = agent(1);
        let pay = agent(2);
        start(&mut app, agent_order, "agent_order");
        start(&mut app, pay, "pay");
        delta(&mut app, agent_order, "partial answer");

        app.reduce(AppEvent::AgentStreamFailed {
            agent: agent_order,
            reason: "failed: transport closed".into(),
        });

        assert_eq!(app.live_cell_body(&agent_order), None);
        assert_eq!(app.live_cell_body(&pay), Some(""));
        let transcript = app.transcript_text_for_tests();
        assert!(
            !transcript.contains("partial answer"),
            "the preview goes with the cell:\n{transcript}"
        );
        assert!(
            transcript.contains("agent_order: failed: transport closed"),
            "the reason is reported against the agent that failed:\n{transcript}"
        );
    }

    #[test]
    fn a_failure_for_an_agent_with_no_open_cell_still_reports_the_reason() {
        let mut app = App::new(room("alpha"));

        app.reduce(AppEvent::AgentStreamFailed {
            agent: agent(1),
            reason: "agent_order: failed: transport closed".into(),
        });

        assert!(
            app.transcript_text_for_tests()
                .contains("agent_order: failed: transport closed")
        );
    }

    #[test]
    fn an_agent_that_streamed_nothing_closes_without_leaving_a_row() {
        let mut app = App::new(room("alpha"));
        let agent_order = agent(1);
        start(&mut app, agent_order, "agent_order");

        app.reduce(AppEvent::AgentStreamFinished { agent: agent_order });

        assert_eq!(app.live_cell_body(&agent_order), None);
        assert!(app.transcript().trim().is_empty(), "{:?}", app.transcript());
    }

    #[test]
    fn switching_rooms_drops_live_cells_from_the_room_being_left() {
        let mut app = App::new(room("alpha"));
        let agent_order = agent(1);
        start(&mut app, agent_order, "agent_order");
        delta(&mut app, agent_order, "mid flight");

        switch_to_with_history(&mut app, room("beta"), Vec::new());

        assert_eq!(app.live_cell_body(&agent_order), None);
        assert!(!app.transcript_text_for_tests().contains("mid flight"));
    }

    fn snapshot(
        context: Context,
        history: Result<History, String>,
        history_fallback: Option<HistoryEntry>,
    ) -> ContextSnapshot {
        ContextSnapshot {
            context,
            history,
            history_fallback,
        }
    }

    fn pending_app(context: Context, input: &str) -> App {
        let mut app = App::new(context);
        for character in input.chars() {
            app.reduce(AppEvent::Key(key(KeyCode::Char(character))));
        }
        app.reduce(AppEvent::Key(key(KeyCode::Enter)));
        app
    }

    #[test]
    fn room_permission_dismissal_only_clears_the_matching_modal() {
        let mut app = App::new(Context::root());
        app.turn = TurnState::Active;
        app.reduce(AppEvent::Chat(ChatEvent::PermissionRequested {
            request_id: "binding:new".to_owned().into(),
            prompt: "permission".into(),
            options: vec![PermissionOption {
                id: "once".into(),
                label: "Allow once".into(),
            }],
        }));
        app.reduce(AppEvent::RoomPermissionDismissed(
            "binding:old".to_owned().into(),
        ));
        assert!(app.permission().is_some());
        assert!(app.bottom_pane.has_active_view());
        app.reduce(AppEvent::RoomPermissionDismissed(
            "binding:new".to_owned().into(),
        ));
        assert!(app.permission().is_none());
        assert!(!app.bottom_pane.has_active_view(), "its prompt closed too");
    }

    #[test]
    fn submitted_snapshot_replaces_routing_echo_once_and_keeps_turn_active() {
        let mut app = pending_app(Context::root(), "@ada hello");
        app.reduce(AppEvent::CommandFinished {
            context: ContextId::root(),
            result: CommandResult::SubmittedWithContext(snapshot(
                Context::new(ContextId::new("dm:01"), "dm::ada"),
                Ok(History {
                    entries: vec![HistoryEntry {
                        author: HistoryAuthor::User,
                        body: "hello".into(),
                    }],
                    truncated: false,
                }),
                Some(HistoryEntry {
                    author: HistoryAuthor::User,
                    body: "hello".into(),
                }),
            )),
        });
        assert_eq!(app.context().label(), "dm::ada");
        assert_eq!(app.transcript().matches("› hello").count(), 1);
        assert!(!app.transcript().contains("@ada"));
        assert!(app.turn_active());
    }

    #[test]
    fn context_history_replaces_old_text_in_order_with_exact_marker_color() {
        let mut app = pending_app(Context::root(), "/dm ada");
        app.reduce(AppEvent::CommandFinished {
            context: ContextId::root(),
            result: CommandResult::ContextWithHistory(snapshot(
                Context::new(ContextId::new("dm:01"), "dm::ada"),
                Ok(History {
                    entries: vec![
                        HistoryEntry {
                            author: HistoryAuthor::User,
                            body: "question".into(),
                        },
                        HistoryEntry {
                            author: HistoryAuthor::Agent,
                            body: "**answer**".into(),
                        },
                    ],
                    truncated: true,
                }),
                None,
            )),
        });
        let transcript = app.transcript();
        assert_eq!(
            transcript.lines().next(),
            Some("… showing 50 most recent messages …")
        );
        assert!(transcript.find("› question").unwrap() < transcript.find("answer").unwrap());
        assert!(!transcript.contains("/dm ada"));
        assert_eq!(
            foreground_of(
                &app.transcript_text(),
                "… showing 50 most recent messages …"
            ),
            Some(SYSTEM_COLOR)
        );
    }

    #[test]
    fn stale_snapshot_changes_neither_context_nor_transcript() {
        let mut app = pending_app(Context::root(), "/dm ada");
        let before = app.transcript();
        app.reduce(AppEvent::CommandFinished {
            context: ContextId::new("stale"),
            result: CommandResult::ContextWithHistory(snapshot(
                Context::new(ContextId::new("dm:poison"), "poison"),
                Ok(History {
                    entries: vec![HistoryEntry {
                        author: HistoryAuthor::Agent,
                        body: "poison".into(),
                    }],
                    truncated: false,
                }),
                None,
            )),
        });
        assert_eq!(app.context(), &Context::root());
        assert_eq!(app.transcript(), before);
        assert_eq!(app.error(), Some("ignored stale command result for stale"));
    }

    #[test]
    fn history_failure_uses_fallback_and_trims_the_history_error() {
        let mut app = pending_app(Context::root(), "@ada hello");
        app.reduce(AppEvent::CommandFinished {
            context: ContextId::root(),
            result: CommandResult::SubmittedWithContext(snapshot(
                Context::new(ContextId::new("dm:01"), "dm::ada"),
                Err("history unavailable\n".into()),
                Some(HistoryEntry {
                    author: HistoryAuthor::User,
                    body: "hello".into(),
                }),
            )),
        });
        assert_eq!(app.transcript(), "› hello");
        assert_eq!(app.error(), Some("history unavailable"));
        assert!(app.turn_active());
    }

    #[test]
    fn failed_in_context_prefers_send_error_and_has_no_unproven_fallback() {
        let mut app = pending_app(Context::root(), "@ada hello");
        app.reduce(AppEvent::CommandFinished {
            context: ContextId::root(),
            result: CommandResult::FailedInContext {
                error: "send failed\n".into(),
                snapshot: snapshot(
                    Context::new(ContextId::new("dm:01"), "dm::ada"),
                    Err("history failed".into()),
                    None,
                ),
            },
        });
        assert_eq!(app.context().label(), "dm::ada");
        assert!(app.transcript().is_empty());
        assert_eq!(app.error(), Some("send failed"));
        assert_eq!(app.turn_state(), TurnState::Idle);
    }

    #[test]
    fn empty_root_snapshot_clears_chat_but_preserves_prompt_history() {
        let dm = Context::new(ContextId::new("dm:01"), "dm::ada");
        let mut app = App::new(dm.clone());
        app.reduce(AppEvent::Chat(ChatEvent::TextDelta(
            "old agent text".into(),
        )));
        app.reduce(AppEvent::Chat(ChatEvent::TurnCompleted));
        for character in "/back".chars() {
            app.reduce(AppEvent::Key(key(KeyCode::Char(character))));
        }
        app.reduce(AppEvent::Key(key(KeyCode::Enter)));
        app.reduce(AppEvent::CommandFinished {
            context: dm.id().clone(),
            result: CommandResult::ContextWithHistory(snapshot(
                Context::root(),
                Ok(History {
                    entries: Vec::new(),
                    truncated: false,
                }),
                None,
            )),
        });
        assert!(app.transcript().is_empty());
        app.reduce(AppEvent::Key(key(KeyCode::Up)));
        assert_eq!(app.input(), "/back");
    }

    #[test]
    fn transcript_sources_use_distinct_palette_colors() {
        let mut app = App::new(Context::root());
        for character in "/help".chars() {
            app.reduce(AppEvent::Key(key(KeyCode::Char(character))));
        }
        app.reduce(AppEvent::Key(key(KeyCode::Enter)));
        app.reduce(AppEvent::CommandFinished {
            context: ContextId::root(),
            result: CommandResult::Output {
                context: Context::root(),
                output: "command output".into(),
            },
        });
        app.reduce(AppEvent::Chat(ChatEvent::TextDelta(
            "agent response".into(),
        )));
        app.reduce(AppEvent::Chat(ChatEvent::Disconnected("offline".into())));

        let transcript = app.transcript_text();
        assert_eq!(
            foreground_of(&transcript, "› /help"),
            Some(Color::Rgb(121, 192, 255))
        );
        assert_eq!(
            foreground_of(&transcript, "command output"),
            Some(Color::Rgb(126, 231, 135))
        );
        assert_eq!(
            foreground_of(&transcript, "agent response"),
            Some(Color::Rgb(208, 215, 222))
        );
        assert_eq!(
            foreground_of(&transcript, "error: offline"),
            Some(Color::Rgb(255, 123, 114))
        );
    }

    #[test]
    fn fenced_curl_keeps_its_line_continuations_copyable() {
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Chat(ChatEvent::TextDelta(
            r#"```sh
curl --request POST 'https://example.com/v1/orders' \
  --header 'Content-Type: application/json' \
  --data '{"name":"Tony"}'
```"#
                .into(),
        )));
        app.reduce(AppEvent::Chat(ChatEvent::TurnCompleted));

        assert_eq!(
            app.transcript(),
            r#"curl --request POST 'https://example.com/v1/orders' \
  --header 'Content-Type: application/json' \
  --data '{"name":"Tony"}'"#
        );
    }

    #[test]
    fn active_turn_spinner_uses_the_system_palette_color() {
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Chat(ChatEvent::TextDelta("response".into())));

        assert_eq!(
            foreground_of(&app.transcript_text(), "working"),
            Some(Color::Rgb(227, 179, 65))
        );
    }

    #[test]
    fn unicode_input_edits_on_character_boundaries_and_submits_exact_text() {
        let mut app = App::new(Context::root());

        app.reduce(AppEvent::Key(key(KeyCode::Char('é'))));
        app.reduce(AppEvent::Key(key(KeyCode::Char('中'))));
        app.reduce(AppEvent::Key(key(KeyCode::Left)));
        app.reduce(AppEvent::Key(key(KeyCode::Char('x'))));

        assert_eq!(app.input(), "éx中");
        assert_eq!(
            app.reduce(AppEvent::Key(key(KeyCode::Enter))),
            vec![AppCommand::Submit {
                context: ContextId::root(),
                text: "éx中".into(),
            }]
        );
    }

    #[test]
    fn alt_enter_keeps_a_newline_in_the_exact_submitted_text() {
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Key(key(KeyCode::Char('a'))));
        app.reduce(AppEvent::Key(alt_key(KeyCode::Enter)));
        app.reduce(AppEvent::Key(key(KeyCode::Char('b'))));

        assert_eq!(
            app.reduce(AppEvent::Key(key(KeyCode::Enter))),
            vec![AppCommand::Submit {
                context: ContextId::root(),
                text: "a\nb".into(),
            }]
        );
    }

    #[test]
    fn shift_enter_keeps_a_newline_in_the_exact_submitted_text() {
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Key(key(KeyCode::Char('a'))));
        app.reduce(AppEvent::Key(shift_key(KeyCode::Enter)));
        app.reduce(AppEvent::Key(key(KeyCode::Char('b'))));

        assert_eq!(app.input(), "a\nb");
        app.reduce(AppEvent::Key(alt_key(KeyCode::Char('j'))));
        assert_eq!(app.input(), "a\nb");
    }

    #[test]
    fn legacy_ctrl_enter_ctrl_j_keeps_a_newline() {
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Key(key(KeyCode::Char('a'))));
        app.reduce(AppEvent::Key(ctrl_key(KeyCode::Char('j'))));
        app.reduce(AppEvent::Key(key(KeyCode::Char('b'))));

        assert_eq!(app.input(), "a\nb");
    }

    #[test]
    fn arrow_keys_recall_submitted_prompts_only_from_an_untouched_draft() {
        let mut app = App::new(Context::root());
        for prompt in ["first", "second"] {
            for character in prompt.chars() {
                app.reduce(AppEvent::Key(key(KeyCode::Char(character))));
            }
            app.reduce(AppEvent::Key(key(KeyCode::Enter)));
            app.reduce(AppEvent::CommandFinished {
                context: ContextId::root(),
                result: CommandResult::Submitted,
            });
            app.reduce(AppEvent::Chat(ChatEvent::TurnCompleted));
        }

        // An empty composer recalls newest-first, and walks back down again.
        app.reduce(AppEvent::Key(key(KeyCode::Up)));
        assert_eq!(app.input(), "second");
        app.reduce(AppEvent::Key(key(KeyCode::Up)));
        assert_eq!(app.input(), "first");
        app.reduce(AppEvent::Key(key(KeyCode::Down)));
        assert_eq!(app.input(), "second");

        // Editing a recalled entry takes the composer out of recall: Up is then ordinary cursor
        // movement inside the draft, which is what shells do and what the composer expects.
        app.reduce(AppEvent::Key(key(KeyCode::Char('!'))));
        assert_eq!(app.input(), "second!");
        app.reduce(AppEvent::Key(key(KeyCode::Up)));
        assert_eq!(app.input(), "second!");
    }
    #[test]
    fn terminal_chat_events_keep_pending_until_matching_submit_ack() {
        let terminal_events = [
            ChatEvent::TurnCompleted,
            ChatEvent::TurnFailed(ChatFailureKind::Protocol),
            ChatEvent::Disconnected("offline".into()),
        ];

        for terminal_event in terminal_events {
            let mut app = App::new(Context::root());
            app.reduce(AppEvent::Key(key(KeyCode::Char('o'))));
            assert_eq!(app.reduce(AppEvent::Key(key(KeyCode::Enter))).len(), 1);
            app.reduce(AppEvent::Chat(terminal_event));

            app.reduce(AppEvent::Key(key(KeyCode::Char('n'))));
            assert!(app.reduce(AppEvent::Key(key(KeyCode::Enter))).is_empty());

            app.reduce(AppEvent::CommandFinished {
                context: ContextId::root(),
                result: CommandResult::Submitted,
            });
            assert_eq!(app.error(), None);
            assert_eq!(
                app.reduce(AppEvent::Key(key(KeyCode::Enter))),
                vec![AppCommand::Submit {
                    context: ContextId::root(),
                    text: "n".into(),
                }]
            );
        }
    }

    #[test]
    fn context_label_is_a_stable_projection_of_the_active_identity() {
        let context = Context::new(ContextId::new("dm:01"), "dm · Ada");
        let app = App::new(context);

        assert_eq!(app.context().id(), &ContextId::new("dm:01"));
        assert_eq!(app.context().label(), "dm · Ada");
    }

    #[test]
    fn context_carries_visible_commands() {
        let context = Context::new(ContextId::new("dm:01"), "dm · Ada")
            .with_commands(vec!["/dm".into(), "/restart".into()]);

        assert_eq!(context.commands(), ["/dm", "/restart"]);
    }

    #[test]
    fn only_one_submit_can_be_pending_at_a_time() {
        let mut app = App::new(Context::root());

        app.reduce(AppEvent::Key(key(KeyCode::Char('o'))));
        app.reduce(AppEvent::Key(key(KeyCode::Char('n'))));
        app.reduce(AppEvent::Key(key(KeyCode::Char('e'))));
        assert_eq!(app.reduce(AppEvent::Key(key(KeyCode::Enter))).len(), 1);

        app.reduce(AppEvent::Key(key(KeyCode::Char('t'))));
        app.reduce(AppEvent::Key(key(KeyCode::Char('w'))));
        app.reduce(AppEvent::Key(key(KeyCode::Char('o'))));

        assert!(app.reduce(AppEvent::Key(key(KeyCode::Enter))).is_empty());
        assert_eq!(app.input(), "two");
    }

    #[test]
    fn acknowledged_submit_stays_single_flight_until_a_terminal_chat_event() {
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Key(key(KeyCode::Char('o'))));
        app.reduce(AppEvent::Key(key(KeyCode::Enter)));
        app.reduce(AppEvent::CommandFinished {
            context: ContextId::root(),
            result: CommandResult::Submitted,
        });
        app.reduce(AppEvent::Key(key(KeyCode::Char('n'))));
        app.reduce(AppEvent::Key(key(KeyCode::Enter)));

        assert_eq!(app.input(), "n");
        assert!(app.turn_active());
    }

    #[test]
    fn repeated_character_and_backspace_keys_edit_input() {
        let mut app = App::new(Context::root());

        app.reduce(AppEvent::Key(repeat_key(KeyCode::Char('a'))));
        app.reduce(AppEvent::Key(repeat_key(KeyCode::Char('b'))));
        app.reduce(AppEvent::Key(repeat_key(KeyCode::Backspace)));

        assert_eq!(app.input(), "a");
    }

    #[test]
    fn stale_command_result_cannot_replace_the_active_context() {
        let original = Context::root().with_commands(vec!["/dm".into(), "/status".into()]);
        let mut app = App::new(original.clone());
        app.reduce(AppEvent::Key(key(KeyCode::Char('/'))));
        app.reduce(AppEvent::Key(key(KeyCode::Char('d'))));
        app.reduce(AppEvent::Key(key(KeyCode::Enter)));

        app.reduce(AppEvent::CommandFinished {
            context: ContextId::new("dm:stale"),
            result: CommandResult::Context(
                Context::new(ContextId::new("dm:stale"), "dm · stale")
                    .with_commands(vec!["/dm".into(), "/restart".into()]),
            ),
        });

        assert_eq!(app.context(), &original);
        assert_eq!(
            app.error(),
            Some("ignored stale command result for dm:stale")
        );
    }

    #[test]
    fn matching_context_result_replaces_label_and_commands_atomically() {
        let mut app = App::new(Context::root().with_commands(vec!["/dm".into(), "/status".into()]));
        app.reduce(AppEvent::Key(key(KeyCode::Char('/'))));
        app.reduce(AppEvent::Key(key(KeyCode::Char('d'))));
        app.reduce(AppEvent::Key(key(KeyCode::Enter)));
        app.reduce(AppEvent::Key(key(KeyCode::Enter)));

        app.reduce(AppEvent::CommandFinished {
            context: ContextId::root(),
            result: CommandResult::Context(
                Context::new(ContextId::new("dm:01"), "dm · Ada")
                    .with_commands(vec!["/dm".into(), "/restart".into()]),
            ),
        });

        assert_eq!(app.context().label(), "dm · Ada");
        assert_eq!(app.context().commands(), ["/dm", "/restart"]);
    }

    #[test]
    fn chat_failure_freezes_received_text_and_returns_the_editor_to_idle() {
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Key(key(KeyCode::Char('h'))));
        app.reduce(AppEvent::Key(key(KeyCode::Enter)));
        app.reduce(AppEvent::Chat(ChatEvent::TextDelta("partial".into())));
        app.reduce(AppEvent::Chat(ChatEvent::TurnFailed(
            ChatFailureKind::Protocol,
        )));

        assert!(!app.turn_active());
        assert_eq!(
            app.markdown.completed().to_string(),
            "› h\npartial\nerror: protocol error"
        );
        assert_eq!(app.stream(), "");
    }
    #[test]
    fn completion_and_disconnect_freeze_each_received_stream() {
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Chat(ChatEvent::TextDelta("first".into())));
        app.reduce(AppEvent::Chat(ChatEvent::MessageCompleted(Message {
            id: Default::default(),
            conversation_id: Default::default(),
            sender_type: MemberType::Agent,
            sender_id: "agent".into(),
            body: "first".into(),
            reply_to: None,
            metadata: serde_json::json!({}),
            created_at: "now".into(),
        })));
        app.reduce(AppEvent::Chat(ChatEvent::TurnCompleted));
        app.reduce(AppEvent::Chat(ChatEvent::TextDelta("second".into())));
        app.reduce(AppEvent::Chat(ChatEvent::Disconnected("offline".into())));

        assert!(!app.turn_active());
        assert_eq!(
            app.markdown.completed().to_string(),
            "first\nsecond\nerror: offline"
        );
    }

    #[test]
    fn completed_markdown_block_freezes_while_the_last_block_keeps_streaming() {
        let mut app = App::new(Context::root());

        app.reduce(AppEvent::Chat(ChatEvent::TextDelta(
            "first paragraph\n\nsecond".into(),
        )));

        assert_eq!(app.markdown.completed_len(), 1);
        assert_eq!(app.markdown.completed().to_string(), "first paragraph\n");
        assert!(!app.stream().contains("first paragraph"));
        assert!(app.stream().ends_with("second"));
    }

    #[test]
    fn terminal_event_wins_over_a_bounded_chat_batch() {
        let mut chat = VecDeque::from_iter(
            (0..CHAT_BATCH_LIMIT + 1).map(|_| ChatEvent::TextDelta("x".into())),
        );

        let terminal = AppEvent::Key(key(KeyCode::PageUp));
        assert!(matches!(
            next_event(Some(terminal), &mut chat),
            Some(AppEvent::Key(_))
        ));
        assert_eq!(chat.len(), CHAT_BATCH_LIMIT + 1);

        let Some(AppEvent::ChatBatch(batch)) = next_event(None, &mut chat) else {
            panic!("expected bounded chat batch");
        };
        assert_eq!(batch.len(), CHAT_BATCH_LIMIT);
        assert_eq!(chat.len(), 1);
    }

    #[test]
    fn reducer_never_drops_an_oversized_chat_batch() {
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::ChatBatch(
            (0..CHAT_BATCH_LIMIT + 1)
                .map(|_| ChatEvent::TextDelta("x".into()))
                .collect(),
        ));

        assert_eq!(app.stream(), "x".repeat(CHAT_BATCH_LIMIT + 1));
    }

    #[test]
    fn leading_blanks_still_execute_a_command_and_failures_surface_as_an_error() {
        let mut app = App::new(Context::root());
        for character in "  /thread 01 --agent agent_order".chars() {
            app.reduce(AppEvent::Key(key(KeyCode::Char(character))));
        }

        assert_eq!(
            app.reduce(AppEvent::Key(key(KeyCode::Enter))),
            vec![AppCommand::Execute {
                context: ContextId::root(),
                input: "/thread 01 --agent agent_order".into(),
            }]
        );

        app.reduce(AppEvent::CommandFinished {
            context: ContextId::root(),
            result: CommandResult::Failed("invalid command\n".into()),
        });

        assert_eq!(app.error(), Some("invalid command"));

        app.reduce(AppEvent::Key(key(KeyCode::Char('h'))));
        app.reduce(AppEvent::Key(key(KeyCode::Enter)));

        assert!(app.error().is_none());
    }

    /// Home and End edit the draft; the transcript is paged with PageUp/PageDown and Ctrl+Up/Down.
    ///
    /// July used Home/End for the transcript before the composer owned multi-line editing, where
    /// they have to mean start- and end-of-line.
    #[test]
    fn home_and_end_move_the_caret_rather_than_the_transcript() {
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Resize {
            width: 20,
            height: 10,
        });
        app.reduce(AppEvent::Chat(ChatEvent::TextDelta(
            (0..40).map(|row| format!("{row}  \n")).collect::<String>(),
        )));
        app.reduce(AppEvent::Chat(ChatEvent::TurnCompleted));
        for character in "abc".chars() {
            app.reduce(AppEvent::Key(key(KeyCode::Char(character))));
        }

        app.reduce(AppEvent::Key(key(KeyCode::Home)));
        app.reduce(AppEvent::Key(key(KeyCode::Char('X'))));

        assert_eq!(app.input(), "Xabc");

        app.reduce(AppEvent::Key(key(KeyCode::End)));
        app.reduce(AppEvent::Key(key(KeyCode::Char('Y'))));

        assert_eq!(app.input(), "XabcY");
    }

    #[test]
    fn permission_modal_is_exclusive_and_selects_the_highlighted_option() {
        let mut app = App::new(Context::root());
        let request_id = ChatPermissionRequestId::from("permission-1".to_owned());
        app.reduce(AppEvent::Chat(ChatEvent::PermissionRequested {
            request_id: request_id.clone(),
            prompt: "Write file".into(),
            options: vec![
                PermissionOption {
                    id: "once".into(),
                    label: "Allow once".into(),
                },
                PermissionOption {
                    id: "always".into(),
                    label: "Allow always".into(),
                },
            ],
        }));

        // The prompt is a pane view, so it takes every key: nothing reaches the draft.
        app.reduce(AppEvent::Key(key(KeyCode::Char('x'))));
        app.reduce(AppEvent::Key(key(KeyCode::Down)));

        assert_eq!(app.input(), "");
        assert_eq!(
            app.reduce(AppEvent::Key(key(KeyCode::Enter))),
            vec![AppCommand::RespondPermission {
                request_id,
                outcome: PermissionOutcome::Selected("always".into()),
            }]
        );
        assert!(app.permission().is_none());
        assert!(!app.bottom_pane.has_active_view(), "the prompt closed");
    }

    #[test]
    fn modified_enter_is_ignored_by_the_permission_modal() {
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Chat(ChatEvent::PermissionRequested {
            request_id: ChatPermissionRequestId::from("permission-1".to_owned()),
            prompt: "Write file".into(),
            options: vec![PermissionOption {
                id: "once".into(),
                label: "Allow once".into(),
            }],
        }));

        assert!(
            app.reduce(AppEvent::Key(alt_key(KeyCode::Enter)))
                .is_empty()
        );
        assert!(app.permission().is_some());
    }

    #[test]
    fn escape_rejects_permission_and_empty_options_do_not_trap_the_editor() {
        let mut app = App::new(Context::root());
        let request_id = ChatPermissionRequestId::from("permission-1".to_owned());
        app.reduce(AppEvent::Chat(ChatEvent::PermissionRequested {
            request_id: request_id.clone(),
            prompt: "Write file".into(),
            options: vec![PermissionOption {
                id: "once".into(),
                label: "Allow once".into(),
            }],
        }));

        assert_eq!(
            app.reduce(AppEvent::Key(key(KeyCode::Esc))),
            vec![AppCommand::RespondPermission {
                request_id,
                outcome: PermissionOutcome::Cancelled,
            }]
        );

        let empty_id = ChatPermissionRequestId::from("permission-empty".to_owned());
        assert_eq!(
            app.reduce(AppEvent::Chat(ChatEvent::PermissionRequested {
                request_id: empty_id.clone(),
                prompt: "Write file".into(),
                options: Vec::new(),
            })),
            vec![AppCommand::RespondPermission {
                request_id: empty_id,
                outcome: PermissionOutcome::Cancelled,
            }]
        );
        assert!(app.permission().is_none());
        assert_eq!(app.error(), Some("permission request had no choices"));
    }

    #[test]
    fn escape_outside_permission_does_not_request_exit() {
        let mut app = App::new(Context::root());

        assert!(app.reduce(AppEvent::Key(key(KeyCode::Esc))).is_empty());
        assert!(!app.exit_requested());
    }

    #[test]
    fn active_turn_cancel_is_sent_once_and_delivery_failure_is_retryable() {
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Chat(ChatEvent::TextDelta("working".into())));

        assert_eq!(
            app.reduce(AppEvent::Key(ctrl_key(KeyCode::Char('c')))),
            vec![AppCommand::CancelTurn]
        );
        assert_eq!(app.turn_state(), TurnState::Cancelling);

        app.reduce(AppEvent::CancelFinished(Err("delivery failed".into())));
        assert_eq!(app.turn_state(), TurnState::Active);
        assert_eq!(app.error(), Some("delivery failed"));
        assert_eq!(
            app.reduce(AppEvent::Key(ctrl_key(KeyCode::Char('c')))),
            vec![AppCommand::CancelTurn]
        );
    }

    #[test]
    fn second_ctrl_c_escapes_pending_or_acknowledged_cancel_without_resending() {
        for acknowledge in [false, true] {
            let mut app = App::new(Context::root());
            app.reduce(AppEvent::Chat(ChatEvent::TextDelta("working".into())));
            assert_eq!(
                app.reduce(AppEvent::Key(ctrl_key(KeyCode::Char('c')))),
                vec![AppCommand::CancelTurn]
            );
            if acknowledge {
                app.reduce(AppEvent::CancelFinished(Ok(())));
                assert_eq!(app.turn_state(), TurnState::CancelAcknowledged);
            }

            assert!(
                app.reduce(AppEvent::Key(ctrl_key(KeyCode::Char('c'))))
                    .is_empty()
            );
            assert!(app.exit_requested());
        }
    }

    #[test]
    fn stale_cancel_result_cannot_revive_a_completed_turn() {
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Chat(ChatEvent::TextDelta("working".into())));
        app.reduce(AppEvent::Key(ctrl_key(KeyCode::Char('c'))));
        app.reduce(AppEvent::Chat(ChatEvent::TurnCompleted));

        app.reduce(AppEvent::CancelFinished(Ok(())));

        assert_eq!(app.turn_state(), TurnState::Idle);
        assert!(
            app.reduce(AppEvent::Key(ctrl_key(KeyCode::Char('c'))))
                .is_empty()
        );
        assert!(app.exit_requested());
    }

    #[test]
    fn ctrl_d_does_not_exit_an_active_turn() {
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Chat(ChatEvent::TextDelta("working".into())));

        assert!(
            app.reduce(AppEvent::Key(ctrl_key(KeyCode::Char('d'))))
                .is_empty()
        );
        assert!(!app.exit_requested());
        assert_eq!(app.input(), "");
    }

    #[test]
    fn idle_ctrl_c_clears_input_before_it_exits() {
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Key(key(KeyCode::Char('x'))));

        app.reduce(AppEvent::Key(ctrl_key(KeyCode::Char('c'))));
        assert_eq!(app.input(), "");
        assert!(!app.exit_requested());

        app.reduce(AppEvent::Key(ctrl_key(KeyCode::Char('c'))));
        assert!(app.exit_requested());
    }
}

#[cfg(test)]
mod followup_tests {
    use super::*;

    fn running() -> App {
        let mut app = App::new(Context::new(
            ContextId::new("room:test"),
            "room::test → @order",
        ));
        assert_eq!(app.submit("@order original".into()).len(), 1);
        app
    }

    fn queue(app: &mut App, text: &str, supplement: bool) {
        app.bottom_pane.set_task_running(true);
        app.bottom_pane.set_composer_text(text.into(), vec![]);
        let key = if supplement {
            KeyCode::Tab
        } else {
            KeyCode::Enter
        };
        let commands = app.reduce(AppEvent::Key(KeyEvent::new(key, KeyModifiers::NONE)));
        assert!(commands.is_empty());
    }

    fn acknowledged(app: &mut App) -> Vec<AppCommand> {
        app.reduce(AppEvent::CommandFinished {
            context: app.context.id.clone(),
            result: CommandResult::Submitted,
        })
    }

    fn submitted_text(commands: &[AppCommand]) -> &str {
        assert_eq!(commands.len(), 1, "{commands:?}");
        match &commands[0] {
            AppCommand::Submit { text, .. } => text,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn followup_waits_for_terminal_and_ack_in_either_order() {
        for ack_first in [false, true] {
            let mut app = running();
            queue(&mut app, "more detail", true);
            assert_eq!(app.input(), "");
            let commands = if ack_first {
                assert!(acknowledged(&mut app).is_empty());
                app.reduce(AppEvent::Chat(ChatEvent::TurnCompleted))
            } else {
                assert!(
                    app.reduce(AppEvent::Chat(ChatEvent::TurnCompleted))
                        .is_empty()
                );
                acknowledged(&mut app)
            };
            let text = submitted_text(&commands);
            assert!(text.starts_with("@order Supplement"), "{text}");
            assert!(text.ends_with("more detail"));
            assert!(app.reduce(AppEvent::Tick).is_empty());
        }
    }

    #[test]
    fn followup_enter_and_tab_keep_fifo_and_original_recipient() {
        let mut app = running();
        queue(&mut app, "@pay new task", false);
        queue(&mut app, "extra requirement", true);
        acknowledged(&mut app);
        assert_eq!(
            submitted_text(&app.reduce(AppEvent::Chat(ChatEvent::TurnCompleted))),
            "@pay new task"
        );
        acknowledged(&mut app);
        let next = app.reduce(AppEvent::Chat(ChatEvent::TurnCompleted));
        assert!(submitted_text(&next).starts_with("@order Supplement"));
    }

    #[test]
    fn followup_failure_then_global_completion_holds_until_resume() {
        let mut app = running();
        queue(&mut app, "saved", false);
        acknowledged(&mut app);
        app.reduce(AppEvent::AgentStreamFailed {
            agent: AgentId::new(),
            reason: "startup failed".into(),
        });
        assert!(
            app.reduce(AppEvent::Chat(ChatEvent::TurnCompleted))
                .is_empty()
        );
        assert!(app.reduce(AppEvent::Tick).is_empty());
        let commands = app.submit("/queue resume".into());
        assert_eq!(submitted_text(&commands), "@order saved");
    }

    #[test]
    fn followup_cancel_holds_and_discard_is_local() {
        let mut app = running();
        queue(&mut app, "saved", false);
        acknowledged(&mut app);
        assert_eq!(app.cancel_or_exit(), vec![AppCommand::CancelTurn]);
        app.reduce(AppEvent::CancelFinished(Ok(())));
        assert!(
            app.reduce(AppEvent::Chat(ChatEvent::TurnCompleted))
                .is_empty()
        );
        assert!(app.submit("/queue discard".into()).is_empty());
        assert!(app.submit("/queue resume".into()).is_empty());
    }

    #[test]
    fn followup_pending_paste_is_preserved_and_command_is_restored() {
        let mut app = running();
        app.reduce(AppEvent::Paste("pasted pending text".into()));
        let commands = app.reduce(AppEvent::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        assert!(commands.is_empty());
        assert_eq!(app.input(), "");
        queue(&mut app, " /exit", true);
        assert_eq!(app.input(), "/exit");
        acknowledged(&mut app);
        assert_eq!(
            submitted_text(&app.reduce(AppEvent::Chat(ChatEvent::TurnCompleted))),
            "@order pasted pending text"
        );
    }
}

#[cfg(test)]
mod followup_boundary_tests {
    use super::*;

    #[test]
    fn continuation_followup_targets_continued_agent_even_before_ack() {
        let mut app = App::new(Context::new(ContextId::new("room:a"), "room::a → @order"));
        app.active_recipients = Some("@order".into());
        app.submit("/continue @pay".into());
        assert!(app.enqueue_followup("verify only", true));
        let text = app
            .followups
            .get_mut(&app.context.id)
            .unwrap()
            .pop()
            .unwrap();
        assert!(text.starts_with("@pay Supplement"), "{text}");
    }

    #[test]
    fn large_tab_paste_is_expanded_before_queuing() {
        let mut app = App::new(Context::new(ContextId::new("room:a"), "room::a → @order"));
        app.submit("@order first".into());
        app.reduce(AppEvent::Tick);
        let pasted = "long pasted requirement ".repeat(200);
        app.reduce(AppEvent::Paste(pasted.clone()));
        assert!(
            app.reduce(AppEvent::Key(KeyEvent::new(
                KeyCode::Tab,
                KeyModifiers::NONE
            )))
            .is_empty()
        );
        app.reduce(AppEvent::CommandFinished {
            context: app.context.id.clone(),
            result: CommandResult::Submitted,
        });
        let commands = app.reduce(AppEvent::Chat(ChatEvent::TurnCompleted));
        match commands.as_slice() {
            [AppCommand::Submit { text, .. }] => {
                assert!(text.contains(pasted.trim_end()), "{text}")
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn queued_work_never_crosses_context_and_requires_resume_when_returning() {
        let first = Context::new(ContextId::new("room:a"), "room::a → @order");
        let mut app = App::new(first.clone());
        app.submit("@order first".into());
        assert!(app.enqueue_followup("saved", false));
        let _ = app.set_context(Context::new(ContextId::new("room:b"), "room::b → @pay"));
        app.pending = None;
        app.finish_turn();
        assert!(app.reduce(AppEvent::Tick).is_empty());
        let _ = app.set_context(first);
        assert!(app.reduce(AppEvent::Tick).is_empty());
        assert!(
            matches!(app.submit("/queue resume".into()).as_slice(), [AppCommand::Submit {context, text}] if context == &ContextId::new("room:a") && text == "@order saved")
        );
    }

    #[test]
    fn dispatch_failure_keeps_queue_held_after_late_completion() {
        let mut app = App::new(Context::new(ContextId::new("room:a"), "room::a → @order"));
        app.submit("@order first".into());
        assert!(app.enqueue_followup("saved", false));
        assert!(
            app.reduce(AppEvent::CommandFinished {
                context: app.context.id.clone(),
                result: CommandResult::Failed("unavailable".into())
            })
            .is_empty()
        );
        assert!(
            app.reduce(AppEvent::Chat(ChatEvent::TurnCompleted))
                .is_empty()
        );
        assert_eq!(app.followups[&app.context.id].len(), 1);
    }
}
