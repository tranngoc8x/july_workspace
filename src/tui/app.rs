use std::collections::VecDeque;
use std::fmt;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui_textarea::TextArea;

use crate::application::{ChatEvent, ChatFailureKind};

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
    Submit { context: ContextId, text: String },
    Execute { context: ContextId, input: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandResult {
    Submitted,
    Context(Context),
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
    Exit,
}

pub struct App {
    context: Context,
    input: TextArea<'static>,
    viewport: Viewport,
    completed_lines: Vec<String>,
    stream: String,
    scroll_offset: usize,
    follow_tail: bool,
    pending: Option<ContextId>,
    turn_active: bool,
    status: Option<String>,
    exit_requested: bool,
}

impl App {
    pub fn new(context: Context) -> Self {
        Self {
            context,
            input: TextArea::default(),
            viewport: Viewport::new(0, 0),
            completed_lines: Vec::new(),
            stream: String::new(),
            scroll_offset: 0,
            follow_tail: true,
            pending: None,
            turn_active: false,
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

    pub fn completed_lines(&self) -> &[String] {
        &self.completed_lines
    }

    pub fn stream(&self) -> &str {
        &self.stream
    }

    pub(crate) fn transcript(&self) -> String {
        let mut transcript = self.completed_lines.join("\n");
        if !self.stream.is_empty() {
            if !transcript.is_empty() {
                transcript.push('\n');
            }
            transcript.push_str(&self.stream);
        }
        transcript
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
        self.turn_active
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
                Vec::new()
            }
            AppEvent::Chat(event) => {
                self.reduce_chat_content(std::iter::once(event));
                Vec::new()
            }
            AppEvent::ChatBatch(events) => {
                self.reduce_chat_content(events);
                Vec::new()
            }
            AppEvent::CommandFinished { context, result } => {
                self.reduce_command_result(context, result);
                Vec::new()
            }
            AppEvent::Exit => {
                self.exit_requested = true;
                Vec::new()
            }
            AppEvent::Tick => Vec::new(),
        }
    }

    fn reduce_key(&mut self, key: KeyEvent) -> Vec<AppCommand> {
        if !key.kind.is_press() {
            return Vec::new();
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
            _ => {
                self.input.input(key);
            }
        }
        Vec::new()
    }

    fn submit(&mut self) -> Vec<AppCommand> {
        if self.pending.is_some() || self.turn_active {
            return Vec::new();
        }

        let text = self.input();
        if text.trim().is_empty() {
            return Vec::new();
        }

        self.input = TextArea::default();
        self.turn_active = true;
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
                self.turn_active = false;
            }
            CommandResult::Failed(error) => {
                self.status = Some(error);
                self.turn_active = false;
            }
        }
    }

    fn reduce_chat_content(&mut self, events: impl IntoIterator<Item = ChatEvent>) {
        let old_max_scroll = self.max_scroll_offset();
        let was_following_tail = self.follow_tail;
        for event in events {
            self.reduce_chat(event);
        }
        let new_max_scroll = self.max_scroll_offset();
        if !was_following_tail {
            self.scroll_offset = self
                .scroll_offset
                .saturating_add(new_max_scroll.saturating_sub(old_max_scroll));
        }
        self.clamp_scroll(new_max_scroll);
    }

    fn reduce_chat(&mut self, event: ChatEvent) {
        match event {
            ChatEvent::TextDelta(text) => {
                self.stream.push_str(&text);
                self.turn_active = true;
            }
            ChatEvent::MessageCompleted(_) => self.freeze_stream(),
            ChatEvent::TurnCompleted => {
                self.freeze_stream();
                self.pending = None;
                self.turn_active = false;
            }
            ChatEvent::TurnFailed(failure) => {
                self.freeze_stream();
                self.completed_lines
                    .push(format!("error: {}", failure_label(failure)));
                self.pending = None;
                self.turn_active = false;
            }
            ChatEvent::Disconnected(reason) => {
                self.freeze_stream();
                self.completed_lines.push(format!("error: {reason}"));
                self.pending = None;
                self.turn_active = false;
            }
            ChatEvent::PermissionRequested { .. } => {}
        }
    }

    fn freeze_stream(&mut self) {
        if !self.stream.is_empty() {
            self.completed_lines.push(std::mem::take(&mut self.stream));
        }
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
        Paragraph::new(self.transcript())
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

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::application::{ChatEvent, ChatFailureKind};
    use crate::domain::{MemberType, Message};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn alt_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::ALT)
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
            width: 80,
            height: 24,
        });
        app.reduce(AppEvent::Key(key(KeyCode::PageUp)));

        assert_eq!(app.viewport(), Viewport::new(80, 24));
        assert_eq!(app.scroll_offset(), 0);
        assert!(app.follow_tail());

        app.reduce(AppEvent::Resize {
            width: 40,
            height: 3,
        });
        app.reduce(AppEvent::Key(key(KeyCode::End)));

        assert_eq!(app.viewport(), Viewport::new(40, 3));
        assert_eq!(app.scroll_offset(), 0);
        assert!(app.follow_tail());
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
        assert_eq!(app.completed_lines(), ["partial", "error: protocol error"]);
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
        assert_eq!(app.completed_lines(), ["first", "second", "error: offline"]);
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
}
