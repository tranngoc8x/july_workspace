//! Composer-side Ctrl+R reverse history search state and rendering helpers.
//!
//! The persistent and local history stores live in `chat_composer_history`, but the composer owns
//! the active search session because it has to snapshot/restore the editable draft and Vim edit
//! state, preview matches in the textarea, and render the footer prompt while the footer line is
//! acting as the search input.
//!
//! This module is responsible for the UI-facing lifecycle of a search session: recognizing the
//! keys that enter and drive search mode, keeping the footer query separate from the textarea
//! preview, restoring the original draft on cancellation or misses, and translating history search
//! results into composer-visible state. It deliberately does not decide which history entries
//! match, how duplicate results are skipped, or when persistent history should be fetched; those
//! traversal invariants stay with `ChatComposerHistory`.
//!
//! A search session starts idle with an empty footer query, so opening Ctrl+R never previews the
//! latest history entry by itself. Typing or pasting a query restarts traversal from newest to oldest,
//! repeated Ctrl+R/Up and Ctrl+S/Down move between unique matches, `Enter` accepts the current
//! preview as an editable draft, and `Esc` or Ctrl+C restores the exact draft that existed before
//! search started.

use std::ops::Range;

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;

use super::super::chat_composer_history::HistorySearchDirection;
use super::super::chat_composer_history::HistorySearchResult;
use super::super::footer::footer_height;
use super::super::footer::reset_mode_after_activity;
use super::super::textarea::VimPersistentState;
use super::ActivePopup;
use super::ChatComposer;
use super::ComposerDraft;
use super::InputResult;
use super::vim_history::VimHistory;
use crate::tui::bottom_pane::events::PaneEvent;
use crate::tui::support::key_hint;
use crate::tui::support::key_hint::KeyBinding;
use crate::tui::support::key_hint::KeyBindingListExt;
use crate::tui::support::key_hint::has_ctrl_or_alt;
use crate::tui::support::ui_consts::FOOTER_INDENT_COLS;

/// Active composer-owned state for one Ctrl+R search interaction.
///
/// The session is created only by [`ChatComposer::begin_history_search`] and is cleared only by
/// accepting, canceling, or replacing the search mode. It stores the original draft and Vim edit
/// state separately from the footer query so transient previews never destroy in-progress content.
#[derive(Debug)]
pub(super) struct HistorySearchSession {
    /// Draft to restore when search is canceled or a query has no match.
    original_draft: ComposerDraft,
    /// Same-draft Vim edits to restore when a temporary preview is canceled.
    original_vim_history: VimHistory,
    /// Active and completed Vim commands suspended during temporary draft replacement.
    original_vim_state: VimPersistentState,
    /// Footer-owned query text typed or pasted while Ctrl+R search is active.
    query: String,
    /// User-visible search status used to choose footer hints and composer preview behavior.
    status: HistorySearchStatus,
}

impl HistorySearchSession {
    /// Renders newlines and tabs as visible markers for the footer and cursor placement.
    /// Matching continues to use the original query.
    fn display_query(&self) -> String {
        self.query.replace('\n', "↵").replace('\t', "⇥")
    }
}

/// User-visible phase of the active Ctrl+R search session.
///
/// Search keeps the footer query and the composer preview separate: `Idle` leaves the original
/// draft untouched, `Searching` waits for persistent history, `Match` previews a found entry, and
/// `NoMatch` restores the original draft while leaving the search input open for more typing.
#[derive(Clone, Debug)]
enum HistorySearchStatus {
    Idle,
    Searching,
    Match,
    NoMatch,
}

impl ChatComposer {
    #[cfg(test)]
    pub(super) fn history_search_active(&self) -> bool {
        self.history_search.is_some()
    }

    /// Returns whether a key event should open reverse history search or step to an older match.
    ///
    /// The check accepts both normal Ctrl+R reports and the raw control character variant that
    /// some terminals emit. Callers should only use this before generic text handling; treating the
    /// raw control character as ordinary input would insert an invisible byte into the search query
    /// or composer draft.
    pub(super) fn is_history_search_key(key_event: &KeyEvent, bindings: &[KeyBinding]) -> bool {
        bindings.is_pressed(*key_event)
    }

    fn is_history_search_forward_key(key_event: &KeyEvent, bindings: &[KeyBinding]) -> bool {
        bindings.is_pressed(*key_event)
    }

    /// Opens footer-owned reverse history search without previewing history yet.
    ///
    /// Entering search mode first flushes pending paste-burst text, then snapshots the full
    /// composer draft, clears any file/search popup state, and resets history traversal. The first
    /// visible match is produced only after the footer query becomes non-empty, which keeps Ctrl+R
    /// from replacing an empty composer with the latest prompt before the user has searched for
    /// anything.
    pub(super) fn begin_history_search(&mut self) -> (InputResult, bool) {
        if let Some(pasted) = self.draft.paste_burst.flush_before_modified_input() {
            self.handle_paste(pasted);
        }
        self.draft.paste_burst.clear_window_after_non_char();

        if self.popups.current_file_query.is_some() {
            self.app_event_tx
                .send(PaneEvent::StartFileSearch(String::new()));
            self.popups.current_file_query = None;
        }
        self.popups.active = ActivePopup::None;
        self.attachments.clear_remote_image_selection();
        let original_draft = self.snapshot_draft();
        let original_vim_history = std::mem::take(&mut self.vim_history);
        let mut original_vim_state = VimPersistentState::default();
        self.draft
            .textarea
            .swap_vim_persistent_state(&mut original_vim_state);
        self.history_search = Some(HistorySearchSession {
            original_draft,
            original_vim_history,
            original_vim_state,
            query: String::new(),
            status: HistorySearchStatus::Idle,
        });
        self.history.reset_search();
        (InputResult::None, true)
    }

    /// Handles every key while the footer is acting as the history search input.
    ///
    /// The method consumes search-mode keys before normal composer editing sees them. It guarantees
    /// that `Esc` and Ctrl+C restore the original draft, `Enter` only accepts an actual match, plain
    /// characters edit the footer query, and navigation keys delegate traversal to
    /// `ChatComposerHistory`. Calling this when no search session exists is harmless for ignored
    /// keys but would make query-edit branches no-op, so route here only after
    /// `history_search.is_some()` has been established.
    pub(super) fn handle_history_search_key(&mut self, key_event: KeyEvent) -> (InputResult, bool) {
        if key_event.kind == KeyEventKind::Release {
            return (InputResult::None, false);
        }

        if Self::is_history_search_key(&key_event, &self.history_search_previous_keys)
            || matches!(key_event.code, KeyCode::Up)
        {
            let result = self.history_search_in_direction(HistorySearchDirection::Older);
            return (result, true);
        }

        if Self::is_history_search_forward_key(&key_event, &self.history_search_next_keys)
            || matches!(key_event.code, KeyCode::Down)
        {
            let result = self.history_search_in_direction(HistorySearchDirection::Newer);
            return (result, true);
        }

        match key_event {
            KeyEvent {
                code: KeyCode::Esc, ..
            } => {
                self.cancel_history_search();
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Char(c),
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) && c.eq_ignore_ascii_case(&'c') => {
                self.cancel_history_search();
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Char('\u{0003}'),
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                self.cancel_history_search();
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                if self
                    .history_search
                    .as_ref()
                    .is_some_and(|search| matches!(search.status, HistorySearchStatus::Match))
                {
                    self.history_search = None;
                    self.history.reset_search();
                    self.footer.mode = reset_mode_after_activity(self.footer.mode);
                    self.move_cursor_to_end();
                }
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Backspace,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('h'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                self.update_history_search_query(|query| {
                    query.pop();
                });
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Char('u'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                self.update_history_search_query(String::clear);
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Char(ch),
                modifiers,
                ..
            } if !has_ctrl_or_alt(modifiers) => {
                self.update_history_search_query(|query| query.push(ch));
                (InputResult::None, true)
            }
            _ => (InputResult::None, true),
        }
    }

    fn history_search_in_direction(&mut self, direction: HistorySearchDirection) -> InputResult {
        let Some((query, original_draft)) = self
            .history_search
            .as_ref()
            .map(|search| (search.query.clone(), search.original_draft.clone()))
        else {
            return InputResult::None;
        };
        if query.is_empty() {
            self.history.reset_search();
            if let Some(search) = self.history_search.as_mut() {
                search.status = HistorySearchStatus::Idle;
            }
            self.restore_draft(original_draft);
            return InputResult::None;
        }
        let result = self.history.search(
            &query,
            direction,
            /*restart*/ false,
            &self.app_event_tx,
        );
        self.apply_history_search_result(result);
        InputResult::None
    }

    /// Edits the footer query and restarts history traversal from the newest entry.
    /// An empty query restores the original draft and leaves search open.
    pub(super) fn update_history_search_query(&mut self, edit: impl FnOnce(&mut String)) {
        let Some(search) = self.history_search.as_mut() else {
            return;
        };
        edit(&mut search.query);
        search.status = HistorySearchStatus::Searching;
        let query = search.query.clone();
        let original_draft = search.original_draft.clone();
        self.restore_draft(original_draft);
        if query.is_empty() {
            self.history.reset_search();
            if let Some(search) = self.history_search.as_mut() {
                search.status = HistorySearchStatus::Idle;
            }
            return;
        }
        let result = self.history.search(
            &query,
            HistorySearchDirection::Older,
            /*restart*/ true,
            &self.app_event_tx,
        );
        self.apply_history_search_result(result);
    }

    /// Cancels active history search and restores the draft from before search mode opened.
    ///
    /// This clears normal history navigation as well as search traversal because previewing a match
    /// temporarily updates the shared history cursor. Callers that handle global cancellation, such
    /// as Ctrl+C, should use the boolean result to consume the key without also clearing the
    /// restored draft or triggering quit/interrupt behavior.
    pub(crate) fn cancel_history_search(&mut self) -> bool {
        let Some(mut search) = self.history_search.take() else {
            return false;
        };
        self.history.reset_navigation();
        self.footer.mode = reset_mode_after_activity(self.footer.mode);
        self.restore_draft(search.original_draft);
        self.vim_history = search.original_vim_history;
        self.draft
            .textarea
            .swap_vim_persistent_state(&mut search.original_vim_state);
        true
    }

    /// Applies a traversal result to the composer preview and search status.
    ///
    /// `Found` previews the matching entry, `Pending` keeps the footer in a waiting state while an
    /// async persistent entry lookup is outstanding, `AtBoundary` preserves the current match, and
    /// `NotFound` restores the original draft while keeping the query available for further edits,
    /// and `Unavailable` does the same without claiming there was no match. Treating `AtBoundary`
    /// like `NotFound` would produce the visible "no match" flicker at the end of a one-result
    /// search and desynchronize Up/Down counts.
    pub(super) fn apply_history_search_result(&mut self, result: HistorySearchResult) {
        match result {
            HistorySearchResult::Found(entry) => {
                if let Some(search) = self.history_search.as_mut() {
                    search.status = HistorySearchStatus::Match;
                }
                self.apply_history_entry(entry);
            }
            HistorySearchResult::Pending => {
                if let Some(search) = self.history_search.as_mut() {
                    search.status = HistorySearchStatus::Searching;
                }
            }
            HistorySearchResult::AtBoundary => {
                if let Some(search) = self.history_search.as_mut() {
                    search.status = HistorySearchStatus::Match;
                }
            }
            result @ (HistorySearchResult::NotFound | HistorySearchResult::Unavailable) => {
                let original_draft = self
                    .history_search
                    .as_ref()
                    .map(|search| search.original_draft.clone());
                if let Some(search) = self.history_search.as_mut() {
                    search.status = if matches!(result, HistorySearchResult::NotFound) {
                        HistorySearchStatus::NoMatch
                    } else {
                        HistorySearchStatus::Idle
                    };
                }
                if let Some(original_draft) = original_draft {
                    self.restore_draft(original_draft);
                }
            }
        }
    }

    /// Builds the footer line shown while reverse history search is active.
    ///
    /// The footer displays the query as the editable field and uses the status to decide whether
    /// to show searching, match actions, or no-match feedback. Newlines and tabs use visible markers
    /// while matching keeps the original query. The line is intentionally separate from cursor
    /// placement so rendering can fall back to normal footer layout if a small terminal cannot
    /// allocate a distinct hint row.
    pub(super) fn history_search_footer_line(&self) -> Option<Line<'static>> {
        let search = self.history_search.as_ref()?;
        let mut line = Line::from(vec![
            "reverse-i-search: ".dim(),
            search.display_query().cyan(),
        ]);
        match search.status {
            HistorySearchStatus::Idle => {}
            HistorySearchStatus::Searching => line.push_span("  searching".dim()),
            HistorySearchStatus::Match => {
                line.push_span("  ".dim());
                line.push_span(Self::history_search_action_key_span(KeyCode::Enter));
                line.push_span(" accept".dim());
                line.push_span(" · ".dim());
                line.push_span(Self::history_search_action_key_span(KeyCode::Esc));
                line.push_span(" cancel".dim());
            }
            HistorySearchStatus::NoMatch => line.push_span("  no match".red()),
        }
        Some(line)
    }

    fn history_search_action_key_span(key: KeyCode) -> Span<'static> {
        Span::from(key_hint::plain(key)).cyan().bold().not_dim()
    }

    /// Returns byte ranges that should be highlighted in the current composer preview.
    ///
    /// Highlights are only exposed while a matched history entry is being previewed. Once the user
    /// accepts with `Enter`, the search session is cleared and this returns an empty set so the
    /// accepted text becomes an ordinary editable draft again.
    pub(super) fn history_search_highlight_ranges(&self) -> Vec<Range<usize>> {
        let Some(search) = self.history_search.as_ref() else {
            return Vec::new();
        };
        if !matches!(search.status, HistorySearchStatus::Match) || search.query.is_empty() {
            return Vec::new();
        }
        Self::case_insensitive_match_ranges(self.draft.textarea.text(), &search.query)
    }

    fn case_insensitive_match_ranges(text: &str, query: &str) -> Vec<Range<usize>> {
        if query.is_empty() {
            return Vec::new();
        }

        let query_lower = query
            .chars()
            .flat_map(char::to_lowercase)
            .collect::<String>();
        if query_lower.is_empty() {
            return Vec::new();
        }

        let mut folded = String::new();
        let mut folded_spans: Vec<(Range<usize>, Range<usize>)> = Vec::new();
        for (original_start, ch) in text.char_indices() {
            let original_range = original_start..original_start + ch.len_utf8();
            for lower in ch.to_lowercase() {
                let folded_start = folded.len();
                folded.push(lower);
                folded_spans.push((folded_start..folded.len(), original_range.clone()));
            }
        }

        let mut ranges = Vec::new();
        let mut search_from = 0;
        // Use two-pointer method to find matches in linear time.
        let mut start_span = 0;
        let mut end_span = 0;
        while search_from <= folded.len()
            && let Some(relative_start) = folded[search_from..].find(&query_lower)
        {
            let folded_start = search_from + relative_start;
            let folded_end = folded_start + query_lower.len();
            while folded_spans[start_span].0.end <= folded_start {
                start_span += 1;
            }
            while folded_spans[end_span].0.end < folded_end {
                end_span += 1;
            }
            ranges.push(folded_spans[start_span].1.start..folded_spans[end_span].1.end);
            search_from = folded_end;
        }
        ranges
    }

    /// Returns the screen cursor position for the footer query when search mode is active.
    ///
    /// The cursor tracks the end of the footer query rather than the textarea preview. If the
    /// footer area is collapsed or too narrow, the x coordinate is clamped inside the hint rect so
    /// terminal backends do not receive an off-screen cursor position.
    pub(super) fn history_search_cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        self.history_search.as_ref()?;
        let [_, _, _, popup_rect] = self.layout_areas(area);
        if popup_rect.is_empty() {
            return None;
        }

        let footer_props = self.footer_props();
        let footer_hint_height = self
            .custom_footer_height()
            .unwrap_or_else(|| footer_height(&footer_props));
        let footer_spacing = Self::footer_spacing(footer_hint_height);
        let hint_rect = if footer_spacing > 0 && footer_hint_height > 0 {
            let [_, hint_rect] = Layout::vertical([
                Constraint::Length(footer_spacing),
                Constraint::Length(footer_hint_height),
            ])
            .areas(popup_rect);
            hint_rect
        } else {
            popup_rect
        };
        if hint_rect.is_empty() {
            return None;
        }

        let indent = (FOOTER_INDENT_COLS as u16).min(hint_rect.width.saturating_sub(1));
        self.history_search_query_cursor_pos(Rect {
            x: hint_rect.x.saturating_add(indent),
            width: hint_rect.width.saturating_sub(indent),
            ..hint_rect
        })
    }

    pub(super) fn history_search_query_cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        let search = self.history_search.as_ref()?;
        if area.is_empty() {
            return None;
        }
        let prompt_width = Line::from("reverse-i-search: ").width() as u16;
        let query_width =
            u16::try_from(Line::from(search.display_query()).width()).unwrap_or(u16::MAX);
        let desired_x = area
            .x
            .saturating_add(prompt_width)
            .saturating_add(query_width);
        let max_x = area.x.saturating_add(area.width.saturating_sub(1));
        Some((desired_x.min(max_x), area.y))
    }
}
