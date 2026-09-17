//! The textarea owns editable composer text, placeholder elements, cursor/wrap state, and a
//! single-entry kill buffer.
//!
//! Whole-buffer replacement APIs intentionally rebuild only the visible draft state. They clear
//! element ranges and derived cursor/wrapping caches, but they keep the kill buffer intact so a
//! caller can clear or rewrite the draft and still allow `Ctrl+Y` to restore the user's most
//! recent `Ctrl+K`. This is the contract higher-level composer flows rely on after submit,
//! slash-command dispatch, and other synthetic clears.
//!
//! This module does not implement an Emacs-style multi-entry kill ring. It keeps only the most
//! recent killed span.
//!
//! Wrapping also reserves a visible insertion point: full logical lines get continuation rows,
//! and trailing spaces wrap instead of moving the cursor outside the textarea. At soft word
//! breaks, interior separators hang off the preceding row without changing the editable text.
//! Visible web URLs carry their complete terminal hyperlink destination across wrapped rows;
//! masked rendering never exposes hyperlink destinations.

use crate::tui::support::key_hint::KeyBindingListExt;
use crate::tui::support::key_hint::is_altgr;
use crate::tui::support::keymap::EditorKeymap;
use crate::tui::support::keymap::KeymapContext;
use crate::tui::support::keymap::RuntimeKeymap;
use crate::tui::support::keymap::VimNormalKeymap;
use crate::tui::support::keymap::VimOperatorKeymap;
use crate::tui::support::keymap::VimSearchKeymap;
use crate::tui::support::keymap::VimTextObjectKeymap;
use crate::tui::support::width::display_width;
use crate::tui::user_input::ByteRange;
use crate::tui::user_input::TextElement as UserTextElement;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Span;
use ratatui::widgets::StatefulWidgetRef;
use ratatui::widgets::WidgetRef;
use std::borrow::Cow;
use std::cell::OnceCell;
use std::cell::Ref;
use std::cell::RefCell;
use std::ops::Range;
use std::sync::Arc;
use unicode_segmentation::UnicodeSegmentation;

mod hyperlinks;
mod vim;
mod vim_commands;
mod vim_search;
mod wrapping;
use self::vim::VimMode;
use self::vim::VimMotion;
use self::vim::VimOperator;
use self::vim::VimPending;
use self::vim::VimTextObject;
use self::vim::VimTextObjectScope;
use self::vim_commands::VimAction;
use self::vim_commands::VimCommandState;
use self::vim_commands::VimEditTarget;
use self::vim_commands::VimInsertPosition;
pub(crate) use self::vim_commands::VimPersistentState;

const WORD_SEPARATORS: &str = "`~!@#$%^&*()-=+[{]}\\|;:'\",.<>/?";

fn is_word_separator(ch: char) -> bool {
    WORD_SEPARATORS.contains(ch)
}

fn split_word_pieces(run: &str) -> Vec<(usize, &str)> {
    let mut pieces = Vec::new();
    for (segment_start, segment) in run.split_word_bound_indices() {
        let mut piece_start = 0;
        let mut chars = segment.char_indices();
        let Some((_, first_char)) = chars.next() else {
            continue;
        };
        let mut in_separator = is_word_separator(first_char);

        for (idx, ch) in chars {
            let is_separator = is_word_separator(ch);
            if is_separator == in_separator {
                continue;
            }
            pieces.push((segment_start + piece_start, &segment[piece_start..idx]));
            piece_start = idx;
            in_separator = is_separator;
        }

        pieces.push((segment_start + piece_start, &segment[piece_start..]));
    }

    pieces
}

/// Replace tabs with the one-column representation used for rendering and wrapping.
///
/// A tab and a space are both one byte, so ranges computed from this text still index the original
/// editable text.
fn text_for_display(text: &str) -> Cow<'_, str> {
    if text.contains('\t') {
        Cow::Owned(text.replace('\t', " "))
    } else {
        Cow::Borrowed(text)
    }
}

#[derive(Debug, Clone)]
struct TextElement {
    id: u64,
    range: Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TextElementSnapshot {
    pub(crate) id: u64,
    pub(crate) range: Range<usize>,
    pub(crate) text: String,
}

/// `TextArea` is the editable buffer behind the TUI composer.
///
/// It owns the raw UTF-8 text, placeholder-like text elements that must move atomically with
/// edits, cursor/wrapping state for rendering, and a single-entry kill buffer for `Ctrl+K` /
/// `Ctrl+Y` style editing. Callers may replace the entire visible buffer through
/// [`Self::set_text_clearing_elements`] or [`Self::set_text_with_elements`] without disturbing the
/// kill buffer; if they incorrectly assume those methods fully reset editing state, a later yank
/// will appear to restore stale text from the user's perspective.
#[derive(Debug)]
pub(crate) struct TextArea {
    text: String,
    cursor_pos: usize,
    wrap_cache: RefCell<Option<WrapCache>>,
    preferred_col: Option<usize>,
    elements: Vec<TextElement>,
    next_element_id: u64,
    kill_buffer: String,
    kill_buffer_kind: KillBufferKind,
    vim_enabled: bool,
    vim_mode: VimMode,
    vim_pending: VimPending,
    vim_commands: VimCommandState,
    vim_search: vim_search::VimSearch,
    vim_search_enabled: bool,
    editor_keymap: Arc<EditorKeymap>,
    vim_normal_keymap: VimNormalKeymap,
    vim_operator_keymap: VimOperatorKeymap,
    vim_search_keymap: VimSearchKeymap,
    vim_text_object_keymap: VimTextObjectKeymap,
}

#[derive(Debug, Clone)]
struct WrapCache {
    width: u16,
    lines: Vec<Range<usize>>,
    hyperlinks: OnceCell<hyperlinks::HyperlinkCache>,
}

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct TextAreaState {
    /// Index into wrapped lines of the first visible line.
    scroll: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KillBufferKind {
    /// Characterwise kills and yanks paste at the cursor.
    Characterwise,
    /// Linewise kills and yanks paste as whole lines below the cursor line.
    Linewise,
}

/// The last editor kill or Vim yank, carried between chat composers in the same TUI session.
pub(crate) struct KillBufferSnapshot {
    text: String,
    kind: KillBufferKind,
}

impl TextArea {
    pub fn new() -> Self {
        let defaults = RuntimeKeymap::defaults();
        Self {
            text: String::new(),
            cursor_pos: 0,
            wrap_cache: RefCell::new(None),
            preferred_col: None,
            elements: Vec::new(),
            next_element_id: 1,
            kill_buffer: String::new(),
            kill_buffer_kind: KillBufferKind::Characterwise,
            vim_enabled: false,
            vim_mode: VimMode::Insert,
            vim_pending: VimPending::None,
            vim_commands: VimCommandState::default(),
            vim_search: vim_search::VimSearch::default(),
            vim_search_enabled: false,
            editor_keymap: defaults.editor,
            vim_normal_keymap: defaults.vim_normal,
            vim_operator_keymap: defaults.vim_operator,
            vim_search_keymap: defaults.vim_search,
            vim_text_object_keymap: defaults.vim_text_object,
        }
    }

    /// Replace the editor and Vim keymaps used by subsequent text-editing input.
    ///
    /// This method intentionally swaps only the keymap caches. It does not
    /// reinterpret pending input, change Vim mode, move the cursor, or mutate
    /// the kill buffer, so callers can safely apply a live config update while
    /// preserving the current draft exactly as typed.
    pub fn set_keymap_bindings(&mut self, keymap: &RuntimeKeymap) {
        self.editor_keymap = Arc::clone(&keymap.editor);
        self.vim_normal_keymap = keymap.vim_normal.clone();
        self.vim_operator_keymap = keymap.vim_operator.clone();
        self.vim_search_keymap = keymap.vim_search.clone();
        self.vim_text_object_keymap = keymap.vim_text_object.clone();
    }

    /// Replace the visible textarea text and clear any existing text elements.
    ///
    /// This is the "fresh buffer" path for callers that want plain text with no placeholder
    /// ranges. It intentionally preserves the current kill buffer, because higher-level flows such
    /// as submit or slash-command dispatch clear the draft through this method and still want
    /// `Ctrl+Y` to recover the user's most recent kill.
    pub fn set_text_clearing_elements(&mut self, text: &str) {
        self.set_text_inner(text, /*elements*/ None);
    }

    /// Replace the visible textarea text and rebuild the provided text elements.
    ///
    /// As with [`Self::set_text_clearing_elements`], this resets only state derived from the
    /// visible buffer. The kill buffer survives so callers restoring drafts or external edits do
    /// not silently discard a pending yank target.
    pub fn set_text_with_elements(&mut self, text: &str, elements: &[UserTextElement]) {
        self.set_text_inner(text, Some(elements));
    }

    fn set_text_inner(&mut self, text: &str, elements: Option<&[UserTextElement]>) {
        // Stage 1: replace the raw text and keep the cursor in a safe byte range.
        self.text = text.to_string();
        self.cursor_pos = self.cursor_pos.clamp(0, self.text.len());
        // Stage 2: rebuild element ranges from scratch against the new text.
        self.elements.clear();
        if let Some(elements) = elements {
            for elem in elements {
                let mut start = elem.byte_range.start.min(self.text.len());
                let mut end = elem.byte_range.end.min(self.text.len());
                start = self.clamp_pos_to_char_boundary(start);
                end = self.clamp_pos_to_char_boundary(end);
                if start >= end {
                    continue;
                }
                let id = self.next_element_id();
                self.elements.push(TextElement {
                    id,
                    range: start..end,
                });
            }
            self.elements.sort_by_key(|e| e.range.start);
        }
        // Stage 3: clamp the cursor and reset derived state tied to the prior content.
        // The kill buffer is editing history rather than visible-buffer state, so full-buffer
        // replacements intentionally leave it alone.
        self.cursor_pos = self.clamp_pos_to_nearest_boundary(self.cursor_pos);
        self.wrap_cache.replace(None);
        self.preferred_col = None;
        self.vim_pending = VimPending::None;
        self.vim_search = vim_search::VimSearch::default();
        self.vim_commands = VimCommandState::default();
    }

    /// Enable or disable modal Vim editing for the textarea.
    ///
    /// Enabling always enters normal mode and disabling always returns to
    /// insert semantics. Pending operators are cleared in both directions so a
    /// toggle cannot leave the next keypress interpreted as the second half of
    /// an old `d` or `y` command.
    pub(crate) fn set_vim_enabled(&mut self, enabled: bool) {
        self.vim_enabled = enabled;
        self.vim_pending = VimPending::None;
        self.vim_search = vim_search::VimSearch::default();
        self.vim_commands = VimCommandState::default();
        self.vim_mode = if enabled {
            VimMode::Normal
        } else {
            VimMode::Insert
        };
    }

    /// Return whether modal Vim editing is currently enabled.
    pub(crate) fn is_vim_enabled(&self) -> bool {
        self.vim_enabled
    }

    /// Return whether Vim mode is enabled and currently waiting in normal mode.
    ///
    /// Composer-level handlers use this to decide whether Up/Down should be
    /// offered to history navigation only after normal-mode movement reaches a
    /// text boundary.
    pub(crate) fn is_vim_normal_mode(&self) -> bool {
        self.vim_enabled && self.vim_mode == VimMode::Normal
    }

    /// Return the cursor position that represents the last editable item in Vim normal mode.
    pub(crate) fn vim_normal_end_cursor(&self) -> usize {
        if self.text.is_empty() {
            0
        } else {
            self.prev_atomic_boundary(self.text.len())
        }
    }

    /// Return whether a Vim operator is waiting for a motion.
    ///
    /// This is observable so the composer can avoid stealing the second key of
    /// `d{motion}` or `y{motion}` for higher-level shortcuts.
    pub(crate) fn is_vim_operator_pending(&self) -> bool {
        self.vim_query().is_some() || !matches!(self.vim_pending, VimPending::None)
    }

    /// Return the keymap context that owns the next editing key.
    pub(crate) fn keymap_context(&self) -> KeymapContext {
        if !self.vim_enabled
            || matches!(self.vim_mode, VimMode::Insert | VimMode::Replace)
            || self.vim_query().is_some()
        {
            return KeymapContext::Editor;
        }
        match self.vim_pending {
            VimPending::None => KeymapContext::VimNormal,
            VimPending::Replace | VimPending::Find { .. } => KeymapContext::Editor,
            VimPending::Operator(_) => KeymapContext::VimOperator,
            VimPending::TextObject { .. } => KeymapContext::VimTextObject,
        }
    }

    /// Enter Vim insert mode if modal editing is enabled.
    ///
    /// Calling this while Vim is disabled is a no-op, which lets parent
    /// workflows reset mode after submissions without first branching on the
    /// current keymap state.
    pub(crate) fn enter_vim_insert_mode(&mut self) {
        if self.vim_enabled {
            self.vim_mode = VimMode::Insert;
            self.vim_pending = VimPending::None;
            self.clear_vim_replace_recovery();
            self.cancel_vim_search();
            if self.vim_commands.pending_change.is_empty() && !self.vim_commands.replaying {
                self.start_vim_edit(VimAction::Insert(VimInsertPosition::Cursor));
            }
        }
    }

    /// Enter Vim normal mode if modal editing is enabled.
    ///
    /// This clears any pending operator and preferred vertical column. The
    /// latter matches normal Vim navigation expectations after leaving insert
    /// mode; preserving the old column would make the next `j` or `k` jump to a
    /// stale visual target.
    pub(crate) fn enter_vim_normal_mode(&mut self) {
        if self.vim_enabled {
            self.vim_mode = VimMode::Normal;
            self.vim_pending = VimPending::None;
            self.cancel_vim_search();
            self.preferred_col = None;
            self.clear_vim_replace_recovery();
        }
    }

    /// Return whether rapid plain-key bursts should be treated as paste input.
    ///
    /// Paste burst detection is disabled in Vim normal mode so a fast sequence
    /// like `dd` or `yw` remains command input instead of being converted into
    /// literal text.
    pub(crate) fn allows_paste_burst(&self) -> bool {
        !self.vim_enabled || matches!(self.vim_mode, VimMode::Insert | VimMode::Replace)
    }

    /// Return whether rendering should use the insert-mode cursor style.
    pub(crate) fn uses_vim_insert_cursor(&self) -> bool {
        self.vim_enabled && self.vim_mode == VimMode::Insert
    }

    /// Return whether Escape should be intercepted before composer-level routing.
    ///
    /// In Vim insert mode or while a command is pending, Escape is an editing
    /// transition rather than a popup cancel/backtrack or turn-interrupt shortcut.
    pub(crate) fn should_handle_vim_insert_escape(&self, event: KeyEvent) -> bool {
        self.vim_enabled
            && (matches!(self.vim_mode, VimMode::Insert | VimMode::Replace)
                || self.is_vim_operator_pending())
            && event.code == KeyCode::Esc
            && event.modifiers == KeyModifiers::NONE
            && matches!(event.kind, KeyEventKind::Press | KeyEventKind::Repeat)
    }

    /// Return the footer label for the active Vim mode.
    ///
    /// `None` means Vim editing is disabled, so callers should omit the mode
    /// indicator rather than rendering an insert-mode label for normal
    /// non-modal editing.
    #[cfg(test)]
    pub(crate) fn vim_mode_label(&self) -> Option<&'static str> {
        if !self.vim_enabled {
            return None;
        }
        Some(match self.vim_mode {
            VimMode::Normal => "Normal",
            VimMode::Insert => "Insert",
            VimMode::Replace => "Replace",
        })
    }

    /// Return the styled footer indicator for the active Vim editing mode.
    pub(crate) fn vim_mode_indicator_span(&self) -> Option<Span<'static>> {
        if !self.vim_enabled {
            return None;
        }
        Some(match self.vim_mode {
            VimMode::Normal => "Vim: Normal".magenta(),
            VimMode::Insert => "Vim: Insert".green(),
            VimMode::Replace => "Vim: Replace".cyan(),
        })
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn insert_str(&mut self, text: &str) {
        self.record_vim_inserted_text(text);
        if self.is_vim_replace_mode() {
            self.replace_vim_text(text);
        } else {
            self.insert_str_at(self.cursor_pos, text);
        }
    }

    pub fn insert_str_at(&mut self, pos: usize, text: &str) {
        self.clear_vim_replace_recovery();
        let pos = self.clamp_pos_for_insertion(pos);
        self.text.insert_str(pos, text);
        self.wrap_cache.replace(None);
        if pos <= self.cursor_pos {
            self.cursor_pos += text.len();
        }
        self.shift_elements(pos, /*removed*/ 0, text.len());
        self.preferred_col = None;
    }

    pub fn replace_range(&mut self, range: std::ops::Range<usize>, text: &str) {
        self.clear_vim_replace_recovery();
        self.replace_range_preserving_recovery(range, text);
    }

    // Replace typing and Backspace keep contiguous recovery, but still respect atomic elements.
    fn replace_range_preserving_recovery(&mut self, range: Range<usize>, text: &str) {
        let range = self.expand_range_to_element_boundaries(range);
        self.replace_range_raw(range, text);
    }

    fn replace_range_raw(&mut self, range: std::ops::Range<usize>, text: &str) {
        assert!(range.start <= range.end);
        let start = range.start.clamp(0, self.text.len());
        let end = range.end.clamp(0, self.text.len());
        let removed_len = end - start;
        let inserted_len = text.len();
        if removed_len == 0 && inserted_len == 0 {
            return;
        }
        let diff = inserted_len as isize - removed_len as isize;

        self.text.replace_range(range, text);
        self.wrap_cache.replace(None);
        self.preferred_col = None;
        self.update_elements_after_replace(start, end, inserted_len);

        // Update the cursor position to account for the edit.
        self.cursor_pos = if self.cursor_pos < start {
            // Cursor was before the edited range – no shift.
            self.cursor_pos
        } else if self.cursor_pos <= end {
            // Cursor was inside the replaced range – move to end of the new text.
            start + inserted_len
        } else {
            // Cursor was after the replaced range – shift by the length diff.
            ((self.cursor_pos as isize) + diff) as usize
        }
        .min(self.text.len());

        // Ensure cursor is not inside an element
        self.cursor_pos = self.clamp_pos_to_nearest_boundary(self.cursor_pos);
    }

    pub fn cursor(&self) -> usize {
        self.cursor_pos
    }

    pub fn set_cursor(&mut self, pos: usize) {
        self.cursor_pos = pos.clamp(0, self.text.len());
        self.cursor_pos = self.clamp_pos_to_nearest_boundary(self.cursor_pos);
        self.preferred_col = None;
    }

    pub fn desired_height(&self, width: u16) -> u16 {
        self.wrapped_lines(width).len() as u16
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        self.cursor_pos_with_state(area, TextAreaState::default())
    }

    /// Returns an on-screen cursor position within `area`, accounting for wrapping and scrolling.
    ///
    /// Returns `None` when the viewport has no visible cells.
    pub fn cursor_pos_with_state(&self, area: Rect, state: TextAreaState) -> Option<(u16, u16)> {
        if area.is_empty() {
            return None;
        }

        let lines = self.wrapped_lines(area.width);
        let effective_scroll = self.effective_scroll(area, &lines, state.scroll);
        let (i, col) = wrapping::cursor_position(&self.text, &lines, area.width, self.cursor_pos)?;
        let screen_row = i
            .saturating_sub(effective_scroll as usize)
            .try_into()
            .unwrap_or(0);
        Some((area.x + col as u16, area.y + screen_row))
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    fn current_display_col(&self) -> usize {
        let bol = self.beginning_of_current_line();
        display_width(&self.text[bol..self.cursor_pos])
    }

    fn move_to_display_col_on_line(
        &mut self,
        line_start: usize,
        line_end: usize,
        target_col: usize,
    ) {
        let mut width_so_far = 0usize;
        for (i, g) in self.text[line_start..line_end].grapheme_indices(true) {
            width_so_far += display_width(g);
            if width_so_far > target_col {
                self.cursor_pos = line_start + i;
                // Avoid landing inside an element; round to nearest boundary
                self.cursor_pos = self.clamp_pos_to_nearest_boundary(self.cursor_pos);
                return;
            }
        }
        self.cursor_pos = line_end;
        self.cursor_pos = self.clamp_pos_to_nearest_boundary(self.cursor_pos);
    }

    fn beginning_of_line(&self, pos: usize) -> usize {
        self.text[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0)
    }
    fn beginning_of_current_line(&self) -> usize {
        self.beginning_of_line(self.cursor_pos)
    }

    fn first_non_blank_of_current_line(&self) -> usize {
        let bol = self.beginning_of_current_line();
        let eol = self.end_of_current_line();
        self.text[bol..eol]
            .char_indices()
            .find_map(|(offset, ch)| (!ch.is_whitespace()).then_some(bol + offset))
            .unwrap_or(eol)
    }

    fn end_of_line(&self, pos: usize) -> usize {
        self.text[pos..]
            .find('\n')
            .map(|i| i + pos)
            .unwrap_or(self.text.len())
    }
    fn end_of_current_line(&self) -> usize {
        self.end_of_line(self.cursor_pos)
    }

    pub fn input(&mut self, event: KeyEvent) {
        // Only process key presses or repeats; ignore releases to avoid inserting
        // characters on key-up events when modifiers are no longer reported.
        if !matches!(event.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return;
        }
        if self.vim_enabled {
            self.handle_vim_input(event);
        } else {
            let keymap = self.editor_keymap.clone();
            self.input_with_keymap(event, &keymap);
        }
    }

    pub fn input_with_keymap(&mut self, event: KeyEvent, keymap: &EditorKeymap) {
        if keymap.insert_newline.is_pressed(event) {
            self.insert_str("\n");
            return;
        }

        if keymap.delete_backward_word.is_pressed(event) {
            self.apply_vim_insert_action(VimAction::DeleteBackwardWord);
            return;
        }

        // Windows AltGr generates ALT|CONTROL. Preserve typed characters for AltGr users
        // unless a specific shortcut already matched above.
        if let KeyEvent {
            code: KeyCode::Char(c),
            modifiers,
            ..
        } = event
            && is_altgr(modifiers)
        {
            self.insert_str(&c.to_string());
            return;
        }

        if keymap.delete_backward.is_pressed(event) {
            self.apply_vim_insert_action(VimAction::DeleteBackward);
            return;
        }
        if keymap.delete_forward_word.is_pressed(event) {
            self.apply_vim_insert_action(VimAction::DeleteForwardWord);
            return;
        }
        if keymap.delete_forward.is_pressed(event) {
            self.apply_vim_insert_action(VimAction::DeleteForward);
            return;
        }
        if keymap.kill_line_start.is_pressed(event) {
            self.apply_vim_insert_action(VimAction::KillLineStart);
            return;
        }
        if keymap.kill_whole_line.is_pressed(event) {
            self.apply_vim_insert_action(VimAction::KillLine);
            return;
        }
        if keymap.kill_line_end.is_pressed(event) {
            self.apply_vim_insert_action(VimAction::KillLineEnd);
            return;
        }
        if keymap.yank.is_pressed(event) {
            self.yank();
            return;
        }
        if keymap.move_word_left.is_pressed(event) {
            self.apply_vim_insert_action(VimAction::MoveWordLeft);
            return;
        }
        if keymap.move_word_right.is_pressed(event) {
            self.apply_vim_insert_action(VimAction::MoveWordRight);
            return;
        }
        if keymap.move_left.is_pressed(event) {
            self.apply_vim_insert_action(VimAction::MoveLeft);
            return;
        }
        if keymap.move_right.is_pressed(event) {
            self.apply_vim_insert_action(VimAction::MoveRight);
            return;
        }
        if keymap.move_up.is_pressed(event) {
            self.apply_vim_insert_action(VimAction::MoveUp);
            return;
        }
        if keymap.move_down.is_pressed(event) {
            self.apply_vim_insert_action(VimAction::MoveDown);
            return;
        }
        if keymap.move_line_start.is_pressed(event) {
            let move_up_at_bol = matches!(
                event,
                KeyEvent {
                    code: KeyCode::Char('a'),
                    modifiers: KeyModifiers::CONTROL,
                    ..
                }
            );
            self.apply_vim_insert_action(VimAction::MoveLineStart { move_up_at_bol });
            return;
        }
        if keymap.move_line_end.is_pressed(event) {
            let move_down_at_eol = matches!(
                event,
                KeyEvent {
                    code: KeyCode::Char('e'),
                    modifiers: KeyModifiers::CONTROL,
                    ..
                }
            );
            self.apply_vim_insert_action(VimAction::MoveLineEnd { move_down_at_eol });
            return;
        }

        if let KeyEvent {
            code: KeyCode::Char(c),
            modifiers: KeyModifiers::NONE | KeyModifiers::SHIFT,
            ..
        } = event
        {
            // Insert plain characters (and Shift-modified). Do not insert when ALT is held,
            // because many terminals map Option/Meta combos to ALT+<char>.
            if c.is_ascii_control() {
                return;
            }
            self.insert_str(&c.to_string());
        }

        tracing::debug!("Unhandled key event in TextArea: {:?}", event);
    }

    fn handle_vim_input(&mut self, event: KeyEvent) {
        if self.handle_vim_search_key(event) {
            return;
        }
        let prior_mode = self.vim_mode;
        match self.vim_mode {
            VimMode::Insert | VimMode::Replace => self.handle_vim_insert(event),
            VimMode::Normal => self.handle_vim_normal(event),
        }
        if matches!(prior_mode, VimMode::Insert | VimMode::Replace)
            && self.vim_mode == VimMode::Normal
        {
            self.finish_pending_vim_change();
        }
    }

    fn handle_vim_insert(&mut self, event: KeyEvent) {
        if matches!(event.code, KeyCode::Esc) {
            self.leave_vim_insert_mode();
            return;
        }
        if self.is_vim_replace_mode()
            && self.editor_keymap.delete_backward.is_pressed(event)
            && self.apply_vim_insert_action(VimAction::RestoreReplacedCharacter)
        {
            return;
        }
        let keymap = self.editor_keymap.clone();
        self.input_with_keymap(event, &keymap);
    }

    fn leave_vim_insert_mode(&mut self) {
        let bol = self.beginning_of_current_line();
        if self.cursor_pos > bol {
            self.cursor_pos = self.prev_atomic_boundary(self.cursor_pos).max(bol);
        }
        self.enter_vim_normal_mode();
    }

    fn handle_vim_normal(&mut self, event: KeyEvent) {
        let pending = std::mem::replace(&mut self.vim_pending, VimPending::None);
        match pending {
            VimPending::None => {}
            VimPending::Operator(op) => {
                self.handle_vim_operator(op, event);
                return;
            }
            VimPending::TextObject { operator, scope } => {
                self.handle_vim_text_object(operator, scope, event);
                return;
            }
            VimPending::Replace | VimPending::Find { .. } => {
                self.handle_vim_pending_command(pending, event);
                return;
            }
        }

        if self.vim_normal_keymap.enter_insert.is_pressed(event) {
            self.start_vim_edit(VimAction::Insert(VimInsertPosition::Cursor));
            return;
        }
        if self.vim_normal_keymap.append_after_cursor.is_pressed(event) {
            self.start_vim_edit(VimAction::Insert(VimInsertPosition::AfterCursor));
            return;
        }
        if self.vim_normal_keymap.append_line_end.is_pressed(event) {
            self.start_vim_edit(VimAction::Insert(VimInsertPosition::LineEnd));
            return;
        }
        if self.vim_normal_keymap.insert_line_start.is_pressed(event) {
            self.start_vim_edit(VimAction::Insert(VimInsertPosition::LineStart));
            return;
        }
        if self.vim_normal_keymap.open_line_below.is_pressed(event) {
            self.start_vim_edit(VimAction::Insert(VimInsertPosition::OpenBelow));
            return;
        }
        if self.vim_normal_keymap.open_line_above.is_pressed(event) {
            self.start_vim_edit(VimAction::Insert(VimInsertPosition::OpenAbove));
            return;
        }
        if self.vim_normal_keymap.move_left.is_pressed(event) {
            self.move_cursor_left();
            return;
        }
        if self.vim_normal_keymap.move_right.is_pressed(event) {
            self.move_cursor_right();
            return;
        }
        if self.vim_normal_keymap.move_down.is_pressed(event) {
            self.move_cursor_down();
            return;
        }
        if self.vim_normal_keymap.move_up.is_pressed(event) {
            self.move_cursor_up();
            return;
        }
        if self.vim_normal_keymap.move_word_forward.is_pressed(event) {
            self.set_cursor(self.beginning_of_next_word());
            return;
        }
        if self.vim_normal_keymap.move_word_backward.is_pressed(event) {
            self.set_cursor(self.beginning_of_previous_word());
            return;
        }
        if self.vim_normal_keymap.move_word_end.is_pressed(event) {
            self.set_cursor(self.vim_word_end_cursor());
            return;
        }
        if self.vim_normal_keymap.move_line_start.is_pressed(event) {
            self.set_cursor(self.beginning_of_current_line());
            return;
        }
        if self.vim_normal_keymap.move_line_end.is_pressed(event) {
            self.set_cursor(self.vim_line_end_cursor());
            return;
        }
        if self.vim_normal_keymap.delete_char.is_pressed(event) {
            self.start_vim_edit(VimAction::Delete(VimEditTarget::Character));
            return;
        }
        if self.vim_normal_keymap.substitute_char.is_pressed(event) {
            self.start_vim_edit(VimAction::Change(VimEditTarget::Character));
            return;
        }
        if self.vim_normal_keymap.delete_to_line_end.is_pressed(event) {
            self.start_vim_edit(VimAction::Delete(VimEditTarget::LineEnd));
            return;
        }
        if self.vim_normal_keymap.change_to_line_end.is_pressed(event) {
            self.start_vim_edit(VimAction::Change(VimEditTarget::LineEnd));
            return;
        }
        if self.vim_normal_keymap.yank_line.is_pressed(event) {
            self.yank_current_line();
            return;
        }
        if self.vim_normal_keymap.paste_after.is_pressed(event) {
            self.start_vim_edit(VimAction::PasteAfter);
            return;
        }
        if self
            .vim_normal_keymap
            .start_delete_operator
            .is_pressed(event)
        {
            self.vim_pending = VimPending::Operator(VimOperator::Delete);
            return;
        }
        if self.vim_normal_keymap.start_yank_operator.is_pressed(event) {
            self.vim_pending = VimPending::Operator(VimOperator::Yank);
            return;
        }
        if self
            .vim_normal_keymap
            .start_change_operator
            .is_pressed(event)
        {
            self.vim_pending = VimPending::Operator(VimOperator::Change);
            return;
        }
        if self.vim_normal_keymap.cancel_operator.is_pressed(event) {
            self.vim_pending = VimPending::None;
            self.cancel_vim_search();
            return;
        }
        self.handle_vim_extra_command(event);
    }

    fn handle_vim_operator(&mut self, op: VimOperator, event: KeyEvent) -> bool {
        if op == VimOperator::Delete && self.vim_operator_keymap.delete_line.is_pressed(event) {
            self.start_vim_edit(VimAction::Delete(VimEditTarget::Line));
            return true;
        }
        if op == VimOperator::Yank && self.vim_operator_keymap.yank_line.is_pressed(event) {
            self.yank_current_line();
            return true;
        }
        if self.vim_operator_keymap.cancel.is_pressed(event) {
            return true;
        }
        if let Some(scope) = self.vim_text_object_scope_for_event(event) {
            self.vim_pending = VimPending::TextObject {
                operator: op,
                scope,
            };
            return true;
        }

        if let Some(motion) = self.vim_motion_for_event(event) {
            match op {
                VimOperator::Delete => {
                    self.start_vim_edit(VimAction::Delete(VimEditTarget::Motion(motion)));
                }
                VimOperator::Change => {
                    self.start_vim_edit(VimAction::Change(VimEditTarget::Motion(motion)));
                }
                VimOperator::Yank => self.apply_vim_operator(op, motion),
            }
            return true;
        }
        if op == VimOperator::Change
            && self
                .vim_normal_keymap
                .start_change_operator
                .is_pressed(event)
        {
            self.start_vim_edit(VimAction::Change(VimEditTarget::Line));
            return true;
        }
        self.handle_vim_operator_command(op, event)
    }

    fn handle_vim_text_object(
        &mut self,
        op: VimOperator,
        scope: VimTextObjectScope,
        event: KeyEvent,
    ) -> bool {
        if self.vim_text_object_keymap.cancel.is_pressed(event) {
            return true;
        }
        let Some(object) = self.vim_text_object_for_event(event) else {
            return false;
        };
        match op {
            VimOperator::Delete => {
                self.start_vim_edit(VimAction::Delete(VimEditTarget::TextObject {
                    scope,
                    object,
                }));
            }
            VimOperator::Change => {
                self.start_vim_edit(VimAction::Change(VimEditTarget::TextObject {
                    scope,
                    object,
                }));
            }
            VimOperator::Yank => {
                if let Some(range) = self.text_object_range(object, scope) {
                    self.apply_vim_operator_to_range(op, range);
                }
            }
        }
        true
    }

    fn vim_motion_for_event(&self, event: KeyEvent) -> Option<VimMotion> {
        if self.vim_operator_keymap.motion_left.is_pressed(event) {
            return Some(VimMotion::Left);
        }
        if self.vim_operator_keymap.motion_right.is_pressed(event) {
            return Some(VimMotion::Right);
        }
        if self.vim_operator_keymap.motion_down.is_pressed(event) {
            return Some(VimMotion::Down);
        }
        if self.vim_operator_keymap.motion_up.is_pressed(event) {
            return Some(VimMotion::Up);
        }
        if self
            .vim_operator_keymap
            .motion_word_forward
            .is_pressed(event)
        {
            return Some(VimMotion::WordForward);
        }
        if self
            .vim_operator_keymap
            .motion_word_backward
            .is_pressed(event)
        {
            return Some(VimMotion::WordBackward);
        }
        if self.vim_operator_keymap.motion_word_end.is_pressed(event) {
            return Some(VimMotion::WordEnd);
        }
        if self.vim_operator_keymap.motion_line_start.is_pressed(event) {
            return Some(VimMotion::LineStart);
        }
        if self.vim_operator_keymap.motion_line_end.is_pressed(event) {
            return Some(VimMotion::LineEnd);
        }
        None
    }

    fn apply_vim_operator(&mut self, op: VimOperator, motion: VimMotion) {
        if op == VimOperator::Change && motion == VimMotion::WordForward {
            let target = if self.text[self.cursor_pos..]
                .chars()
                .next()
                .is_some_and(|ch| !ch.is_whitespace())
            {
                self.end_of_next_word()
            } else {
                self.beginning_of_next_word()
                    .min(self.end_of_current_line())
            };
            if target > self.cursor_pos {
                self.apply_vim_operator_to_range(op, self.cursor_pos..target);
            } else {
                self.vim_mode = VimMode::Insert;
            }
            return;
        }
        let Some(range) = self.range_for_motion(motion) else {
            if op == VimOperator::Change && motion == VimMotion::LineEnd {
                self.vim_mode = VimMode::Insert;
            }
            return;
        };
        if op == VimOperator::Change && matches!(motion, VimMotion::Up | VimMotion::Down) {
            if motion == VimMotion::Up && self.beginning_of_current_line() == 0
                || motion == VimMotion::Down && self.end_of_current_line() == self.text.len()
            {
                return;
            }
            let retain_newline =
                range.end < self.text.len() && self.text[range.clone()].ends_with('\n');
            let start = range.start;
            self.kill_line_range(range);
            if retain_newline {
                self.insert_str_at(start, "\n");
                self.set_cursor(start);
            }
            self.vim_mode = VimMode::Insert;
            return;
        }
        self.apply_vim_operator_to_range(op, range);
    }

    fn apply_vim_operator_to_range(&mut self, op: VimOperator, range: Range<usize>) {
        match op {
            VimOperator::Delete => self.kill_range(range),
            VimOperator::Yank => self.yank_range(range),
            VimOperator::Change => {
                self.kill_range(range);
                self.vim_mode = VimMode::Insert;
            }
        }
    }

    fn range_for_motion(&mut self, motion: VimMotion) -> Option<Range<usize>> {
        if matches!(motion, VimMotion::Up | VimMotion::Down) {
            return self.linewise_range_for_vertical_motion(motion);
        }
        let start = self.cursor_pos;
        let target = self.target_for_motion(motion);
        if start == target {
            return None;
        }
        let (range_start, range_end) = if target < start {
            (target, start)
        } else {
            (start, target)
        };
        Some(range_start..range_end)
    }

    fn linewise_range_for_vertical_motion(&self, motion: VimMotion) -> Option<Range<usize>> {
        let current = self.current_line_range_with_newline();
        let range = match motion {
            VimMotion::Up => {
                let start = if current.start == 0 {
                    current.start
                } else {
                    self.beginning_of_line(current.start.saturating_sub(1))
                };
                start..current.end
            }
            VimMotion::Down => {
                let end = if current.end >= self.text.len() {
                    current.end
                } else {
                    let next_eol = self.end_of_line(current.end);
                    if next_eol < self.text.len() {
                        next_eol + 1
                    } else {
                        next_eol
                    }
                };
                current.start..end
            }
            VimMotion::Left
            | VimMotion::Right
            | VimMotion::WordForward
            | VimMotion::WordBackward
            | VimMotion::WordEnd
            | VimMotion::LineStart
            | VimMotion::LineEnd => return None,
        };
        (range.start < range.end).then_some(range)
    }

    fn target_for_motion(&mut self, motion: VimMotion) -> usize {
        let original_cursor = self.cursor_pos;
        let original_preferred = self.preferred_col;
        match motion {
            VimMotion::Left => self.move_cursor_left(),
            VimMotion::Right => self.move_cursor_right(),
            VimMotion::Up => self.move_cursor_up(),
            VimMotion::Down => self.move_cursor_down(),
            VimMotion::WordForward => self.set_cursor(self.beginning_of_next_word()),
            VimMotion::WordBackward => self.set_cursor(self.beginning_of_previous_word()),
            VimMotion::WordEnd => self.set_cursor(self.vim_word_end_exclusive()),
            VimMotion::LineStart => self.set_cursor(self.beginning_of_current_line()),
            VimMotion::LineEnd => self.set_cursor(self.end_of_current_line()),
        }
        let target = self.cursor_pos;
        self.cursor_pos = original_cursor;
        self.preferred_col = original_preferred;
        target
    }

    // ####### Input Functions #######
    pub fn delete_backward(&mut self, n: usize) {
        if n == 0 || self.cursor_pos == 0 {
            return;
        }
        let mut target = self.cursor_pos;
        for _ in 0..n {
            if let Some((boundary, ch)) = self.text[..target].char_indices().next_back()
                && matches!(
                    ch,
                    // Thai-only special casing is not ideal; refactor if it becomes more complex.
                    // All Thai nonspacing marks: vowel signs, tone marks, and other diacritics.
                    '\u{0e31}' | '\u{0e34}'..='\u{0e3a}' | '\u{0e47}'..='\u{0e4e}'
                )
                && !self
                    .elements
                    .iter()
                    .any(|element| target > element.range.start && target <= element.range.end)
            {
                target = boundary;
            } else {
                target = self.prev_atomic_boundary(target);
            }
            if target == 0 {
                break;
            }
        }
        self.replace_range(target..self.cursor_pos, "");
    }

    pub fn delete_forward(&mut self, n: usize) {
        if n == 0 || self.cursor_pos >= self.text.len() {
            return;
        }
        let mut target = self.cursor_pos;
        for _ in 0..n {
            target = self.next_atomic_boundary(target);
            if target >= self.text.len() {
                break;
            }
        }
        self.replace_range(self.cursor_pos..target, "");
    }

    pub fn delete_forward_kill(&mut self, n: usize) {
        if n == 0 || self.cursor_pos >= self.text.len() {
            return;
        }
        let mut target = self.cursor_pos;
        for _ in 0..n {
            target = self.next_atomic_boundary(target);
            if target >= self.text.len() {
                break;
            }
        }
        self.kill_range(self.cursor_pos..target);
    }

    pub fn delete_backward_word(&mut self) {
        let start = self.beginning_of_previous_word();
        self.kill_range(start..self.cursor_pos);
    }

    /// Delete text to the right of the cursor using "word" semantics.
    ///
    /// Deletes from the current cursor position through the end of the next word as determined
    /// by `end_of_next_word()`. Any whitespace (including newlines) between the cursor and that
    /// word is included in the deletion.
    pub fn delete_forward_word(&mut self) {
        let end = self.end_of_next_word();
        if end > self.cursor_pos {
            self.kill_range(self.cursor_pos..end);
        }
    }

    /// Kill from the cursor to the end of the current logical line.
    ///
    /// If the cursor is already at end-of-line and a trailing newline exists, this kills that
    /// newline so repeated invocations continue making progress. The removed text becomes the next
    /// yank target and remains available even if a caller later clears or rewrites the visible
    /// buffer via `set_text_*`.
    pub fn kill_to_end_of_line(&mut self) {
        let eol = self.end_of_current_line();
        let range = if self.cursor_pos == eol {
            if eol < self.text.len() {
                Some(self.cursor_pos..eol + 1)
            } else {
                None
            }
        } else {
            Some(self.cursor_pos..eol)
        };

        if let Some(range) = range {
            self.kill_range(range);
        }
    }

    fn vim_kill_to_end_of_line(&mut self) {
        let eol = self.end_of_current_line();
        if self.cursor_pos < eol {
            self.kill_range(self.cursor_pos..eol);
        }
    }

    pub fn kill_to_beginning_of_line(&mut self) {
        let bol = self.beginning_of_current_line();
        let range = if self.cursor_pos == bol {
            if bol > 0 { Some(bol - 1..bol) } else { None }
        } else {
            Some(bol..self.cursor_pos)
        };

        if let Some(range) = range {
            self.kill_range(range);
        }
    }

    /// Insert the most recently killed text at the cursor.
    ///
    /// This uses the textarea's single-entry kill buffer. Because whole-buffer replacement APIs do
    /// not clear that buffer, `yank` can restore text after composer-level clears such as submit
    /// and slash-command dispatch.
    pub fn yank(&mut self) {
        if self.kill_buffer.is_empty() {
            return;
        }
        let text = self.kill_buffer.clone();
        self.insert_str(&text);
    }

    pub(crate) fn take_kill_buffer_snapshot(&mut self) -> KillBufferSnapshot {
        KillBufferSnapshot {
            text: std::mem::take(&mut self.kill_buffer),
            kind: self.kill_buffer_kind,
        }
    }

    pub(crate) fn restore_kill_buffer_snapshot(&mut self, snapshot: KillBufferSnapshot) {
        self.kill_buffer = snapshot.text;
        self.kill_buffer_kind = snapshot.kind;
    }

    fn kill_range(&mut self, range: Range<usize>) {
        self.kill_range_with_kind(range, KillBufferKind::Characterwise);
    }

    fn kill_line_range(&mut self, range: Range<usize>) {
        self.kill_range_with_kind(range, KillBufferKind::Linewise);
    }

    fn kill_range_with_kind(&mut self, range: Range<usize>, kind: KillBufferKind) {
        let range = self.expand_range_to_element_boundaries(range);
        if range.start >= range.end {
            return;
        }

        let removed = self.text[range.clone()].to_string();
        if removed.is_empty() {
            return;
        }

        self.store_kill_buffer(removed, kind);
        self.replace_range_raw(range, "");
    }

    fn yank_range(&mut self, range: Range<usize>) {
        self.yank_range_with_kind(range, KillBufferKind::Characterwise);
    }

    fn yank_line_range(&mut self, range: Range<usize>) {
        self.yank_range_with_kind(range, KillBufferKind::Linewise);
    }

    fn yank_range_with_kind(&mut self, range: Range<usize>, kind: KillBufferKind) {
        let range = self.expand_range_to_element_boundaries(range);
        if range.start >= range.end {
            return;
        }
        let removed = self.text[range].to_string();
        if removed.is_empty() {
            return;
        }
        self.store_kill_buffer(removed, kind);
    }

    fn store_kill_buffer(&mut self, text: String, kind: KillBufferKind) {
        self.kill_buffer = text;
        self.kill_buffer_kind = kind;
    }

    fn paste_after_cursor(&mut self) {
        if self.kill_buffer.is_empty() {
            return;
        }
        if self.kill_buffer_kind == KillBufferKind::Linewise {
            self.paste_line_after_current_line();
            return;
        }
        let insert_at = self.next_atomic_boundary(self.cursor_pos);
        self.set_cursor(insert_at);
        let text = self.kill_buffer.clone();
        self.insert_str(&text);
    }

    fn paste_line_after_current_line(&mut self) {
        let eol = self.end_of_current_line();
        let insert_at = if eol < self.text.len() { eol + 1 } else { eol };
        let cursor = if eol < self.text.len() {
            insert_at
        } else {
            insert_at + 1
        };
        let text = if eol < self.text.len() {
            if self.kill_buffer.ends_with('\n') {
                self.kill_buffer.clone()
            } else {
                format!("{}\n", self.kill_buffer)
            }
        } else {
            format!("\n{}", self.kill_buffer.trim_end_matches('\n'))
        };
        self.insert_str_at(insert_at, &text);
        self.set_cursor(cursor.min(self.text.len()));
    }

    fn yank_current_line(&mut self) {
        let range = self.current_line_range_with_newline();
        self.yank_line_range(range);
    }

    fn kill_current_line(&mut self) {
        let range = self.current_line_range_with_newline();
        self.kill_line_range(range);
    }

    fn current_line_range_with_newline(&self) -> Range<usize> {
        let bol = self.beginning_of_current_line();
        let eol = self.end_of_current_line();
        let end = if eol < self.text.len() { eol + 1 } else { eol };
        bol..end
    }

    /// Move the cursor left by a single grapheme cluster.
    pub fn move_cursor_left(&mut self) {
        self.cursor_pos = self.prev_atomic_boundary(self.cursor_pos);
        self.preferred_col = None;
    }

    /// Move the cursor right by a single grapheme cluster.
    pub fn move_cursor_right(&mut self) {
        self.cursor_pos = self.next_atomic_boundary(self.cursor_pos);
        self.preferred_col = None;
    }

    pub fn move_cursor_up(&mut self) {
        // If we have a wrapping cache, prefer navigating across wrapped (visual) lines.
        if let Some((target_col, maybe_line)) = {
            let cache_ref = self.wrap_cache.borrow();
            if let Some(cache) = cache_ref.as_ref() {
                let lines = &cache.lines;
                if let Some((idx, col)) =
                    wrapping::cursor_position(&self.text, lines, cache.width, self.cursor_pos)
                {
                    let cur_range = &lines[idx];
                    // A saved column can outlive a resize. Do not land in hanging whitespace
                    // that is displayed on the following row.
                    let target_col = self
                        .preferred_col
                        .unwrap_or(col)
                        .min(usize::from(cache.width.saturating_sub(1)));
                    if idx > 0 {
                        let prev = &lines[idx - 1];
                        let line_start = prev.start;
                        let mut line_end = prev.end.saturating_sub(1);
                        if line_end == cur_range.start {
                            line_end = self.prev_atomic_boundary(line_end).max(line_start);
                        }
                        Some((target_col, Some((line_start, line_end))))
                    } else {
                        Some((target_col, None))
                    }
                } else {
                    None
                }
            } else {
                None
            }
        } {
            // We had wrapping info. Apply movement accordingly.
            match maybe_line {
                Some((line_start, line_end)) => {
                    if self.preferred_col.is_none() {
                        self.preferred_col = Some(target_col);
                    }
                    self.move_to_display_col_on_line(line_start, line_end, target_col);
                    return;
                }
                None => {
                    // Already at first visual line -> move to start
                    self.cursor_pos = 0;
                    self.preferred_col = None;
                    return;
                }
            }
        }

        // Fallback to logical line navigation if we don't have wrapping info yet.
        if let Some(prev_nl) = self.text[..self.cursor_pos].rfind('\n') {
            let target_col = match self.preferred_col {
                Some(c) => c,
                None => {
                    let c = self.current_display_col();
                    self.preferred_col = Some(c);
                    c
                }
            };
            let prev_line_start = self.text[..prev_nl].rfind('\n').map(|i| i + 1).unwrap_or(0);
            let prev_line_end = prev_nl;
            self.move_to_display_col_on_line(prev_line_start, prev_line_end, target_col);
        } else {
            self.cursor_pos = 0;
            self.preferred_col = None;
        }
    }

    pub fn move_cursor_down(&mut self) {
        // If we have a wrapping cache, prefer navigating across wrapped (visual) lines.
        if let Some((target_col, move_to_last)) = {
            let cache_ref = self.wrap_cache.borrow();
            if let Some(cache) = cache_ref.as_ref() {
                let lines = &cache.lines;
                if let Some((idx, col)) =
                    wrapping::cursor_position(&self.text, lines, cache.width, self.cursor_pos)
                {
                    let target_col = self
                        .preferred_col
                        .unwrap_or(col)
                        .min(usize::from(cache.width.saturating_sub(1)));
                    if idx + 1 < lines.len() {
                        let next = &lines[idx + 1];
                        let line_start = next.start;
                        let mut line_end = next.end.saturating_sub(1);
                        if lines
                            .get(idx + 2)
                            .is_some_and(|following| following.start == line_end)
                        {
                            line_end = self.prev_atomic_boundary(line_end).max(line_start);
                        }
                        Some((target_col, Some((line_start, line_end))))
                    } else {
                        Some((target_col, None))
                    }
                } else {
                    None
                }
            } else {
                None
            }
        } {
            match move_to_last {
                Some((line_start, line_end)) => {
                    if self.preferred_col.is_none() {
                        self.preferred_col = Some(target_col);
                    }
                    self.move_to_display_col_on_line(line_start, line_end, target_col);
                    return;
                }
                None => {
                    // Already on last visual line -> move to end
                    self.cursor_pos = self.text.len();
                    self.preferred_col = None;
                    return;
                }
            }
        }

        // Fallback to logical line navigation if we don't have wrapping info yet.
        let target_col = match self.preferred_col {
            Some(c) => c,
            None => {
                let c = self.current_display_col();
                self.preferred_col = Some(c);
                c
            }
        };
        if let Some(next_nl) = self.text[self.cursor_pos..]
            .find('\n')
            .map(|i| i + self.cursor_pos)
        {
            let next_line_start = next_nl + 1;
            let next_line_end = self.text[next_line_start..]
                .find('\n')
                .map(|i| i + next_line_start)
                .unwrap_or(self.text.len());
            self.move_to_display_col_on_line(next_line_start, next_line_end, target_col);
        } else {
            self.cursor_pos = self.text.len();
            self.preferred_col = None;
        }
    }

    pub fn move_cursor_to_beginning_of_line(&mut self, move_up_at_bol: bool) {
        let bol = self.beginning_of_current_line();
        if move_up_at_bol && self.cursor_pos == bol {
            self.set_cursor(self.beginning_of_line(self.cursor_pos.saturating_sub(1)));
        } else {
            self.set_cursor(bol);
        }
        self.preferred_col = None;
    }

    pub fn move_cursor_to_end_of_line(&mut self, move_down_at_eol: bool) {
        let eol = self.end_of_current_line();
        if move_down_at_eol && self.cursor_pos == eol {
            let next_pos = (self.cursor_pos.saturating_add(1)).min(self.text.len());
            self.set_cursor(self.end_of_line(next_pos));
        } else {
            self.set_cursor(eol);
        }
    }

    // ===== Text elements support =====

    pub fn element_payloads(&self) -> Vec<String> {
        self.elements
            .iter()
            .filter_map(|e| self.text.get(e.range.clone()).map(str::to_string))
            .collect()
    }

    pub fn text_elements(&self) -> Vec<UserTextElement> {
        self.elements
            .iter()
            .map(|e| {
                let placeholder = self.text.get(e.range.clone()).map(str::to_string);
                UserTextElement::new(
                    ByteRange {
                        start: e.range.start,
                        end: e.range.end,
                    },
                    placeholder,
                )
            })
            .collect()
    }

    pub(crate) fn text_element_snapshots(&self) -> Vec<TextElementSnapshot> {
        self.elements
            .iter()
            .filter_map(|element| {
                self.text
                    .get(element.range.clone())
                    .map(|text| TextElementSnapshot {
                        id: element.id,
                        range: element.range.clone(),
                        text: text.to_string(),
                    })
            })
            .collect()
    }

    /// Iterates borrowed atomic element ranges in ascending start order.
    pub(crate) fn text_element_ranges(&self) -> impl Iterator<Item = &Range<usize>> {
        self.elements.iter().map(|element| &element.range)
    }

    /// Iterates ordered atomic element ranges that overlap `range`.
    ///
    /// Elements ending exactly at the range start or starting exactly at its end are excluded.
    pub(crate) fn text_element_ranges_overlapping(
        &self,
        range: Range<usize>,
    ) -> impl Iterator<Item = &Range<usize>> {
        let first = self
            .elements
            .partition_point(|element| element.range.end <= range.start);
        self.elements[first..]
            .iter()
            .take_while(move |element| element.range.start < range.end)
            .map(|element| &element.range)
    }

    pub(crate) fn element_id_for_exact_range(&self, range: Range<usize>) -> Option<u64> {
        self.elements
            .iter()
            .find(|element| element.range == range)
            .map(|element| element.id)
    }

    /// Renames a single text element in-place, keeping it atomic.
    ///
    /// Use this when the element payload is an identifier (e.g. a placeholder) that must be
    /// updated without converting the element back into normal text.
    pub fn replace_element_payload(&mut self, old: &str, new: &str) -> bool {
        let Some(idx) = self
            .elements
            .iter()
            .position(|e| self.text.get(e.range.clone()) == Some(old))
        else {
            return false;
        };

        let range = self.elements[idx].range.clone();
        let start = range.start;
        let end = range.end;
        if start > end || end > self.text.len() {
            return false;
        }

        let removed_len = end - start;
        let inserted_len = new.len();
        let diff = inserted_len as isize - removed_len as isize;

        self.text.replace_range(range, new);
        self.wrap_cache.replace(None);
        self.preferred_col = None;

        // Update the modified element's range.
        self.elements[idx].range = start..(start + inserted_len);

        // Shift element ranges that occur after the replaced element.
        if diff != 0 {
            for (j, e) in self.elements.iter_mut().enumerate() {
                if j == idx {
                    continue;
                }
                if e.range.end <= start {
                    continue;
                }
                if e.range.start >= end {
                    e.range.start = ((e.range.start as isize) + diff) as usize;
                    e.range.end = ((e.range.end as isize) + diff) as usize;
                    continue;
                }

                // Elements should not partially overlap each other; degrade gracefully by
                // snapping anything intersecting the replaced range to the new bounds.
                e.range.start = start.min(e.range.start);
                e.range.end = (start + inserted_len).max(e.range.end.saturating_add_signed(diff));
            }
        }

        // Update the cursor position to account for the edit.
        self.cursor_pos = if self.cursor_pos < start {
            self.cursor_pos
        } else if self.cursor_pos <= end {
            start + inserted_len
        } else {
            ((self.cursor_pos as isize) + diff) as usize
        };
        self.cursor_pos = self.clamp_pos_to_nearest_boundary(self.cursor_pos);

        // Keep element ordering deterministic.
        self.elements.sort_by_key(|e| e.range.start);

        true
    }

    pub fn insert_element(&mut self, text: &str) -> u64 {
        let start = self.clamp_pos_for_insertion(self.cursor_pos);
        self.insert_str_at(start, text);
        let end = start + text.len();
        let id = self.add_element(start..end);
        // Place cursor at end of inserted element
        self.set_cursor(end);
        id
    }

    fn add_element(&mut self, range: Range<usize>) -> u64 {
        let id = self.next_element_id();
        self.elements.push(TextElement { id, range });
        self.elements.sort_by_key(|e| e.range.start);
        id
    }

    /// Mark an existing text range as an atomic element without changing the text.
    ///
    /// This is used to convert already-typed tokens (like `/plan`) into elements
    /// so they render and edit atomically. Overlapping or duplicate ranges are ignored.
    pub fn add_element_range(&mut self, range: Range<usize>) -> Option<u64> {
        let start = self.clamp_pos_to_char_boundary(range.start.min(self.text.len()));
        let end = self.clamp_pos_to_char_boundary(range.end.min(self.text.len()));
        if start >= end {
            return None;
        }
        if self
            .elements
            .iter()
            .any(|e| e.range.start == start && e.range.end == end)
        {
            return None;
        }
        if self
            .elements
            .iter()
            .any(|e| start < e.range.end && end > e.range.start)
        {
            return None;
        }
        let id = self.add_element(start..end);
        Some(id)
    }

    pub fn remove_element_range(&mut self, range: Range<usize>) -> bool {
        let start = self.clamp_pos_to_char_boundary(range.start.min(self.text.len()));
        let end = self.clamp_pos_to_char_boundary(range.end.min(self.text.len()));
        if start >= end {
            return false;
        }
        let len_before = self.elements.len();
        self.elements
            .retain(|elem| elem.range.start != start || elem.range.end != end);
        len_before != self.elements.len()
    }

    fn next_element_id(&mut self) -> u64 {
        let id = self.next_element_id;
        self.next_element_id = self.next_element_id.saturating_add(1);
        id
    }
    fn find_element_containing(&self, pos: usize) -> Option<usize> {
        self.elements
            .iter()
            .position(|e| pos > e.range.start && pos < e.range.end)
    }

    fn clamp_pos_to_char_boundary(&self, pos: usize) -> usize {
        let pos = pos.min(self.text.len());
        if self.text.is_char_boundary(pos) {
            return pos;
        }
        let mut prev = pos;
        while prev > 0 && !self.text.is_char_boundary(prev) {
            prev -= 1;
        }
        let mut next = pos;
        while next < self.text.len() && !self.text.is_char_boundary(next) {
            next += 1;
        }
        if pos.saturating_sub(prev) <= next.saturating_sub(pos) {
            prev
        } else {
            next
        }
    }

    fn clamp_pos_to_nearest_boundary(&self, pos: usize) -> usize {
        let pos = self.clamp_pos_to_char_boundary(pos);
        if let Some(idx) = self.find_element_containing(pos) {
            let e = &self.elements[idx];
            let dist_start = pos.saturating_sub(e.range.start);
            let dist_end = e.range.end.saturating_sub(pos);
            if dist_start <= dist_end {
                self.clamp_pos_to_char_boundary(e.range.start)
            } else {
                self.clamp_pos_to_char_boundary(e.range.end)
            }
        } else {
            pos
        }
    }

    fn clamp_pos_for_insertion(&self, pos: usize) -> usize {
        let pos = self.clamp_pos_to_char_boundary(pos);
        // Do not allow inserting into the middle of an element
        if let Some(idx) = self.find_element_containing(pos) {
            let e = &self.elements[idx];
            // Choose closest edge for insertion
            let dist_start = pos.saturating_sub(e.range.start);
            let dist_end = e.range.end.saturating_sub(pos);
            if dist_start <= dist_end {
                self.clamp_pos_to_char_boundary(e.range.start)
            } else {
                self.clamp_pos_to_char_boundary(e.range.end)
            }
        } else {
            pos
        }
    }

    fn expand_range_to_element_boundaries(&self, mut range: Range<usize>) -> Range<usize> {
        // Expand to include any intersecting elements fully
        loop {
            let mut changed = false;
            for e in &self.elements {
                if e.range.start < range.end && e.range.end > range.start {
                    let new_start = range.start.min(e.range.start);
                    let new_end = range.end.max(e.range.end);
                    if new_start != range.start || new_end != range.end {
                        range.start = new_start;
                        range.end = new_end;
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        range
    }

    fn shift_elements(&mut self, at: usize, removed: usize, inserted: usize) {
        // Generic shift: for pure insert, removed = 0; for delete, inserted = 0.
        let end = at + removed;
        let diff = inserted as isize - removed as isize;
        // Remove elements fully deleted by the operation and shift the rest
        self.elements
            .retain(|e| !(e.range.start >= at && e.range.end <= end));
        for e in &mut self.elements {
            if e.range.end <= at {
                // before edit
            } else if e.range.start >= end {
                // after edit
                e.range.start = ((e.range.start as isize) + diff) as usize;
                e.range.end = ((e.range.end as isize) + diff) as usize;
            } else {
                // Overlap with element but not fully contained (shouldn't happen when using
                // element-aware replace, but degrade gracefully by snapping element to new bounds)
                let new_start = at.min(e.range.start);
                let new_end = at + inserted.max(e.range.end.saturating_sub(end));
                e.range.start = new_start;
                e.range.end = new_end;
            }
        }
    }

    fn update_elements_after_replace(&mut self, start: usize, end: usize, inserted_len: usize) {
        self.shift_elements(start, end.saturating_sub(start), inserted_len);
    }

    fn prev_atomic_boundary(&self, pos: usize) -> usize {
        if pos == 0 {
            return 0;
        }
        // If currently at an element end or inside, jump to start of that element.
        if let Some(idx) = self
            .elements
            .iter()
            .position(|e| pos > e.range.start && pos <= e.range.end)
        {
            return self.elements[idx].range.start;
        }
        let mut gc = unicode_segmentation::GraphemeCursor::new(pos, self.text.len(), false);
        match gc.prev_boundary(&self.text, 0) {
            Ok(Some(b)) => {
                if let Some(idx) = self.find_element_containing(b) {
                    self.elements[idx].range.start
                } else {
                    b
                }
            }
            Ok(None) => 0,
            Err(_) => pos.saturating_sub(1),
        }
    }

    fn next_atomic_boundary(&self, pos: usize) -> usize {
        if pos >= self.text.len() {
            return self.text.len();
        }
        // If currently at an element start or inside, jump to end of that element.
        if let Some(idx) = self
            .elements
            .iter()
            .position(|e| pos >= e.range.start && pos < e.range.end)
        {
            return self.elements[idx].range.end;
        }
        let mut gc = unicode_segmentation::GraphemeCursor::new(pos, self.text.len(), false);
        match gc.next_boundary(&self.text, 0) {
            Ok(Some(b)) => {
                if let Some(idx) = self.find_element_containing(b) {
                    self.elements[idx].range.end
                } else {
                    b
                }
            }
            Ok(None) => self.text.len(),
            Err(_) => pos.saturating_add(1),
        }
    }

    pub(crate) fn beginning_of_previous_word(&self) -> usize {
        let prefix = &self.text[..self.cursor_pos];
        let Some((first_non_ws_idx, ch)) = prefix
            .char_indices()
            .rev()
            .find(|&(_, ch)| !ch.is_whitespace())
        else {
            return 0;
        };
        let run_start = prefix[..first_non_ws_idx]
            .char_indices()
            .rev()
            .find(|&(_, ch)| ch.is_whitespace())
            .map_or(0, |(idx, ch)| idx + ch.len_utf8());
        let run_end = first_non_ws_idx + ch.len_utf8();
        let pieces = split_word_pieces(&prefix[run_start..run_end]);
        let mut pieces = pieces.into_iter().rev().peekable();
        let Some((piece_start, piece)) = pieces.next() else {
            return run_start;
        };
        let mut start = run_start + piece_start;

        if piece.chars().all(is_word_separator) {
            while let Some((idx, piece)) = pieces.peek() {
                if !piece.chars().all(is_word_separator) {
                    break;
                }
                start = run_start + *idx;
                pieces.next();
            }
        }

        self.adjust_pos_out_of_elements(start, /*prefer_start*/ true)
    }

    pub(crate) fn end_of_next_word(&self) -> usize {
        self.end_of_next_word_from(self.cursor_pos)
    }

    fn end_of_next_word_from(&self, cursor_pos: usize) -> usize {
        let suffix = &self.text[cursor_pos..];
        let Some(first_non_ws) = suffix.find(|ch: char| !ch.is_whitespace()) else {
            return self.text.len();
        };
        let run = &suffix[first_non_ws..];
        let run = &run[..run.find(char::is_whitespace).unwrap_or(run.len())];
        let mut pieces = split_word_pieces(run).into_iter().peekable();
        let Some((start, piece)) = pieces.next() else {
            return cursor_pos + first_non_ws;
        };
        let word_start = cursor_pos + first_non_ws + start;
        let mut end = word_start + piece.len();
        if piece.chars().all(is_word_separator) {
            while let Some((idx, piece)) = pieces.peek() {
                if !piece.chars().all(is_word_separator) {
                    break;
                }
                end = cursor_pos + first_non_ws + *idx + piece.len();
                pieces.next();
            }
        }

        self.adjust_pos_out_of_elements(end, /*prefer_start*/ false)
    }

    fn vim_word_end_exclusive(&self) -> usize {
        let end = self.end_of_next_word();
        let target = if end > self.cursor_pos {
            self.prev_atomic_boundary(end)
        } else {
            end
        };
        if target == self.cursor_pos && end < self.text.len() {
            self.end_of_next_word_from(end)
        } else {
            end
        }
    }

    fn vim_word_end_cursor(&self) -> usize {
        let end = self.vim_word_end_exclusive();
        if end > self.cursor_pos {
            self.prev_atomic_boundary(end)
        } else {
            end
        }
    }

    fn vim_line_end_cursor(&self) -> usize {
        let bol = self.beginning_of_current_line();
        let eol = self.end_of_current_line();
        if eol > bol {
            self.prev_atomic_boundary(eol).max(bol)
        } else {
            eol
        }
    }

    pub(crate) fn beginning_of_next_word(&self) -> usize {
        let Some(first_non_ws) = self.text[self.cursor_pos..].find(|c: char| !c.is_whitespace())
        else {
            return self.text.len();
        };
        let word_start = self.cursor_pos + first_non_ws;
        if word_start != self.cursor_pos {
            return self.adjust_pos_out_of_elements(word_start, /*prefer_start*/ true);
        }
        let end = self.end_of_next_word();
        if end >= self.text.len() {
            return self.text.len();
        }
        let Some(next_non_ws) = self.text[end..].find(|c: char| !c.is_whitespace()) else {
            return self.text.len();
        };
        self.adjust_pos_out_of_elements(end + next_non_ws, /*prefer_start*/ true)
    }

    fn adjust_pos_out_of_elements(&self, pos: usize, prefer_start: bool) -> usize {
        if let Some(idx) = self.find_element_containing(pos) {
            let e = &self.elements[idx];
            if prefer_start {
                e.range.start
            } else {
                e.range.end
            }
        } else {
            pos
        }
    }

    /// Returns cached grapheme-safe visual ranges, including cursor-position sentinel bytes.
    ///
    /// Overflowing spaces wrap without separating a partial whitespace continuation from the next
    /// word, existing word breakpoints stay intact, and full logical lines receive a continuation
    /// row so their insertion point stays visible.
    #[expect(clippy::unwrap_used)]
    fn wrapped_lines(&self, width: u16) -> Ref<'_, Vec<Range<usize>>> {
        // Ensure cache is ready (potentially mutably borrow, then drop)
        {
            let mut cache = self.wrap_cache.borrow_mut();
            let needs_recalc = match cache.as_ref() {
                Some(c) => c.width != width,
                None => true,
            };
            if needs_recalc {
                let display_text = text_for_display(&self.text);
                let lines = wrapping::wrapped_lines(display_text.as_ref(), width);
                *cache = Some(WrapCache {
                    width,
                    lines,
                    hyperlinks: OnceCell::new(),
                });
            }
        }

        let cache = self.wrap_cache.borrow();
        Ref::map(cache, |c| &c.as_ref().unwrap().lines)
    }

    /// Calculate the scroll offset that should be used to satisfy the
    /// invariants given the current area size and wrapped lines.
    ///
    /// - Cursor is always on screen.
    /// - No scrolling if content fits in the area.
    fn effective_scroll(&self, area: Rect, lines: &[Range<usize>], current_scroll: u16) -> u16 {
        let total_lines = lines.len() as u16;
        if area.height >= total_lines {
            return 0;
        }

        let cursor_line_idx =
            wrapping::cursor_position(&self.text, lines, area.width, self.cursor_pos)
                .map_or(0, |(row, _)| row) as u16;

        let max_scroll = total_lines.saturating_sub(area.height);
        let mut scroll = current_scroll.min(max_scroll);

        // Ensure cursor is visible within [scroll, scroll + area_height)
        if cursor_line_idx < scroll {
            scroll = cursor_line_idx;
        } else if cursor_line_idx >= scroll + area.height {
            scroll = cursor_line_idx + 1 - area.height;
        }
        scroll
    }
}

impl WidgetRef for &TextArea {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        let lines = self.wrapped_lines(area.width);
        self.render_lines(
            area,
            buf,
            &lines,
            0..lines.len().min(usize::from(area.height)),
            Style::default(),
            &[],
        );
    }
}

impl StatefulWidgetRef for &TextArea {
    type State = TextAreaState;

    fn render_ref(&self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let lines = self.wrapped_lines(area.width);
        let scroll = self.effective_scroll(area, &lines, state.scroll);
        state.scroll = scroll;

        let start = scroll as usize;
        let end = (scroll + area.height).min(lines.len() as u16) as usize;
        self.render_lines(area, buf, &lines, start..end, Style::default(), &[]);
    }
}

impl TextArea {
    pub(crate) fn render_ref_masked(
        &self,
        area: Rect,
        buf: &mut Buffer,
        state: &mut TextAreaState,
        mask_char: char,
    ) {
        let lines = self.wrapped_lines(area.width);
        let scroll = self.effective_scroll(area, &lines, state.scroll);
        state.scroll = scroll;

        let start = scroll as usize;
        let end = (scroll + area.height).min(lines.len() as u16) as usize;
        self.render_lines_masked(area, buf, &lines, start..end, mask_char);
    }

    /// Render the textarea with `base_style` plus additional render-only highlight ranges.
    ///
    /// Highlight ranges are byte ranges in `self.text`. They affect only the buffer rendering and
    /// do not mutate the editable text, cursor, element metadata, or wrapping cache.
    pub(crate) fn render_ref_styled_with_highlights(
        &self,
        area: Rect,
        buf: &mut Buffer,
        state: &mut TextAreaState,
        base_style: Style,
        highlights: &[(Range<usize>, Style)],
    ) {
        let lines = self.wrapped_lines(area.width);
        let scroll = self.effective_scroll(area, &lines, state.scroll);
        state.scroll = scroll;

        let start = scroll as usize;
        let end = (scroll + area.height).min(lines.len() as u16) as usize;
        self.render_lines(area, buf, &lines, start..end, base_style, highlights);
    }

    /// Renders visible text and styled overlays without writing outside the textarea viewport.
    fn render_lines(
        &self,
        area: Rect,
        buf: &mut Buffer,
        lines: &[Range<usize>],
        range: std::ops::Range<usize>,
        base_style: Style,
        highlights: &[(Range<usize>, Style)],
    ) {
        let element_style = base_style.fg(Color::Cyan);
        for (row, idx) in range.clone().enumerate() {
            let r = &lines[idx];
            let y = area.y + row as u16;
            let visible = wrapping::visible_prefix(&self.text[r.start..r.end - 1], area.width);
            let line_range = r.start..r.start + visible.len();
            buf.set_style(Rect::new(area.x, y, area.width, 1), base_style);
            // Draw base line with the provided style.
            buf.set_stringn(
                area.x,
                y,
                text_for_display(visible),
                usize::from(area.width),
                base_style,
            );

            // Apply search highlights last so they remain visible over styled elements.
            let overlays = self
                .elements
                .iter()
                .map(|element| (&element.range, element_style))
                .chain(highlights.iter().map(|(range, style)| (range, *style)));
            for (overlay_range, style) in overlays {
                let overlap_start = overlay_range.start.max(line_range.start);
                let overlap_end = overlay_range.end.min(line_range.end);
                if overlap_start >= overlap_end {
                    continue;
                }
                let styled = &self.text[overlap_start..overlap_end];
                let x_off = display_width(&self.text[line_range.start..overlap_start]);
                if x_off >= usize::from(area.width) {
                    continue;
                }
                let x_off = x_off as u16;
                buf.set_stringn(
                    area.x + x_off,
                    y,
                    text_for_display(styled),
                    usize::from(area.width.saturating_sub(x_off)),
                    style,
                );
            }
        }
        if let Some(wrap_cache) = self.wrap_cache.borrow().as_ref() {
            wrap_cache
                .hyperlinks
                .get_or_init(|| hyperlinks::HyperlinkCache::new(&self.text, lines))
                .mark(buf, area, &self.text, lines, range);
        }
    }

    /// Renders width-preserving mask glyphs without writing outside the textarea viewport.
    fn render_lines_masked(
        &self,
        area: Rect,
        buf: &mut Buffer,
        lines: &[Range<usize>],
        range: std::ops::Range<usize>,
        mask_char: char,
    ) {
        for (row, idx) in range.enumerate() {
            let r = &lines[idx];
            let y = area.y + row as u16;
            let visible = wrapping::visible_prefix(&self.text[r.start..r.end - 1], area.width);
            let masked = visible
                .graphemes(/*is_extended*/ true)
                .flat_map(|grapheme| std::iter::repeat_n(mask_char, display_width(grapheme)))
                .collect::<String>();
            buf.set_stringn(
                area.x,
                y,
                &masked,
                usize::from(area.width),
                Style::default(),
            );
        }
    }
}


