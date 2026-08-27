use std::collections::VecDeque;
use std::fmt;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
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
                self.clamp_scroll();
                Vec::new()
            }
            AppEvent::Chat(event) => {
                self.reduce_chat(event);
                Vec::new()
            }
            AppEvent::ChatBatch(events) => {
                for event in events.into_iter().take(CHAT_BATCH_LIMIT) {
                    self.reduce_chat(event);
                }
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
            }
            KeyCode::PageDown => {
                self.scroll_offset = self.scroll_offset.saturating_sub(1);
                self.follow_tail = self.scroll_offset == 0;
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
        if self.pending.is_some() {
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
        self.clamp_scroll();
    }

    fn freeze_stream(&mut self) {
        if !self.stream.is_empty() {
            self.completed_lines.push(std::mem::take(&mut self.stream));
        }
    }

    fn clamp_scroll(&mut self) {
        self.scroll_offset = self
            .scroll_offset
            .min(self.completed_lines.len() + usize::from(!self.stream.is_empty()));
        if self.scroll_offset == 0 {
            self.follow_tail = true;
        }
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

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
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
    fn viewport_scroll_disables_follow_tail_until_end_and_resize_clamps_it() {
        let mut app = App::new(Context::root());

        app.reduce(AppEvent::Resize {
            width: 80,
            height: 24,
        });
        app.reduce(AppEvent::Key(key(KeyCode::PageUp)));

        assert_eq!(app.viewport(), Viewport::new(80, 24));
        assert_eq!(app.scroll_offset(), 1);
        assert!(!app.follow_tail());

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
}
