//! The composer and everything that can take the bottom of the screen from it.
//!
//! Ported from `codex-rs/tui/src/bottom_pane`. The composer, its popups, the textarea, and the
//! modal view stack are kept; Codex's product surface - approvals, MCP elicitation, reasoning
//! effort, skills, hooks, memories, status line, voice - is not.
//!
//! [`BottomPane`] owns a [`ChatComposer`] plus a stack of [`BottomPaneView`]s. A view takes the
//! whole pane while it is open and the composer keeps its draft underneath, so dismissing a modal
//! returns the user to exactly what they were typing. Work the pane cannot do itself leaves through
//! [`events::PaneEventSender`] instead of Codex's global app-event bus, and the reducer drains it
//! with [`BottomPane::drain_events`].
//!
//! Redraws are not scheduled here. Codex asks a `FrameRequester` for a frame at a delay; July's
//! event loop already draws on a timer and calls [`BottomPane::pre_draw_tick`] before each frame,
//! so animations and paste-burst flushes ride that tick instead.

// ponytail: the pane carries views July has not wired into its runtime yet - the agent-question
// editor, the user-input overlay, the pickers - plus composer setters with no caller. They were kept
// deliberately (plan 26, section 2.1), so the crate-level lint would only be noise. Remove this and
// delete what is still unused once July drives those views.
#![allow(dead_code)]

use std::path::PathBuf;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::tui::file_search::FileMatch;
use crate::tui::support::keymap::{KeymapContextSet, RuntimeKeymap};
use crate::tui::support::render::renderable::{Renderable, RenderableItem};
use crate::tui::user_input::TextElement;

pub(crate) mod events;

mod bottom_pane_view;
pub(crate) use bottom_pane_view::BottomPaneView;
pub(crate) use bottom_pane_view::ViewCompletion;

mod async_questions;
mod multi_select_picker;
mod request_user_input;
pub(crate) use async_questions::{AsyncQuestions, AsyncUserInputQuestion};
pub(crate) use request_user_input::{RequestUserInputOverlay, UserInputRequest};

/// An image the user attached to the draft, and the text standing in for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LocalImageAttachment {
    pub(crate) placeholder: String,
    pub(crate) path: PathBuf,
}

/// A mention in the draft, and what it points at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MentionBinding {
    /// Visible mention sigil.
    pub(crate) sigil: char,
    /// Mention token text without the leading sigil.
    pub(crate) mention: String,
    /// Canonical mention target.
    pub(crate) path: String,
}

mod chat_composer;
mod chat_composer_history;
mod command_popup;
pub(crate) mod custom_prompt_view;
mod file_search_popup;
mod footer;
mod list_selection_view;
mod mentions_v2;
pub(crate) mod prompt_args;
pub(crate) mod slash_commands;

mod paste_burst;
pub(crate) mod popup_consts;
mod scroll_state;
mod selection_popup_common;
mod selection_row_layout;
mod selection_tabs;
mod textarea;

pub(crate) use chat_composer::ChatComposer;
pub(crate) use chat_composer::ChatComposerConfig;
pub(crate) use chat_composer::InputResult;
pub(crate) use events::PaneEvent;
pub(crate) use list_selection_view::{ListSelectionView, SelectionItem, SelectionViewParams};
pub(crate) use slash_commands::SlashCommand;

/// How long the "press again to quit" hint stays visible.
pub(crate) const QUIT_SHORTCUT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

/// The result of offering a cancellation key to a bottom-pane surface.
///
/// Used for Ctrl+C routing: an open view can consume the key to dismiss itself, and the caller
/// decides what to do when nothing local handled it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CancellationEvent {
    Handled,
    NotHandled,
}

/// How the pane is built.
pub(crate) struct BottomPaneParams {
    pub(crate) has_input_focus: bool,
    /// Whether the terminal reports modified keys, which decides the newline hint.
    pub(crate) enhanced_keys_supported: bool,
    pub(crate) placeholder_text: String,
    /// Turns off the heuristic that treats fast keystrokes as a paste.
    pub(crate) disable_paste_burst: bool,
}

/// The composer, and whatever modal view is currently covering it.
pub(crate) struct BottomPane {
    /// Keeps the draft alive underneath any open view.
    composer: ChatComposer,
    /// Last element is the active view; empty means the composer is showing.
    view_stack: Vec<Box<dyn BottomPaneView>>,
    events: events::PaneEventSender,
    has_input_focus: bool,
    enhanced_keys_supported: bool,
    disable_paste_burst: bool,
    keymap: RuntimeKeymap,
}

impl BottomPane {
    pub(crate) fn new(params: BottomPaneParams) -> Self {
        Self::new_with_composer_config(params, ChatComposerConfig::default())
    }

    /// Builds a pane whose composer is restricted, for example to a plain notes field.
    pub(crate) fn new_with_composer_config(
        params: BottomPaneParams,
        composer_config: ChatComposerConfig,
    ) -> Self {
        let BottomPaneParams {
            has_input_focus,
            enhanced_keys_supported,
            placeholder_text,
            disable_paste_burst,
        } = params;
        let events = events::PaneEventSender::new();
        let keymap = RuntimeKeymap::defaults();
        let mut composer = ChatComposer::new_with_config(
            has_input_focus,
            events.clone(),
            enhanced_keys_supported,
            placeholder_text,
            disable_paste_burst,
            composer_config,
        );
        composer.set_keymap_bindings(&keymap);
        // ponytail: July has no token budget to report, and Codex's footer falls back to a flat
        // "100% context left" when none is set. Marking it pending keeps that column empty. Call
        // `ChatComposer::set_context_window` instead once July tracks context usage.
        composer.set_context_window_pending(true);
        Self {
            composer,
            view_stack: Vec::new(),
            events,
            has_input_focus,
            enhanced_keys_supported,
            disable_paste_burst,
            keymap,
        }
    }

    /// Takes the side effects queued since the last drain, for the reducer to carry out.
    pub(crate) fn drain_events(&self) -> Vec<PaneEvent> {
        self.events.drain()
    }

    // ---- key routing -------------------------------------------------------

    /// Routes a key to the active view, or to the composer when none is open.
    ///
    /// A view never returns an [`InputResult`]: only the composer submits.
    pub(crate) fn handle_key_event(&mut self, key_event: KeyEvent) -> InputResult {
        if self.view_stack.is_empty() {
            return self.composer.handle_key_event(key_event).0;
        }

        if key_event.kind == KeyEventKind::Release {
            return InputResult::None;
        }

        // Esc reaches a view through its cancellation path unless the view asked to read Esc as an
        // ordinary key, so a picker can dismiss itself while an editor can leave insert mode.
        let (completed_by_escape, view_complete, completion) = {
            let view = self
                .view_stack
                .last_mut()
                .expect("view stack checked non-empty");
            let prefer_esc =
                key_event.code == KeyCode::Esc && view.prefer_esc_to_handle_key_event();
            let completed_by_escape = key_event.code == KeyCode::Esc
                && !prefer_esc
                && matches!(view.on_ctrl_c(), CancellationEvent::Handled)
                && view.is_complete();
            if completed_by_escape {
                (true, true, view.completion())
            } else {
                view.handle_key_event(key_event);
                (false, view.is_complete(), view.completion())
            }
        };

        if completed_by_escape || view_complete {
            self.pop_active_view_with_completion(completion);
        }
        InputResult::None
    }

    /// Handles Ctrl+C, returning whether anything in the pane consumed it.
    ///
    /// An open view gets first refusal. Otherwise Ctrl+C cancels an in-progress search before it
    /// clears the draft, and reports `NotHandled` only when there was nothing left to cancel -
    /// which is the caller's signal that Ctrl+C means interrupt or quit.
    pub(crate) fn on_ctrl_c(&mut self) -> CancellationEvent {
        if let Some(view) = self.view_stack.last_mut() {
            let event = view.on_ctrl_c();
            let view_complete = view.is_complete();
            let completion = view.completion();
            if matches!(event, CancellationEvent::Handled) && view_complete {
                self.pop_active_view_with_completion(completion);
            }
            return event;
        }

        if self.composer.cancel_vim_search() || self.composer.cancel_history_search() {
            CancellationEvent::Handled
        } else if self.composer_is_empty() {
            CancellationEvent::NotHandled
        } else {
            self.composer.clear_for_ctrl_c();
            CancellationEvent::Handled
        }
    }

    /// Routes pasted text to the active view, or to the composer when none is open.
    pub(crate) fn handle_paste(&mut self, pasted: String) {
        if let Some(view) = self.view_stack.last_mut() {
            view.handle_paste(pasted);
            if view.is_complete() {
                let completion = view.completion();
                self.pop_active_view_with_completion(completion);
            }
        } else {
            self.composer.handle_paste(pasted);
        }
    }

    /// Inserts text at the composer cursor, bypassing paste handling.
    pub(crate) fn insert_str(&mut self, text: &str) {
        self.composer.insert_str(text);
    }

    /// Which keymap contexts can consume the next key, for rendering shortcut hints.
    pub(crate) fn keymap_contexts(&self) -> KeymapContextSet {
        match self.view_stack.last() {
            Some(view) => view.keymap_contexts(),
            None => self.composer.keymap_contexts(),
        }
    }

    // ---- per-frame work ----------------------------------------------------

    /// Advances time-based state, reporting whether anything visible changed.
    ///
    /// The event loop calls this before each draw.
    pub(crate) fn pre_draw_tick(&mut self) -> bool {
        self.pre_draw_tick_at(Instant::now())
    }

    fn pre_draw_tick_at(&mut self, now: Instant) -> bool {
        self.composer.sync_popups();
        let Some(view) = self.view_stack.last_mut() else {
            return false;
        };
        let changed = view.pre_draw_tick(now);
        if view.is_complete() {
            let completion = view.completion();
            self.pop_active_view_with_completion(completion);
            return true;
        }
        changed
    }

    /// Flushes a paste burst whose window has closed, so held keystrokes reach the draft.
    pub(crate) fn flush_paste_burst_if_due(&mut self) -> bool {
        match self.view_stack.last_mut() {
            Some(view) => view.flush_paste_burst_if_due(),
            None => self.composer.flush_paste_burst_if_due(),
        }
    }

    /// Whether keystrokes are currently being held as a suspected paste.
    pub(crate) fn is_in_paste_burst(&self) -> bool {
        match self.view_stack.last() {
            Some(view) => view.is_in_paste_burst(),
            None => self.composer.is_in_paste_burst(),
        }
    }

    // ---- composer state ----------------------------------------------------

    pub(crate) fn composer_text(&self) -> String {
        self.composer.current_text()
    }

    pub(crate) fn composer_is_empty(&self) -> bool {
        self.composer.is_empty()
    }

    /// Replaces the composer's footer hints with July's own.
    /// Overrides the composer's footer row, or hands it back when given `None`.
    pub(crate) fn set_footer_hint(&mut self, items: Option<Vec<(String, String)>>) {
        self.composer.set_footer_hint_override(items);
    }

    /// Finishes a slash command the composer dispatched.
    ///
    /// The composer hands out the command and its arguments but leaves the text in place, because
    /// it does not know whether the caller accepted it. Once the command is on its way, its text
    /// belongs in history, not in the draft.
    pub(crate) fn finish_command_submission(&mut self) {
        self.composer.record_pending_slash_command_history();
        self.composer
            .set_text_content(String::new(), Vec::new(), Vec::new());
    }

    /// The visible draft and caret, for carrying across a scope switch.
    pub(crate) fn composer_draft(&self) -> crate::tui::app::ComposerDraft {
        crate::tui::app::ComposerDraft {
            text: self.composer.current_text(),
            cursor: self.composer.current_cursor(),
        }
    }

    /// Puts a previously captured draft back, caret included.
    pub(crate) fn restore_composer_draft(&mut self, draft: crate::tui::app::ComposerDraft) {
        let crate::tui::app::ComposerDraft { text, cursor } = draft;
        let cursor = cursor.min(text.len());
        self.composer.set_text_content(text, Vec::new(), Vec::new());
        self.composer.set_current_cursor(cursor);
    }

    pub(crate) fn set_composer_text(&mut self, text: String, text_elements: Vec<TextElement>) {
        self.composer
            .set_text_content(text, text_elements, Vec::new());
    }

    pub(crate) fn set_placeholder_text(&mut self, placeholder: String) {
        self.composer.set_placeholder_text(placeholder);
    }

    /// Replaces the slash commands offered after `/`.
    pub(crate) fn set_slash_commands(&mut self, commands: Vec<SlashCommand>) {
        self.composer.set_slash_commands(commands);
    }

    /// Turns on the `@` popup that offers agents alongside workspace files.
    pub(crate) fn set_mentions_v2_enabled(&mut self, enabled: bool) {
        self.composer.set_mentions_v2_enabled(enabled);
    }

    /// Replaces the agents offered after `@`.
    pub(crate) fn set_agents(&mut self, agents: Vec<String>) {
        self.composer.set_agents(agents);
    }

    /// Tells the composer a turn is running, which changes its footer hints.
    pub(crate) fn set_task_running(&mut self, running: bool) {
        self.composer.set_task_running(running);
    }

    /// Delivers file-search results for the `@` popup's current query.
    pub(crate) fn on_file_search_result(&mut self, query: String, matches: Vec<FileMatch>) {
        self.composer.on_file_search_result(query, matches);
    }

    /// Records a submitted prompt so Up/Down and reverse search can recall it.
    pub(crate) fn record_submission_history(&mut self, text: String) {
        self.composer
            .record_replayed_user_message_history(chat_composer_history::HistoryEntry::new(text));
    }

    // ---- view stack --------------------------------------------------------

    /// Whether a view is covering the composer.
    pub(crate) fn has_active_view(&self) -> bool {
        !self.view_stack.is_empty()
    }

    /// The active view's id, when it has one.
    pub(crate) fn active_view_id(&self) -> Option<&'static str> {
        self.view_stack.last().and_then(|view| view.view_id())
    }

    /// Pushes a view on top of the composer.
    pub(crate) fn show_view(&mut self, view: Box<dyn BottomPaneView>) {
        self.view_stack.push(view);
    }

    /// The id a permission prompt is pushed under, so a resolved request can dismiss it.
    pub(crate) const PERMISSION_VIEW_ID: &'static str = "permission";

    /// Pushes the prompt asking the user to approve or reject an agent's request.
    ///
    /// Each option becomes a row; picking one answers the request, and dismissing it rejects.
    pub(crate) fn push_permission_request(
        &mut self,
        request_id: crate::application::ChatPermissionRequestId,
        prompt: String,
        options: Vec<crate::domain::PermissionOption>,
    ) {
        let items = options
            .into_iter()
            .map(|option| {
                let request_id = request_id.clone();
                let outcome = crate::domain::PermissionOutcome::Selected(option.id);
                SelectionItem {
                    name: option.label,
                    dismiss_on_select: true,
                    actions: vec![Box::new(move |events: &events::PaneEventSender| {
                        events.send(PaneEvent::PermissionResponse {
                            request_id: request_id.clone(),
                            outcome: outcome.clone(),
                        });
                    })],
                    ..SelectionItem::default()
                }
            })
            .collect();
        let cancel_request_id = request_id;
        let params = SelectionViewParams {
            view_id: Some(Self::PERMISSION_VIEW_ID),
            title: Some("Permission requested".to_string()),
            subtitle: Some(prompt),
            items,
            on_cancel: Some(Box::new(move |events: &events::PaneEventSender| {
                events.send(PaneEvent::PermissionResponse {
                    request_id: cancel_request_id.clone(),
                    outcome: crate::domain::PermissionOutcome::Cancelled,
                });
            })),
            ..SelectionViewParams::default()
        };
        self.show_selection_view(params);
    }

    /// Pushes the inline editor for questions an agent asked mid-turn.
    pub(crate) fn push_questions(
        &mut self,
        message_id: &str,
        questions: &[AsyncUserInputQuestion],
    ) {
        if questions.is_empty() {
            return;
        }
        let mut view = AsyncQuestions::new(
            self.events.clone(),
            self.has_input_focus,
            self.enhanced_keys_supported,
            self.disable_paste_burst,
            self.keymap.clone(),
        );
        view.append(message_id, questions);
        self.show_view(Box::new(view));
    }

    /// Pushes the overlay for an agent's request for user input.
    pub(crate) fn push_user_input_request(&mut self, request: UserInputRequest) {
        let view = RequestUserInputOverlay::new_with_keymap(
            request,
            self.events.clone(),
            self.has_input_focus,
            self.enhanced_keys_supported,
            self.disable_paste_burst,
            self.keymap.clone(),
        );
        self.show_view(Box::new(view));
    }

    /// Pushes a list picker.
    pub(crate) fn show_selection_view(&mut self, params: SelectionViewParams) {
        let view = ListSelectionView::new(params, self.events.clone(), self.keymap.list.clone());
        self.show_view(Box::new(view));
    }

    /// Removes the top view if it carries `view_id`.
    pub(crate) fn dismiss_active_view_if_id(&mut self, view_id: &'static str) -> bool {
        if self.active_view_id() != Some(view_id) {
            return false;
        }
        self.pop_active_view_with_completion(Some(ViewCompletion::Cancelled));
        true
    }

    /// Removes a view anywhere in the stack by id.
    pub(crate) fn dismiss_view_by_id(&mut self, view_id: &'static str) -> bool {
        let Some(index) = self
            .view_stack
            .iter()
            .rposition(|view| view.view_id() == Some(view_id))
        else {
            return false;
        };
        self.view_stack.remove(index);
        true
    }

    /// Closes every open view, leaving the composer showing with its draft intact.
    pub(crate) fn clear_views(&mut self) {
        self.view_stack.clear();
    }

    /// Pops the active view, unwinding parents that asked to close with their child.
    ///
    /// Accepting a child view closes each parent that opted into `dismiss_after_child_accept`, so a
    /// multi-step flow collapses in one step. Cancelling only clears that intent on the new top, so
    /// backing out of a child returns to its parent rather than closing it too.
    fn pop_active_view_with_completion(&mut self, completion: Option<ViewCompletion>) {
        if self.view_stack.pop().is_none() {
            return;
        }
        match completion {
            Some(ViewCompletion::Accepted) => {
                while self
                    .view_stack
                    .last()
                    .is_some_and(|view| view.dismiss_after_child_accept())
                {
                    self.view_stack.pop();
                }
            }
            Some(ViewCompletion::Cancelled) => {
                if let Some(view) = self.view_stack.last_mut() {
                    view.clear_dismiss_after_child_accept();
                }
            }
            None => {}
        }
    }

    /// The pane's render tree: the active view if there is one, else the composer.
    fn as_renderable(&self) -> RenderableItem<'_> {
        match self.view_stack.last() {
            Some(view) => RenderableItem::Borrowed(view.as_ref()),
            None => RenderableItem::Borrowed(&self.composer),
        }
    }
}

impl Renderable for BottomPane {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.as_renderable().render(area, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.as_renderable().desired_height(width)
    }

    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        self.as_renderable().cursor_pos(area)
    }

    fn cursor_style(&self, area: Rect) -> crossterm::cursor::SetCursorStyle {
        self.as_renderable().cursor_style(area)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    /// A view that records the keys it saw and completes when told to.
    #[derive(Default)]
    struct TestView {
        id: Option<&'static str>,
        keys_seen: usize,
        complete: bool,
        completion: Option<ViewCompletion>,
        dismiss_after_child_accept: bool,
        /// Whether Ctrl+C (and bare Esc) should finish this view.
        cancels: bool,
    }

    impl Renderable for TestView {
        fn render(&self, _area: Rect, _buf: &mut Buffer) {}
        fn desired_height(&self, _width: u16) -> u16 {
            1
        }
    }

    impl BottomPaneView for TestView {
        fn handle_key_event(&mut self, _key_event: KeyEvent) {
            self.keys_seen += 1;
        }

        fn is_complete(&self) -> bool {
            self.complete
        }

        fn completion(&self) -> Option<ViewCompletion> {
            self.completion
        }

        fn view_id(&self) -> Option<&'static str> {
            self.id
        }

        fn dismiss_after_child_accept(&self) -> bool {
            self.dismiss_after_child_accept
        }

        fn clear_dismiss_after_child_accept(&mut self) {
            self.dismiss_after_child_accept = false;
        }

        fn on_ctrl_c(&mut self) -> CancellationEvent {
            if self.cancels {
                self.complete = true;
                self.completion = Some(ViewCompletion::Cancelled);
                CancellationEvent::Handled
            } else {
                CancellationEvent::NotHandled
            }
        }
    }

    fn pane() -> BottomPane {
        BottomPane::new(BottomPaneParams {
            has_input_focus: true,
            enhanced_keys_supported: false,
            placeholder_text: String::new(),
            disable_paste_burst: true,
        })
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn parent(dismiss_after_child_accept: bool) -> Box<TestView> {
        Box::new(TestView {
            id: Some("parent"),
            dismiss_after_child_accept,
            ..TestView::default()
        })
    }

    #[test]
    fn dismissing_by_id_only_matches_that_view() {
        let mut pane = pane();
        pane.show_view(parent(/*dismiss_after_child_accept*/ false));

        assert!(!pane.dismiss_active_view_if_id("other"));
        assert!(pane.dismiss_active_view_if_id("parent"));
        assert!(!pane.has_active_view());
    }

    #[test]
    fn a_view_buried_in_the_stack_can_still_be_dismissed_by_id() {
        let mut pane = pane();
        pane.show_view(parent(/*dismiss_after_child_accept*/ false));
        pane.show_view(Box::new(TestView {
            id: Some("child"),
            ..TestView::default()
        }));

        assert!(pane.dismiss_view_by_id("parent"));
        assert_eq!(pane.active_view_id(), Some("child"));
    }
}
