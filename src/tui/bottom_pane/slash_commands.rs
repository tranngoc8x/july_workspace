//! The slash commands the composer can complete, and how a typed name is matched against them.
//!
//! Codex compiles its commands into an enum and gates each variant behind a product feature flag.
//! July's commands come from the active [`Context`](crate::tui::app::Context) instead, so this is a
//! plain list the app hands in and the composer filters. What is left of Codex's module is the
//! matching: exact name first, then a fuzzy prefix for the popup.

use crate::tui::support::fuzzy_match::fuzzy_match;

/// One command the composer can offer and submit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SlashCommand {
    name: String,
    description: String,
}

impl SlashCommand {
    /// Builds a command from its name (without the leading `/`) and a one-line description.
    pub(crate) fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
        }
    }

    /// The command name, without the leading `/`.
    pub(crate) fn command(&self) -> &str {
        &self.name
    }

    /// One line describing the command, shown beside it in the popup. May be empty.
    pub(crate) fn description(&self) -> &str {
        &self.description
    }

    /// Whether `/name rest of line` should submit `rest of line` as the command's argument.
    ///
    /// ponytail: always true. July dispatches the whole line to the command registry, which parses
    /// its own arguments. Gate this per command if some command must reject trailing text in the
    /// composer rather than at dispatch.
    pub(crate) fn supports_inline_args(&self) -> bool {
        true
    }

    /// Whether the command can be submitted while a turn is running.
    ///
    /// ponytail: always true. July's reducer already refuses a second submission while a turn is
    /// active (`App::submit`), so the composer does not need a second gate. Carry a per-command
    /// flag here if some command must stay available and others must not.
    pub(crate) fn available_during_task(&self) -> bool {
        true
    }
}

/// Builds the command list the composer offers, from the names the active context exposes.
///
/// ponytail: descriptions come through empty. `Context::commands` carries names only; the registry
/// that knows the descriptions lives behind the CLI bridge. Plumb them through `Context` to fill the
/// right-hand column of the popup.
pub(crate) fn commands_from_names<I, S>(names: I) -> Vec<SlashCommand>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    names
        .into_iter()
        .map(|name| SlashCommand::new(name.as_ref().trim_start_matches('/'), ""))
        .collect()
}

/// Finds the command a typed name refers to exactly.
pub(crate) fn find_slash_command(name: &str, commands: &[SlashCommand]) -> Option<SlashCommand> {
    commands
        .iter()
        .find(|command| command.command() == name)
        .cloned()
}

/// Whether any command could still be completed from what has been typed so far.
pub(crate) fn has_slash_command_prefix(name: &str, commands: &[SlashCommand]) -> bool {
    commands
        .iter()
        .any(|command| fuzzy_match(command.command(), name).is_some())
}

#[cfg(test)]
mod tests {
    use super::{SlashCommand, commands_from_names, find_slash_command, has_slash_command_prefix};

    fn commands() -> Vec<SlashCommand> {
        commands_from_names(["dm", "debug", "status"])
    }

    #[test]
    fn names_lose_a_leading_slash() {
        assert_eq!(
            commands_from_names(["/dm", "status"])
                .iter()
                .map(SlashCommand::command)
                .collect::<Vec<_>>(),
            vec!["dm", "status"]
        );
    }

    #[test]
    fn lookup_is_exact_not_prefix() {
        assert_eq!(
            find_slash_command("dm", &commands())
                .as_ref()
                .map(SlashCommand::command),
            Some("dm")
        );
        assert_eq!(find_slash_command("d", &commands()), None);
        assert_eq!(find_slash_command("nope", &commands()), None);
    }

    #[test]
    fn a_prefix_keeps_the_popup_open_even_when_no_command_matches_exactly() {
        assert!(has_slash_command_prefix("d", &commands()));
        assert!(has_slash_command_prefix("stat", &commands()));
        assert!(!has_slash_command_prefix("zzz", &commands()));
    }
}
