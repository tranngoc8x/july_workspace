//! TypeSafe (JEV) as a `DecisionEngine`.
//!
//! The wire contract lives here and nowhere else: the application layer only
//! ever sees `DecisionEngine`, `AgentSelectionDecision` and `DecisionError`.

mod client;
mod engine;
mod mapper;

pub use client::JevClient;
pub use engine::JevDecisionEngine;

use crate::adapter::AdapterStore;

/// A value July was configured with: the process environment first, then the
/// `.env` file in July's home (`~/.july/.env`, or `$JULY_HOME/.env`).
///
/// An exported variable always wins, so a shell can override stored config
/// without anyone editing a file. Nothing is written back into the process
/// environment: mutating it is unsafe in this edition and racy under threads.
// ponytail: only the JEV variables need this today. Move it up a level when a
// second boundary wants the same file.
pub(super) fn configured(name: &str) -> Option<String> {
    if let Some(value) = trimmed(std::env::var(name).ok()) {
        return Some(value);
    }
    let path = AdapterStore::open_default().ok()?.home().join(".env");
    let text = std::fs::read_to_string(path).ok()?;
    trimmed(read_env_file(&text, name))
}

/// `KEY=VALUE`, one per line. Blank lines and `#` comments are skipped, an
/// `export ` prefix and surrounding quotes are tolerated, and the last
/// assignment wins - the same shape people already write by hand.
fn read_env_file(text: &str, name: &str) -> Option<String> {
    let mut found = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() == name {
            found = Some(unquote(value.trim()));
        }
    }
    found
}

fn unquote(value: &str) -> String {
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|inner| inner.strip_suffix(quote))
        {
            return inner.to_owned();
        }
    }
    value.to_owned()
}

fn trimmed(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::read_env_file;

    const FILE: &str = "# July configuration\n\
                        \n\
                        TYPESAFE_API_KEY=apikey_first\n\
                        export JULY_JEV_MODEL = \"jev-latest\"\n\
                        JULY_JEV_BASE_URL='https://staging.example'\n\
                        #TYPESAFE_API_KEY=commented_out\n\
                        TYPESAFE_API_KEY=apikey_second\n\
                        MALFORMED\n";

    #[test]
    fn an_env_file_yields_the_last_assignment_without_quotes_or_comments() {
        assert_eq!(
            read_env_file(FILE, "TYPESAFE_API_KEY").as_deref(),
            Some("apikey_second"),
            "a later line overrides an earlier one, and a comment is not a line"
        );
        assert_eq!(
            read_env_file(FILE, "JULY_JEV_MODEL").as_deref(),
            Some("jev-latest"),
            "an export prefix, spacing and double quotes are all tolerated"
        );
        assert_eq!(
            read_env_file(FILE, "JULY_JEV_BASE_URL").as_deref(),
            Some("https://staging.example")
        );
        assert_eq!(read_env_file(FILE, "MALFORMED"), None);
        assert_eq!(read_env_file(FILE, "ABSENT"), None);
        assert_eq!(read_env_file("", "TYPESAFE_API_KEY"), None);
    }
}
