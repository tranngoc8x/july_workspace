use std::collections::VecDeque;
use std::fmt;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::style::Style;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui_textarea::{CursorMove, TextArea, WrapMode};

use super::markdown::MarkdownStream;
use super::{CODE_COLOR, COMMAND_OUTPUT_COLOR, ERROR_COLOR, SYSTEM_COLOR, USER_COLOR};
use crate::application::{ChatEvent, ChatFailureKind, ChatPermissionRequestId};
use crate::domain::{PermissionOption, PermissionOutcome};

pub const CHAT_BATCH_LIMIT: usize = 32;
const NON_INPUT_ROWS: u16 = 3;
pub(crate) const INPUT_HORIZONTAL_MARGIN: u16 = 1;
pub(crate) const INPUT_VERTICAL_MARGIN: u16 = 1;

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
    Resize {
        width: u16,
        height: u16,
    },
    /// Wheel or trackpad scroll over the transcript.
    Scroll {
        up: bool,
        rows: u16,
    },
    Tick,
    /// Agent names for `@` completion, sent once when the session opens.
    Agents(Vec<String>),
    /// July Room activation status, never a private runtime transcript.
    RoomStatus(String),
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

#[derive(Clone, Debug, PartialEq)]
pub struct PermissionModal {
    request_id: ChatPermissionRequestId,
    prompt: String,
    options: Vec<PermissionOption>,
    selected: usize,
    scroll: u16,
    follow_selection: bool,
}

impl PermissionModal {
    pub fn prompt(&self) -> &str {
        &self.prompt
    }

    pub fn options(&self) -> &[PermissionOption] {
        &self.options
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn scroll(&self) -> u16 {
        self.scroll
    }

    pub fn follows_selection(&self) -> bool {
        self.follow_selection
    }

    pub(crate) fn max_scroll_page(&self, viewport: Viewport) -> u16 {
        let width = viewport
            .width
            .saturating_sub(4)
            .min(60)
            .saturating_sub(2)
            .max(1);
        let height = viewport
            .height
            .saturating_sub(2)
            .min((self.options.len() as u16).saturating_add(7))
            .max(5)
            .saturating_sub(2)
            .max(1);
        let mut lines = vec![Line::from(self.prompt.clone()), Line::default()];
        lines.extend(
            self.options
                .iter()
                .map(|option| Line::from(format!("  {}", option.label))),
        );
        lines.push(Line::default());
        lines.push(Line::from("Enter choose · Esc reject · Ctrl-C cancel"));
        let rows = Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .line_count(width);
        rows.saturating_sub(usize::from(height))
            .div_ceil(usize::from(height))
            .min(usize::from(u16::MAX)) as u16
    }
}

/// `TextArea` underlines the cursor line by default, which reads as the typed
/// text being underlined; the input keeps a plain line style instead.
fn new_input() -> TextArea<'static> {
    input_with("")
}

fn input_with(text: &str) -> TextArea<'static> {
    let mut input = TextArea::from(text.split('\n'));
    input.set_cursor_line_style(Style::default());
    input.set_wrap_mode(WrapMode::WordOrGlyph);
    input.move_cursor(CursorMove::Bottom);
    input.move_cursor(CursorMove::End);
    input
}

pub struct App {
    context: Context,
    input: TextArea<'static>,
    viewport: Viewport,
    markdown: MarkdownStream,
    scroll_offset: usize,
    follow_tail: bool,
    pending: Option<ContextId>,
    turn: TurnState,
    permission: Option<PermissionModal>,
    error: Option<String>,
    agents: Vec<String>,
    completion_selected: usize,
    prompt_history: Vec<String>,
    history_index: Option<usize>,
    history_draft: Option<String>,
    tick: usize,
    exit_requested: bool,
}

impl App {
    pub fn new(context: Context) -> Self {
        Self {
            context,
            input: new_input(),
            viewport: Viewport::new(0, 0),
            markdown: MarkdownStream::default(),
            scroll_offset: 0,
            follow_tail: true,
            pending: None,
            turn: TurnState::Idle,
            permission: None,
            error: None,
            agents: Vec::new(),
            completion_selected: 0,
            prompt_history: Vec::new(),
            history_index: None,
            history_draft: None,
            tick: 0,
            exit_requested: false,
        }
    }

    pub fn context(&self) -> &Context {
        &self.context
    }

    pub fn input(&self) -> String {
        self.input.lines().join("\n")
    }

    pub(crate) fn input_widget(&self) -> &TextArea<'static> {
        &self.input
    }

    pub(crate) fn input_height(&self) -> u16 {
        let width = self
            .viewport
            .width
            .saturating_sub(INPUT_HORIZONTAL_MARGIN.saturating_mul(2))
            .max(1);
        let visual_rows = Paragraph::new(self.input())
            .wrap(Wrap { trim: false })
            .line_count(width);
        let content_rows = u16::try_from(visual_rows.max(self.input.lines().len()))
            .unwrap_or(u16::MAX)
            .max(1);
        content_rows
            .saturating_add(INPUT_VERTICAL_MARGIN.saturating_mul(2))
            .min(self.viewport.height.saturating_sub(NON_INPUT_ROWS).max(1))
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
        // ponytail: terminal cells have no line-height; one spacer row between
        // consecutive non-empty lines is the closest equivalent.
        let text = self.markdown.text();
        let mut lines = Vec::with_capacity(text.lines.len() * 2);
        let mut rest = text.lines.into_iter().peekable();
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

    pub(crate) fn transcript_scroll(&self) -> u16 {
        self.max_scroll_offset()
            .saturating_sub(self.scroll_offset)
            .min(u16::MAX.into()) as u16
    }

    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    pub fn follow_tail(&self) -> bool {
        self.follow_tail
    }

    pub fn turn_active(&self) -> bool {
        self.turn != TurnState::Idle
    }

    pub fn turn_state(&self) -> TurnState {
        self.turn
    }

    pub fn permission(&self) -> Option<&PermissionModal> {
        self.permission.as_ref()
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

    pub fn reduce(&mut self, event: AppEvent) -> Vec<AppCommand> {
        match event {
            AppEvent::Key(key) => {
                let old_input_height = self.input_height();
                let old_max_scroll = self.max_scroll_offset();
                let was_following_tail = self.follow_tail;
                let commands = self.reduce_key(key);
                if self.input_height() != old_input_height {
                    let new_max_scroll = self.max_scroll_offset();
                    if !was_following_tail {
                        self.scroll_offset = if new_max_scroll >= old_max_scroll {
                            self.scroll_offset
                                .saturating_add(new_max_scroll - old_max_scroll)
                        } else {
                            self.scroll_offset
                                .saturating_sub(old_max_scroll - new_max_scroll)
                        };
                    }
                    self.clamp_scroll(new_max_scroll);
                }
                commands
            }
            AppEvent::Resize { width, height } => {
                self.viewport = Viewport::new(width, height);
                let max_scroll = self.max_scroll_offset();
                self.clamp_scroll(max_scroll);
                if let Some(max_page) = self
                    .permission
                    .as_ref()
                    .map(|permission| permission.max_scroll_page(self.viewport))
                {
                    let permission = self.permission.as_mut().unwrap();
                    permission.scroll = permission.scroll.min(max_page);
                }
                Vec::new()
            }
            AppEvent::Scroll { up, rows } => {
                self.scroll_by(usize::from(rows), up);
                Vec::new()
            }
            AppEvent::Agents(agents) => {
                self.agents = agents;
                self.completion_selected = 0;
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
                if self
                    .permission
                    .as_ref()
                    .is_some_and(|modal| modal.request_id == request_id)
                {
                    self.permission = None;
                }
                Vec::new()
            }
            AppEvent::PermissionFinished(result) => {
                if let Err(error) = result {
                    self.error = Some(error);
                }
                Vec::new()
            }
            AppEvent::Tick => {
                self.tick = self.tick.wrapping_add(1);
                Vec::new()
            }
        }
    }

    fn reduce_key(&mut self, key: KeyEvent) -> Vec<AppCommand> {
        if key.kind == KeyEventKind::Release {
            return Vec::new();
        }

        if self.permission.is_some() {
            return self.reduce_permission_key(key);
        }

        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return self.cancel_or_exit();
        }

        match key.code {
            KeyCode::PageUp => self.scroll_by(self.transcript_rows(), true),
            KeyCode::PageDown => self.scroll_by(self.transcript_rows(), false),
            KeyCode::Up if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll_by(1, true);
            }
            KeyCode::Down if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll_by(1, false);
            }
            KeyCode::Up if key.modifiers == KeyModifiers::NONE => {
                if !self.move_completion(false) {
                    let cursor = self.input.cursor();
                    self.input.input(key);
                    if self.input.cursor() == cursor {
                        self.history_up();
                    }
                }
            }
            KeyCode::Down if key.modifiers == KeyModifiers::NONE => {
                if !self.move_completion(true) {
                    let cursor = self.input.cursor();
                    self.input.input(key);
                    if self.input.cursor() == cursor {
                        self.history_down();
                    }
                }
            }
            KeyCode::Home => self.scroll_by(usize::MAX, true),
            KeyCode::End => {
                self.scroll_offset = 0;
                self.follow_tail = true;
            }
            KeyCode::Tab if key.modifiers == KeyModifiers::NONE => {
                self.complete();
            }
            KeyCode::Enter if key.modifiers == KeyModifiers::NONE => {
                if !self.complete() {
                    return self.submit();
                }
            }
            KeyCode::Enter => {
                self.input.insert_newline();
                self.completion_selected = 0;
                self.history_index = None;
                self.history_draft = None;
            }
            KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.input.insert_newline();
                self.completion_selected = 0;
                self.history_index = None;
                self.history_draft = None;
            }
            KeyCode::Esc => {}
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.turn == TurnState::Idle && self.input().is_empty() {
                    self.exit_requested = true;
                }
            }
            _ => {
                let before = self.input();
                self.input.input(key);
                if self.input() != before {
                    self.completion_selected = 0;
                    self.history_index = None;
                    self.history_draft = None;
                }
            }
        }
        Vec::new()
    }

    fn reduce_permission_key(&mut self, key: KeyEvent) -> Vec<AppCommand> {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return self.cancel_or_exit();
        }

        let max_scroll_page = self
            .permission
            .as_ref()
            .unwrap()
            .max_scroll_page(self.viewport);
        let permission = self.permission.as_mut().unwrap();
        match key.code {
            KeyCode::Up => {
                permission.selected = permission.selected.saturating_sub(1);
                permission.follow_selection = true;
            }
            KeyCode::Down => {
                permission.selected =
                    (permission.selected + 1).min(permission.options.len().saturating_sub(1));
                permission.follow_selection = true;
            }
            KeyCode::PageUp => {
                permission.scroll = permission.scroll.saturating_sub(1);
                permission.follow_selection = false;
            }
            KeyCode::PageDown => {
                permission.scroll = permission.scroll.saturating_add(1).min(max_scroll_page);
                permission.follow_selection = false;
            }
            KeyCode::Enter if key.modifiers == KeyModifiers::NONE => {
                let permission = self.permission.take().unwrap();
                return vec![AppCommand::RespondPermission {
                    request_id: permission.request_id,
                    outcome: PermissionOutcome::Selected(
                        permission.options[permission.selected].id.clone(),
                    ),
                }];
            }
            KeyCode::Esc => {
                let permission = self.permission.take().unwrap();
                return vec![AppCommand::RespondPermission {
                    request_id: permission.request_id,
                    outcome: PermissionOutcome::Cancelled,
                }];
            }
            _ => {}
        }
        Vec::new()
    }

    fn cancel_or_exit(&mut self) -> Vec<AppCommand> {
        match self.turn {
            TurnState::Active => {
                self.turn = TurnState::Cancelling;
                vec![AppCommand::CancelTurn]
            }
            TurnState::Cancelling | TurnState::CancelAcknowledged => {
                self.exit_requested = true;
                Vec::new()
            }
            TurnState::Idle if !self.input().is_empty() => {
                self.input = new_input();
                self.history_index = None;
                self.history_draft = None;
                Vec::new()
            }
            TurnState::Idle => {
                self.exit_requested = true;
                Vec::new()
            }
        }
    }

    /// The `@` prefix being typed at the end of the input, if any.
    /// ponytail: completion follows the caret only at the end of the input,
    /// which is where mentions are typed; mid-line editing skips it.
    fn mention_prefix(&self) -> Option<String> {
        if !self.cursor_is_at_input_end() {
            return None;
        }
        let input = self.input();
        let word = input.split_whitespace().next_back()?;
        if !input.ends_with(word) {
            return None;
        }
        word.strip_prefix('@').map(str::to_owned)
    }

    fn command_prefix(&self) -> Option<String> {
        if !self.cursor_is_at_input_end() {
            return None;
        }
        let input = self.input();
        if input.contains('\n') {
            return None;
        }
        let prefix = input.trim_start();
        if !prefix.starts_with('/')
            || (prefix.chars().next_back().is_some_and(char::is_whitespace)
                && self
                    .context
                    .commands()
                    .iter()
                    .any(|name| name == prefix.trim_end()))
        {
            return None;
        }
        Some(prefix.to_owned())
    }

    fn cursor_is_at_input_end(&self) -> bool {
        let lines = self.input.lines();
        let Some(last) = lines.last() else {
            return false;
        };
        self.input.cursor() == (lines.len() - 1, last.chars().count())
    }

    fn active_completion(&self) -> Option<(String, Vec<&str>)> {
        if self.error.is_some() || self.permission.is_some() {
            return None;
        }
        if let Some(prefix) = self.command_prefix() {
            let mut matches: Vec<_> = self
                .context
                .commands()
                .iter()
                .map(String::as_str)
                .filter(|name| name.starts_with(&prefix))
                .collect();
            if let Some(exact) = matches.iter().position(|name| *name == prefix) {
                matches.swap(0, exact);
            }
            if !matches.is_empty() {
                return Some((prefix, matches));
            }
        }

        let prefix = self.mention_prefix()?;
        let matches: Vec<_> = self
            .agents
            .iter()
            .filter(|agent| agent.starts_with(&prefix) && agent.len() > prefix.len())
            .map(String::as_str)
            .collect();
        (!matches.is_empty()).then_some((prefix, matches))
    }

    /// Candidate names for the completion list.
    pub fn completions(&self) -> Vec<&str> {
        self.active_completion()
            .map(|(_, matches)| matches)
            .unwrap_or_default()
    }

    pub(crate) fn completion_selected(&self) -> usize {
        self.completion_selected
            .min(self.completions().len().saturating_sub(1))
    }

    pub(crate) fn completion_is_mention(&self) -> bool {
        self.mention_prefix().is_some()
    }

    fn move_completion(&mut self, down: bool) -> bool {
        let count = self.completions().len();
        if count == 0 {
            return false;
        }
        let selected = self.completion_selected();
        self.completion_selected = if down {
            (selected + 1).min(count - 1)
        } else {
            selected.saturating_sub(1)
        };
        true
    }

    /// Complete the selected slash command or `@` mention candidate.
    /// Returns whether candidates were active.
    fn complete(&mut self) -> bool {
        let Some((prefix, matches)) = self.active_completion() else {
            return false;
        };
        let selected = matches[self.completion_selected.min(matches.len() - 1)];
        let suffix = selected[prefix.len()..].to_owned();
        if !suffix.is_empty() {
            self.input.insert_str(suffix);
        }
        self.input.insert_str(" ");
        self.completion_selected = 0;
        true
    }

    fn history_up(&mut self) {
        let Some(last) = self.prompt_history.len().checked_sub(1) else {
            return;
        };
        let index = match self.history_index {
            Some(index) => index.saturating_sub(1),
            None => {
                self.history_draft = Some(self.input());
                last
            }
        };
        self.history_index = Some(index);
        self.input = input_with(&self.prompt_history[index]);
        self.completion_selected = 0;
    }

    fn history_down(&mut self) {
        let Some(index) = self.history_index else {
            return;
        };
        if index + 1 < self.prompt_history.len() {
            self.history_index = Some(index + 1);
            self.input = input_with(&self.prompt_history[index + 1]);
        } else {
            self.history_index = None;
            self.input = input_with(self.history_draft.take().as_deref().unwrap_or(""));
        }
        self.completion_selected = 0;
    }

    fn submit(&mut self) -> Vec<AppCommand> {
        if self.pending.is_some() || self.turn != TurnState::Idle {
            return Vec::new();
        }

        let text = self.input();
        if text.trim().is_empty() {
            return Vec::new();
        }
        self.error = None;
        // Leading blanks must not turn a command into chat.
        let command = text.trim_start().starts_with('/');

        self.prompt_history.push(text.clone());
        self.history_index = None;
        self.history_draft = None;
        self.input = new_input();
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
                self.context = context;
                self.turn = TurnState::Idle;
            }
            CommandResult::ContextWithHistory(snapshot) => {
                self.error = self.apply_snapshot(snapshot);
                self.turn = TurnState::Idle;
            }
            CommandResult::Output { context, output } => {
                self.context = context;
                // Command output belongs in the transcript: it can be long,
                // and the footer is one line.
                self.freeze_stream();
                self.markdown.push_plain(output, COMMAND_OUTPUT_COLOR);
                self.turn = TurnState::Idle;
            }
            CommandResult::Failed(error) => {
                self.error = Some(error.trim_end().to_owned());
                self.turn = TurnState::Idle;
            }
            CommandResult::FailedInContext { error, snapshot } => {
                let _ = self.apply_snapshot(snapshot);
                self.error = Some(error.trim_end().to_owned());
                self.turn = TurnState::Idle;
            }
        }
    }

    fn apply_snapshot(&mut self, snapshot: ContextSnapshot) -> Option<String> {
        self.context = snapshot.context;
        self.markdown = MarkdownStream::default();
        self.scroll_offset = 0;
        self.follow_tail = true;

        match snapshot.history {
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
        }
    }

    fn push_history_entry(&mut self, entry: &HistoryEntry) {
        match entry.author {
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
        let old_max_scroll = self.max_scroll_offset();
        let was_following_tail = self.follow_tail;
        let mut commands = Vec::new();
        for event in events {
            commands.extend(self.reduce_chat(event));
        }
        let new_max_scroll = self.max_scroll_offset();
        if !was_following_tail {
            self.scroll_offset = if new_max_scroll >= old_max_scroll {
                self.scroll_offset
                    .saturating_add(new_max_scroll - old_max_scroll)
            } else {
                self.scroll_offset
                    .saturating_sub(old_max_scroll - new_max_scroll)
            };
        }
        self.clamp_scroll(new_max_scroll);
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
                self.freeze_stream();
                self.markdown
                    .push_plain(format!("error: {}", failure_label(failure)), ERROR_COLOR);
                self.finish_turn();
            }
            ChatEvent::Disconnected(reason) => {
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
                self.permission = Some(PermissionModal {
                    request_id,
                    prompt,
                    options,
                    selected: 0,
                    scroll: 0,
                    follow_selection: false,
                });
            }
        }
        Vec::new()
    }

    fn finish_turn(&mut self) {
        self.turn = TurnState::Idle;
        self.permission = None;
    }

    fn freeze_stream(&mut self) {
        self.markdown.finish();
    }

    fn scroll_by(&mut self, rows: usize, up: bool) {
        if up {
            self.follow_tail = false;
            self.scroll_offset = self.scroll_offset.saturating_add(rows);
        } else {
            self.scroll_offset = self.scroll_offset.saturating_sub(rows);
        }
        let max_scroll = self.max_scroll_offset();
        self.clamp_scroll(max_scroll);
    }

    /// Rows the transcript pane owns: the frame minus header, input and hints.
    fn transcript_rows(&self) -> usize {
        usize::from(
            self.viewport
                .height
                .saturating_sub(self.input_height().saturating_add(2)),
        )
        .max(1)
    }

    fn clamp_scroll(&mut self, max_scroll: usize) {
        self.scroll_offset = self.scroll_offset.min(max_scroll);
        if self.scroll_offset == 0 {
            self.follow_tail = true;
        }
    }

    fn max_scroll_offset(&self) -> usize {
        self.wrapped_row_count()
            .saturating_sub(self.transcript_rows())
    }

    fn wrapped_row_count(&self) -> usize {
        Paragraph::new(self.transcript_text())
            .wrap(Wrap { trim: false })
            .line_count(self.viewport.width.max(1))
    }
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

    fn app_with_commands(commands: &[&str]) -> App {
        App::new(
            Context::root().with_commands(commands.iter().map(|name| (*name).into()).collect()),
        )
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
        assert!(app.permission.is_some());
        app.reduce(AppEvent::RoomPermissionDismissed(
            "binding:new".to_owned().into(),
        ));
        assert!(app.permission.is_none());
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
    fn arrow_keys_recall_submitted_prompts_and_restore_the_draft() {
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
        for character in "draft".chars() {
            app.reduce(AppEvent::Key(key(KeyCode::Char(character))));
        }

        app.reduce(AppEvent::Key(key(KeyCode::Up)));
        assert_eq!(app.input(), "second");
        app.reduce(AppEvent::Key(key(KeyCode::Up)));
        assert_eq!(app.input(), "first");
        app.reduce(AppEvent::Key(key(KeyCode::Down)));
        assert_eq!(app.input(), "second");
        app.reduce(AppEvent::Key(key(KeyCode::Down)));
        assert_eq!(app.input(), "draft");
        app.reduce(AppEvent::Key(key(KeyCode::Char('!'))));
        assert_eq!(app.input(), "draft!");
        app.reduce(AppEvent::Key(key(KeyCode::Up)));
        app.reduce(AppEvent::Key(key(KeyCode::Char('?'))));
        assert_eq!(app.input(), "second?");
    }

    #[test]
    fn recalled_prompt_resets_completion_selection() {
        let mut app = app_with_commands(&["/start", "/status"]);
        app.prompt_history.push("/st".into());
        for character in "/st".chars() {
            app.reduce(AppEvent::Key(key(KeyCode::Char(character))));
        }
        app.reduce(AppEvent::Key(key(KeyCode::Down)));
        app.reduce(AppEvent::Key(ctrl_key(KeyCode::Char('c'))));

        app.reduce(AppEvent::Key(key(KeyCode::Up)));

        assert_eq!(app.input(), "/st");
        assert_eq!(app.completion_selected(), 0);
    }

    #[test]
    fn arrow_keys_move_the_multiline_cursor_before_opening_history() {
        let mut app = App::new(Context::root());
        for character in "saved".chars() {
            app.reduce(AppEvent::Key(key(KeyCode::Char(character))));
        }
        app.reduce(AppEvent::Key(key(KeyCode::Enter)));
        app.reduce(AppEvent::CommandFinished {
            context: ContextId::root(),
            result: CommandResult::Submitted,
        });
        app.reduce(AppEvent::Chat(ChatEvent::TurnCompleted));
        for character in "top".chars() {
            app.reduce(AppEvent::Key(key(KeyCode::Char(character))));
        }
        app.reduce(AppEvent::Key(alt_key(KeyCode::Enter)));
        for character in "bottom".chars() {
            app.reduce(AppEvent::Key(key(KeyCode::Char(character))));
        }

        app.reduce(AppEvent::Key(key(KeyCode::Up)));
        assert_eq!(app.input(), "top\nbottom");
        assert_eq!(app.input.cursor().0, 0);
        app.reduce(AppEvent::Key(key(KeyCode::Down)));
        assert_eq!(app.input.cursor().0, 1);
        app.reduce(AppEvent::Key(key(KeyCode::Up)));
        app.reduce(AppEvent::Key(key(KeyCode::Up)));
        assert_eq!(app.input(), "saved");
        app.reduce(AppEvent::Key(key(KeyCode::Down)));
        assert_eq!(app.input(), "top\nbottom");
    }

    #[test]
    fn viewport_scroll_disables_follow_tail_until_end_and_resize_clamps_it() {
        let mut app = App::new(Context::root());

        app.reduce(AppEvent::Resize {
            width: 5,
            height: 8,
        });
        app.reduce(AppEvent::Chat(ChatEvent::TextDelta(
            "long wrapped content keeps the transcript taller than the viewport".into(),
        )));
        app.reduce(AppEvent::Key(key(KeyCode::PageUp)));

        assert_eq!(app.viewport(), Viewport::new(5, 8));
        assert!(app.scroll_offset() > 0);
        assert!(!app.follow_tail());

        app.reduce(AppEvent::Resize {
            width: 80,
            height: 24,
        });

        assert_eq!(app.viewport(), Viewport::new(80, 24));
        assert_eq!(app.scroll_offset(), 0);
        assert!(app.follow_tail());
    }

    #[test]
    fn shrinking_input_clamps_the_transcript_scroll() {
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Resize {
            width: 10,
            height: 10,
        });
        app.reduce(AppEvent::Chat(ChatEvent::TextDelta(
            "one two three four five six seven eight nine ten".into(),
        )));
        app.reduce(AppEvent::Chat(ChatEvent::TurnCompleted));
        for _ in 0..4 {
            app.reduce(AppEvent::Key(alt_key(KeyCode::Enter)));
        }
        app.reduce(AppEvent::Key(key(KeyCode::Home)));

        app.reduce(AppEvent::Key(ctrl_key(KeyCode::Char('c'))));

        assert_eq!(app.scroll_offset(), app.max_scroll_offset());
    }

    #[test]
    fn growing_input_preserves_the_manually_scrolled_transcript_row() {
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Resize {
            width: 20,
            height: 10,
        });
        app.reduce(AppEvent::Chat(ChatEvent::TextDelta(
            (0..20).map(|row| format!("{row}  \n")).collect(),
        )));
        app.reduce(AppEvent::Chat(ChatEvent::TurnCompleted));
        app.reduce(AppEvent::Scroll { up: true, rows: 2 });
        let visible_row = app.transcript_scroll();

        app.reduce(AppEvent::Key(alt_key(KeyCode::Enter)));

        assert_eq!(app.transcript_scroll(), visible_row);
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
    fn tab_completes_the_selected_agent_mention() {
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Agents(vec![
            "cashpoint".into(),
            "cashflow".into(),
            "pay".into(),
        ]));

        // Nothing to complete until an `@` is being typed.
        for character in "hello ".chars() {
            app.reduce(AppEvent::Key(key(KeyCode::Char(character))));
        }
        assert!(app.completions().is_empty());
        app.reduce(AppEvent::Key(key(KeyCode::Tab)));
        assert_eq!(app.input(), "hello ");

        // Several matches: the first candidate is selected by default.
        for character in "@cash".chars() {
            app.reduce(AppEvent::Key(key(KeyCode::Char(character))));
        }
        assert_eq!(app.completions(), ["cashpoint", "cashflow"]);
        app.reduce(AppEvent::Key(key(KeyCode::Tab)));
        assert_eq!(app.input(), "hello @cashpoint ");
        assert!(app.completions().is_empty());
    }

    #[test]
    fn unique_command_uses_first_enter_to_complete_and_second_to_submit() {
        let mut app = app_with_commands(&["/status", "/start"]);
        for character in "/stat".chars() {
            app.reduce(AppEvent::Key(key(KeyCode::Char(character))));
        }

        assert!(app.reduce(AppEvent::Key(key(KeyCode::Enter))).is_empty());
        assert_eq!(app.input(), "/status ");
        assert!(app.completions().is_empty());
        assert_eq!(
            app.reduce(AppEvent::Key(key(KeyCode::Enter))),
            vec![AppCommand::Execute {
                context: ContextId::root(),
                input: "/status ".into(),
            }]
        );
    }

    #[test]
    fn enter_completes_the_default_command_candidate() {
        let mut app = app_with_commands(&["/status", "/start"]);
        for character in "/sta".chars() {
            app.reduce(AppEvent::Key(key(KeyCode::Char(character))));
        }

        assert!(app.reduce(AppEvent::Key(key(KeyCode::Enter))).is_empty());
        assert_eq!(app.input(), "/status ");
        assert!(app.completions().is_empty());
        assert!(!app.turn_active());
    }

    #[test]
    fn down_selects_the_next_completion_for_enter() {
        let mut app = app_with_commands(&["/start", "/status"]);
        for character in "/st".chars() {
            app.reduce(AppEvent::Key(key(KeyCode::Char(character))));
        }

        app.reduce(AppEvent::Key(key(KeyCode::Down)));
        assert!(app.reduce(AppEvent::Key(key(KeyCode::Enter))).is_empty());

        assert_eq!(app.input(), "/status ");
    }

    #[test]
    fn error_hides_completion_and_enter_submits_the_typed_input() {
        let mut app = app_with_commands(&["/dm"]);
        app.reduce(AppEvent::Key(key(KeyCode::Char('x'))));
        assert_eq!(app.reduce(AppEvent::Key(key(KeyCode::Enter))).len(), 1);
        app.reduce(AppEvent::CommandFinished {
            context: ContextId::root(),
            result: CommandResult::Failed("boom".into()),
        });
        for character in "/d".chars() {
            app.reduce(AppEvent::Key(key(KeyCode::Char(character))));
        }

        assert!(app.completions().is_empty());
        assert_eq!(
            app.reduce(AppEvent::Key(key(KeyCode::Enter))),
            vec![AppCommand::Execute {
                context: ContextId::root(),
                input: "/d".into(),
            }]
        );
    }

    #[test]
    fn tab_completes_the_default_command_candidate() {
        let mut app = app_with_commands(&["/status", "/start"]);
        for character in "/st".chars() {
            app.reduce(AppEvent::Key(key(KeyCode::Char(character))));
        }
        app.reduce(AppEvent::Key(key(KeyCode::Tab)));
        assert_eq!(app.input(), "/status ");
    }

    #[test]
    fn slash_completion_preserves_leading_blanks_and_supports_multiword_names() {
        let mut leading = app_with_commands(&["/status"]);
        for character in "  /stat".chars() {
            leading.reduce(AppEvent::Key(key(KeyCode::Char(character))));
        }
        leading.reduce(AppEvent::Key(key(KeyCode::Tab)));
        assert_eq!(leading.input(), "  /status ");

        let mut multiword = app_with_commands(&["/work new"]);
        for character in "/work n".chars() {
            multiword.reduce(AppEvent::Key(key(KeyCode::Char(character))));
        }
        multiword.reduce(AppEvent::Key(key(KeyCode::Tab)));
        assert_eq!(multiword.input(), "/work new ");
    }

    #[test]
    fn slash_completion_ignores_arguments_and_multiline_input() {
        let mut argument = app_with_commands(&["/status"]);
        for character in "/status argument".chars() {
            argument.reduce(AppEvent::Key(key(KeyCode::Char(character))));
        }
        assert!(argument.completions().is_empty());

        let mut multiline = app_with_commands(&["/status"]);
        for character in "/stat".chars() {
            multiline.reduce(AppEvent::Key(key(KeyCode::Char(character))));
        }
        multiline.reduce(AppEvent::Key(alt_key(KeyCode::Enter)));
        assert!(multiline.completions().is_empty());
    }

    #[test]
    fn exact_command_enter_appends_space_before_submit() {
        let mut app = app_with_commands(&["/status"]);
        for character in "/status".chars() {
            app.reduce(AppEvent::Key(key(KeyCode::Char(character))));
        }
        assert!(app.reduce(AppEvent::Key(key(KeyCode::Enter))).is_empty());
        assert_eq!(app.input(), "/status ");
        assert_eq!(app.reduce(AppEvent::Key(key(KeyCode::Enter))).len(), 1);
    }

    #[test]
    fn exact_command_beats_a_longer_command_for_tab_and_enter() {
        let commands = ["/thread", "/thread new"];
        let mut tab = app_with_commands(&commands);
        for character in "/thread".chars() {
            tab.reduce(AppEvent::Key(key(KeyCode::Char(character))));
        }
        tab.reduce(AppEvent::Key(key(KeyCode::Tab)));
        assert_eq!(tab.input(), "/thread ");
        assert!(tab.completions().is_empty());

        let mut enter = app_with_commands(&commands);
        for character in "/thread".chars() {
            enter.reduce(AppEvent::Key(key(KeyCode::Char(character))));
        }
        assert!(enter.reduce(AppEvent::Key(key(KeyCode::Enter))).is_empty());
        assert_eq!(enter.input(), "/thread ");
        assert_eq!(
            enter.reduce(AppEvent::Key(key(KeyCode::Enter))),
            vec![AppCommand::Execute {
                context: ContextId::root(),
                input: "/thread ".into(),
            }]
        );
    }

    #[test]
    fn slash_completion_is_inactive_away_from_the_input_end() {
        let mut app = app_with_commands(&["/status"]);
        for character in "/stat".chars() {
            app.reduce(AppEvent::Key(key(KeyCode::Char(character))));
        }
        app.reduce(AppEvent::Key(key(KeyCode::Left)));

        assert!(app.completions().is_empty());
        app.reduce(AppEvent::Key(key(KeyCode::Tab)));
        assert_eq!(app.input(), "/stat");
    }

    #[test]
    fn mention_completion_is_inactive_away_from_the_input_end() {
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Agents(vec!["cashflow".into()]));
        for character in "@cashf".chars() {
            app.reduce(AppEvent::Key(key(KeyCode::Char(character))));
        }
        app.reduce(AppEvent::Key(key(KeyCode::Left)));

        assert!(app.completions().is_empty());
        app.reduce(AppEvent::Key(key(KeyCode::Tab)));
        assert_eq!(app.input(), "@cashf");
    }

    #[test]
    fn enter_completes_agent_mention() {
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Agents(vec![
            "cashpoint".into(),
            "cashflow".into(),
        ]));
        for character in "@cashf".chars() {
            app.reduce(AppEvent::Key(key(KeyCode::Char(character))));
        }
        assert!(app.reduce(AppEvent::Key(key(KeyCode::Enter))).is_empty());
        assert_eq!(app.input(), "@cashflow ");
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
    fn scrolling_clamps_against_wrapped_rows_not_message_count() {
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Resize {
            width: 4,
            height: 8,
        });
        app.reduce(AppEvent::Chat(ChatEvent::TextDelta(
            "1234567890123456".into(),
        )));
        for _ in 0..10 {
            app.reduce(AppEvent::Key(key(KeyCode::PageUp)));
        }

        assert_eq!(app.scroll_offset(), app.max_scroll_offset());
        assert!(!app.follow_tail());
    }

    #[test]
    fn leading_blanks_still_execute_a_command_and_failures_surface_as_an_error() {
        let mut app = App::new(Context::root());
        for character in "  /thread 01 --agent cashpoint".chars() {
            app.reduce(AppEvent::Key(key(KeyCode::Char(character))));
        }

        assert_eq!(
            app.reduce(AppEvent::Key(key(KeyCode::Enter))),
            vec![AppCommand::Execute {
                context: ContextId::root(),
                input: "/thread 01 --agent cashpoint".into(),
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

    #[test]
    fn page_keys_move_a_transcript_page_and_the_wheel_moves_its_rows() {
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Resize {
            width: 20,
            height: 10,
        });
        app.reduce(AppEvent::Chat(ChatEvent::TextDelta(
            (0..40).map(|row| format!("{row}  \n")).collect::<String>(),
        )));
        app.reduce(AppEvent::Chat(ChatEvent::TurnCompleted));

        app.reduce(AppEvent::Key(key(KeyCode::PageUp)));
        assert_eq!(app.scroll_offset(), 5);
        assert!(!app.follow_tail());

        app.reduce(AppEvent::Scroll { up: true, rows: 3 });
        assert_eq!(app.scroll_offset(), 8);

        app.reduce(AppEvent::Key(key(KeyCode::PageDown)));
        assert_eq!(app.scroll_offset(), 3);

        app.reduce(AppEvent::Key(key(KeyCode::Home)));
        assert_eq!(app.scroll_offset(), app.max_scroll_offset());

        app.reduce(AppEvent::Key(key(KeyCode::End)));
        assert_eq!(app.scroll_offset(), 0);
        assert!(app.follow_tail());
    }

    #[test]
    fn wrapped_row_count_matches_paragraph_word_wrapping() {
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Resize {
            width: 5,
            height: 7,
        });
        app.reduce(AppEvent::Chat(ChatEvent::TextDelta("a a a a a a".into())));
        app.reduce(AppEvent::Chat(ChatEvent::TurnCompleted));

        assert_eq!(app.wrapped_row_count(), 2);
    }

    #[test]
    fn scrolling_stops_at_the_top_wrapped_row() {
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Resize {
            width: 4,
            height: 6,
        });
        app.reduce(AppEvent::Chat(ChatEvent::TextDelta(
            "1234567890123456".into(),
        )));
        app.reduce(AppEvent::Chat(ChatEvent::TurnCompleted));
        for _ in 0..10 {
            app.reduce(AppEvent::Key(key(KeyCode::PageUp)));
        }

        assert_eq!(app.scroll_offset(), app.max_scroll_offset());
        assert_eq!(app.scroll_offset(), 3);
    }

    #[test]
    fn shrinking_markdown_tail_preserves_the_manually_scrolled_row() {
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Resize {
            width: 6,
            height: 7,
        });
        app.reduce(AppEvent::Chat(ChatEvent::TextDelta(
            "0  \n1  \n1234 **x".into(),
        )));
        app.reduce(AppEvent::Key(ctrl_key(KeyCode::Up)));
        assert_eq!(app.scroll_offset(), 1);

        app.reduce(AppEvent::Chat(ChatEvent::TextDelta("**".into())));

        assert_eq!(app.scroll_offset(), 0);
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

        app.reduce(AppEvent::Key(key(KeyCode::Char('x'))));
        app.reduce(AppEvent::Key(key(KeyCode::Down)));

        assert_eq!(app.input(), "");
        assert_eq!(app.permission().unwrap().selected(), 1);
        assert_eq!(
            app.reduce(AppEvent::Key(key(KeyCode::Enter))),
            vec![AppCommand::RespondPermission {
                request_id,
                outcome: PermissionOutcome::Selected("always".into()),
            }]
        );
        assert!(app.permission().is_none());
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
