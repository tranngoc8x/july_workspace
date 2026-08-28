use std::collections::VecDeque;
use std::fmt;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui_textarea::TextArea;

use super::markdown::MarkdownStream;
use crate::application::{ChatEvent, ChatFailureKind, ChatPermissionRequestId};
use crate::domain::{PermissionOption, PermissionOutcome};

pub const CHAT_BATCH_LIMIT: usize = 32;

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
}

impl Context {
    pub fn new(id: ContextId, label: impl Into<String>) -> Self {
        Self {
            id,
            label: label.into(),
        }
    }

    pub fn root() -> Self {
        Self::new(ContextId::root(), "root")
    }

    pub fn id(&self) -> &ContextId {
        &self.id
    }

    pub fn label(&self) -> &str {
        &self.label
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
    Context(Context),
    Output { context: Context, output: String },
    Failed(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum AppEvent {
    Key(KeyEvent),
    Resize {
        width: u16,
        height: u16,
    },
    Tick,
    Chat(ChatEvent),
    ChatBatch(Vec<ChatEvent>),
    CommandFinished {
        context: ContextId,
        result: CommandResult,
    },
    PermissionFinished(Result<(), String>),
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
    status: Option<String>,
    exit_requested: bool,
}

impl App {
    pub fn new(context: Context) -> Self {
        Self {
            context,
            input: TextArea::default(),
            viewport: Viewport::new(0, 0),
            markdown: MarkdownStream::default(),
            scroll_offset: 0,
            follow_tail: true,
            pending: None,
            turn: TurnState::Idle,
            permission: None,
            status: None,
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

    pub fn viewport(&self) -> Viewport {
        self.viewport
    }

    pub fn stream(&self) -> &str {
        self.markdown.tail()
    }

    pub(crate) fn transcript_text(&self) -> Text<'static> {
        self.markdown.text()
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

    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    pub fn exit_requested(&self) -> bool {
        self.exit_requested
    }

    pub fn reduce(&mut self, event: AppEvent) -> Vec<AppCommand> {
        match event {
            AppEvent::Key(key) => self.reduce_key(key),
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
                        self.status = Some(error);
                    }
                }
                Vec::new()
            }
            AppEvent::PermissionFinished(result) => {
                if let Err(error) = result {
                    self.status = Some(error);
                }
                Vec::new()
            }
            AppEvent::Tick => Vec::new(),
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
            KeyCode::PageUp => {
                self.follow_tail = false;
                self.scroll_offset = self.scroll_offset.saturating_add(1);
                let max_scroll = self.max_scroll_offset();
                self.clamp_scroll(max_scroll);
            }
            KeyCode::PageDown => {
                self.scroll_offset = self.scroll_offset.saturating_sub(1);
                let max_scroll = self.max_scroll_offset();
                self.clamp_scroll(max_scroll);
            }
            KeyCode::End => {
                self.scroll_offset = 0;
                self.follow_tail = true;
            }
            KeyCode::Enter if key.modifiers == KeyModifiers::NONE => return self.submit(),
            KeyCode::Esc => self.exit_requested = true,
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.turn == TurnState::Idle && self.input().is_empty() {
                    self.exit_requested = true;
                }
            }
            _ => {
                self.input.input(key);
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
                self.input = TextArea::default();
                Vec::new()
            }
            TurnState::Idle => {
                self.exit_requested = true;
                Vec::new()
            }
        }
    }

    fn submit(&mut self) -> Vec<AppCommand> {
        if self.pending.is_some() || self.turn != TurnState::Idle {
            return Vec::new();
        }

        let text = self.input();
        if text.trim().is_empty() {
            return Vec::new();
        }

        self.input = TextArea::default();
        self.turn = TurnState::Active;
        let context = self.context.id.clone();
        self.pending = Some(context.clone());
        vec![if text.starts_with('/') {
            AppCommand::Execute {
                context,
                input: text,
            }
        } else {
            AppCommand::Submit { context, text }
        }]
    }

    fn reduce_command_result(&mut self, context: ContextId, result: CommandResult) {
        if self.pending.as_ref() != Some(&context) {
            self.status = Some(format!("ignored stale command result for {context}"));
            return;
        }

        self.pending = None;
        match result {
            CommandResult::Submitted => {}
            CommandResult::Context(context) => {
                self.context = context;
                self.turn = TurnState::Idle;
            }
            CommandResult::Output { context, output } => {
                self.context = context;
                self.status = Some(output);
                self.turn = TurnState::Idle;
            }
            CommandResult::Failed(error) => {
                self.status = Some(error);
                self.turn = TurnState::Idle;
            }
        }
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
                    .push_plain(format!("error: {}", failure_label(failure)));
                self.finish_turn();
            }
            ChatEvent::Disconnected(reason) => {
                self.freeze_stream();
                self.markdown.push_plain(format!("error: {reason}"));
                self.finish_turn();
            }
            ChatEvent::PermissionRequested {
                request_id,
                prompt,
                options,
            } => {
                if options.is_empty() {
                    self.status = Some("permission request had no choices".into());
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

    fn clamp_scroll(&mut self, max_scroll: usize) {
        self.scroll_offset = self.scroll_offset.min(max_scroll);
        if self.scroll_offset == 0 {
            self.follow_tail = true;
        }
    }

    fn max_scroll_offset(&self) -> usize {
        self.wrapped_row_count()
            .saturating_sub(usize::from(self.viewport.height.saturating_sub(5)).max(1))
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

    use super::*;
    use crate::application::{ChatEvent, ChatFailureKind, ChatPermissionRequestId};
    use crate::domain::{MemberType, Message, PermissionOption, PermissionOutcome};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn alt_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::ALT)
    }

    fn ctrl_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    fn repeat_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new_with_kind(code, KeyModifiers::NONE, KeyEventKind::Repeat)
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
            assert_eq!(app.status(), None);
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
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Key(key(KeyCode::Char('/'))));
        app.reduce(AppEvent::Key(key(KeyCode::Char('d'))));
        app.reduce(AppEvent::Key(key(KeyCode::Enter)));

        app.reduce(AppEvent::CommandFinished {
            context: ContextId::new("dm:stale"),
            result: CommandResult::Context(Context::new(ContextId::new("dm:stale"), "dm · stale")),
        });

        assert_eq!(app.context(), &Context::root());
        assert_eq!(
            app.status(),
            Some("ignored stale command result for dm:stale")
        );
    }

    #[test]
    fn matching_context_result_replaces_the_label_and_leaves_no_active_turn() {
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Key(key(KeyCode::Char('/'))));
        app.reduce(AppEvent::Key(key(KeyCode::Char('d'))));
        app.reduce(AppEvent::Key(key(KeyCode::Enter)));

        app.reduce(AppEvent::CommandFinished {
            context: ContextId::root(),
            result: CommandResult::Context(Context::new(ContextId::new("dm:01"), "dm · Ada")),
        });

        assert_eq!(app.context().label(), "dm · Ada");
        assert!(!app.turn_active());
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
            "partial\nerror: protocol error"
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
    fn scrolling_clamps_against_wrapped_rows_not_message_count() {
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Resize {
            width: 4,
            height: 7,
        });
        app.reduce(AppEvent::Chat(ChatEvent::TextDelta(
            "1234567890123456".into(),
        )));
        app.reduce(AppEvent::Key(key(KeyCode::PageUp)));
        app.reduce(AppEvent::Key(key(KeyCode::PageUp)));

        assert_eq!(app.scroll_offset(), 2);
        assert!(!app.follow_tail());
    }

    #[test]
    fn wrapped_row_count_matches_paragraph_word_wrapping() {
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Resize {
            width: 5,
            height: 7,
        });
        app.reduce(AppEvent::Chat(ChatEvent::TextDelta("a a a a a a".into())));

        assert_eq!(app.wrapped_row_count(), 2);
    }

    #[test]
    fn scrolling_stops_at_the_top_wrapped_row() {
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Resize {
            width: 4,
            height: 7,
        });
        app.reduce(AppEvent::Chat(ChatEvent::TextDelta(
            "1234567890123456".into(),
        )));
        for _ in 0..10 {
            app.reduce(AppEvent::Key(key(KeyCode::PageUp)));
        }

        assert_eq!(app.scroll_offset(), 2);
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
        app.reduce(AppEvent::Key(key(KeyCode::PageUp)));
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
        assert_eq!(app.status(), Some("permission request had no choices"));
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
        assert_eq!(app.status(), Some("delivery failed"));
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
