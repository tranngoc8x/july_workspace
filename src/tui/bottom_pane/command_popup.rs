use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::WidgetRef;

use super::popup_consts::MAX_POPUP_ROWS;
use super::scroll_state::ScrollState;
use super::selection_popup_common::ColumnWidthConfig;
use super::selection_popup_common::ColumnWidthMode;
use super::selection_popup_common::GenericDisplayRow;
use super::selection_popup_common::measure_rows_height_with_col_width_mode;
use super::selection_popup_common::render_rows_with_col_width_mode;
use super::slash_commands::SlashCommand;
use crate::tui::support::render::Insets;
use crate::tui::support::render::RectExt;

const COMMAND_COLUMN_WIDTH: ColumnWidthConfig = ColumnWidthConfig::new(
    ColumnWidthMode::AutoAllRows,
    /*name_column_width*/ None,
);

pub(crate) struct CommandPopup {
    command_filter: String,
    commands: Vec<SlashCommand>,
    state: ScrollState,
}

impl CommandPopup {
    /// Builds a popup over the commands the active context exposes.
    pub(crate) fn new(commands: Vec<SlashCommand>) -> Self {
        Self {
            command_filter: String::new(),
            commands,
            state: ScrollState::new(),
        }
    }

    /// Update the filter string based on the current composer text. The text
    /// passed in is expected to start with a leading '/'. Everything after the
    /// *first* '/' on the *first* line becomes the active filter that is used
    /// to narrow down the list of available commands.
    pub(crate) fn on_composer_text_change(&mut self, text: String) {
        let first_line = text.lines().next().unwrap_or("");
        let previous_filter = self.command_filter.clone();

        if let Some(stripped) = first_line.strip_prefix('/') {
            // Extract the *first* token (sequence of non-whitespace
            // characters) after the slash so that `/clear something` still
            // shows the help for `/clear`.
            let token = stripped.trim_start();
            let cmd_token = token.split_whitespace().next().unwrap_or("");

            // Update the filter keeping the original case (commands are all
            // lower-case for now but this may change in the future).
            self.command_filter = cmd_token.to_string();
        } else {
            // The composer no longer starts with '/'. Reset the filter so the
            // popup shows the *full* command list if it is still displayed
            // for some reason.
            self.command_filter.clear();
        }

        if self.command_filter != previous_filter {
            self.state.reset();
        }

        // Reset or clamp selected index based on new filtered list.
        let matches_len = self.filtered_items().len();
        self.state.clamp_selection(matches_len);
        self.state
            .ensure_visible(matches_len, MAX_POPUP_ROWS.min(matches_len));
    }

    /// Determine the preferred height of the popup for a given width.
    /// Accounts for wrapped descriptions so that long tooltips don't overflow.
    pub(crate) fn calculate_required_height(&self, width: u16) -> u16 {
        let rows = self.rows_from_matches(self.filtered());

        measure_rows_height_with_col_width_mode(
            &rows,
            &self.state,
            MAX_POPUP_ROWS,
            width,
            COMMAND_COLUMN_WIDTH,
        )
    }

    /// Compute exact/prefix matches over built-in commands and user prompts,
    /// paired with optional highlight indices. Preserves the original
    /// presentation order for built-ins and prompts.
    fn filtered(&self) -> Vec<(SlashCommand, Option<Vec<usize>>)> {
        let filter = self.command_filter.trim();
        let mut out: Vec<(SlashCommand, Option<Vec<usize>>)> = Vec::new();
        if filter.is_empty() {
            out.extend(self.commands.iter().map(|command| (command.clone(), None)));
            return out;
        }

        let filter_lower = filter.to_lowercase();
        let filter_chars = filter.chars().count();
        let mut exact: Vec<(SlashCommand, Option<Vec<usize>>)> = Vec::new();
        let mut prefix: Vec<(SlashCommand, Option<Vec<usize>>)> = Vec::new();
        let indices_for = |offset| Some((offset..offset + filter_chars).collect());

        let mut push_match =
            |item: SlashCommand, display: &str, name: Option<&str>, name_offset: usize| {
                let display_lower = display.to_lowercase();
                let name_lower = name.map(str::to_lowercase);
                let display_exact = display_lower == filter_lower;
                let name_exact = name_lower.as_deref() == Some(filter_lower.as_str());
                if display_exact || name_exact {
                    let offset = if display_exact { 0 } else { name_offset };
                    exact.push((item, indices_for(offset)));
                    return;
                }
                let display_prefix = display_lower.starts_with(&filter_lower);
                let name_prefix = name_lower
                    .as_ref()
                    .is_some_and(|name| name.starts_with(&filter_lower));
                if display_prefix || name_prefix {
                    let offset = if display_prefix { 0 } else { name_offset };
                    prefix.push((item, indices_for(offset)));
                }
            };

        for command in self.commands.iter() {
            let display = command.command();
            push_match(command.clone(), display, None, 0);
        }

        out.extend(exact);
        out.extend(prefix);
        out
    }

    fn filtered_items(&self) -> Vec<SlashCommand> {
        self.filtered().into_iter().map(|(c, _)| c).collect()
    }

    fn rows_from_matches(
        &self,
        matches: Vec<(SlashCommand, Option<Vec<usize>>)>,
    ) -> Vec<GenericDisplayRow> {
        matches
            .into_iter()
            .map(|(item, indices)| {
                let name = format!("/{}", item.command());
                let description = item.description().to_string();
                GenericDisplayRow {
                    name,
                    name_prefix_spans: Vec::new(),
                    match_indices: indices.map(|v| v.into_iter().map(|i| i + 1).collect()),
                    display_shortcut: None,
                    description: Some(description),
                    category_tag: None,
                    wrap_indent: None,
                    is_disabled: false,
                    disabled_reason: None,
                }
            })
            .collect()
    }

    /// Move the selection cursor one step up.
    pub(crate) fn move_up(&mut self) {
        let len = self.filtered_items().len();
        self.state.move_up_wrap(len);
        self.state.ensure_visible(len, MAX_POPUP_ROWS.min(len));
    }

    /// Move the selection cursor one step down.
    pub(crate) fn move_down(&mut self) {
        let matches_len = self.filtered_items().len();
        self.state.move_down_wrap(matches_len);
        self.state
            .ensure_visible(matches_len, MAX_POPUP_ROWS.min(matches_len));
    }

    /// Return currently selected command, if any.
    pub(crate) fn selected_item(&self) -> Option<SlashCommand> {
        let matches = self.filtered_items();
        self.state
            .selected_idx
            .and_then(|idx| matches.get(idx).cloned())
    }
}

impl WidgetRef for CommandPopup {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        let rows = self.rows_from_matches(self.filtered());
        render_rows_with_col_width_mode(
            area.inset(Insets::tlbr(
                /*top*/ 0, /*left*/ 2, /*bottom*/ 0, /*right*/ 0,
            )),
            buf,
            &rows,
            &self.state,
            MAX_POPUP_ROWS,
            "no matches",
            COMMAND_COLUMN_WIDTH,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{CommandPopup, SlashCommand};

    fn popup() -> CommandPopup {
        CommandPopup::new(vec![
            SlashCommand::new("init", "set up this project"),
            SlashCommand::new("inspect", "show workspace state"),
            SlashCommand::new("exit", "leave July"),
        ])
    }

    fn visible(popup: &CommandPopup) -> Vec<String> {
        popup
            .filtered_items()
            .iter()
            .map(|command| command.command().to_string())
            .collect()
    }

    #[test]
    fn an_empty_filter_shows_every_command_in_order() {
        let popup = popup();
        assert_eq!(visible(&popup), vec!["init", "inspect", "exit"]);
    }

    #[test]
    fn a_prefix_narrows_the_list() {
        let mut popup = popup();
        popup.on_composer_text_change("/in".to_string());
        assert_eq!(visible(&popup), vec!["init", "inspect"]);
    }

    #[test]
    fn an_exact_match_sorts_ahead_of_a_longer_prefix_match() {
        let mut popup = popup();
        popup.on_composer_text_change("/init".to_string());
        assert_eq!(visible(&popup), vec!["init"]);
    }

    #[test]
    fn arguments_after_the_command_do_not_narrow_the_filter() {
        let mut popup = popup();
        popup.on_composer_text_change("/init some args".to_string());
        assert_eq!(visible(&popup), vec!["init"]);
    }

    #[test]
    fn moving_down_selects_the_next_match() {
        let mut popup = popup();
        popup.on_composer_text_change("/in".to_string());
        popup.move_down();

        assert_eq!(
            popup.selected_item().as_ref().map(SlashCommand::command),
            Some("inspect")
        );
    }

    #[test]
    fn text_that_is_no_longer_a_command_clears_the_filter() {
        let mut popup = popup();
        popup.on_composer_text_change("/in".to_string());
        popup.on_composer_text_change("plain text".to_string());

        assert_eq!(visible(&popup), vec!["init", "inspect", "exit"]);
    }
}
