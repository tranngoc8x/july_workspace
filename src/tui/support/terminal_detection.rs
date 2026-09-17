//! Which terminal emulator July is running inside, to the extent rendering cares.
//!
//! Codex ships a full `codex-terminal-detection` crate that fingerprints a dozen emulators for
//! feature probing. The composer only ever asks one question - "is this Windows Terminal?" - because
//! that emulator under-reports its color support, so this module answers that and nothing else.
//!
//! ponytail: one emulator, detected from env. Port the full crate only if a feature needs to branch
//! on another terminal.

/// Terminal emulators rendering has to treat specially.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TerminalName {
    /// Windows Terminal, which advertises 16 colors while supporting truecolor.
    WindowsTerminal,
    /// Anything else, including a terminal that does not identify itself.
    Unknown,
}

/// What the current process could learn about its terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TerminalInfo {
    pub(crate) name: TerminalName,
}

/// Identifies the terminal from the environment the process was started with.
pub(crate) fn terminal_info() -> TerminalInfo {
    TerminalInfo {
        name: detect(|key| std::env::var(key).ok()),
    }
}

fn detect(var: impl Fn(&str) -> Option<String>) -> TerminalName {
    // Windows Terminal sets WT_SESSION in every pane it spawns; WT_PROFILE_ID covers older builds.
    let present = |key: &str| var(key).is_some_and(|value| !value.is_empty());
    if present("WT_SESSION") || present("WT_PROFILE_ID") {
        TerminalName::WindowsTerminal
    } else {
        TerminalName::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::{TerminalName, detect};

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |key| {
            pairs
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| (*value).to_string())
        }
    }

    #[test]
    fn windows_terminal_is_recognized_from_either_marker() {
        assert_eq!(
            detect(env(&[("WT_SESSION", "abc")])),
            TerminalName::WindowsTerminal
        );
        assert_eq!(
            detect(env(&[("WT_PROFILE_ID", "{guid}")])),
            TerminalName::WindowsTerminal
        );
    }

    #[test]
    fn empty_or_missing_markers_stay_unknown() {
        assert_eq!(detect(env(&[])), TerminalName::Unknown);
        assert_eq!(detect(env(&[("WT_SESSION", "")])), TerminalName::Unknown);
        assert_eq!(
            detect(env(&[("TERM_PROGRAM", "iTerm.app")])),
            TerminalName::Unknown
        );
    }
}
