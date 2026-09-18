//! Leading `@agent` mentions: the deterministic front half of work-centric
//! input routing. No LLM, no fuzzy matching — a mention is a literal token.

/// The agent names a line targets, and the prompt that follows them.
#[derive(Debug, Eq, PartialEq)]
pub struct Mentions<'a> {
    /// Deduplicated, in the order they were typed.
    pub agents: Vec<&'a str>,
    pub prompt: &'a str,
}

/// A name character: what an agent name may contain after the `@`.
fn is_name_char(character: char) -> bool {
    character.is_alphanumeric() || matches!(character, '-' | '_' | '.')
}

/// Parse `@a @b rest`. Returns `None` unless the line opens with a mention,
/// so plain chat and slash commands are untouched.
pub fn parse(line: &str) -> Option<Mentions<'_>> {
    let mut rest = line.trim_start();
    let mut agents: Vec<&str> = Vec::new();
    while let Some(after) = rest.strip_prefix('@') {
        let end = after
            .find(|c: char| !is_name_char(c))
            .unwrap_or(after.len());
        let (name, tail) = after.split_at(end);
        if name.is_empty() {
            break;
        }
        if !agents.contains(&name) {
            agents.push(name);
        }
        rest = tail.trim_start();
    }
    (!agents.is_empty()).then_some(Mentions {
        agents,
        prompt: rest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_single_mention_splits_into_agent_and_prompt() {
        let parsed = parse("@agent_order fix callback retry").unwrap();
        assert_eq!(parsed.agents, ["agent_order"]);
        assert_eq!(parsed.prompt, "fix callback retry");
    }

    #[test]
    fn several_mentions_are_collected_and_deduplicated_in_order() {
        let parsed = parse("  @agent_order @pay @agent_order implement refund flow").unwrap();
        assert_eq!(parsed.agents, ["agent_order", "pay"]);
        assert_eq!(parsed.prompt, "implement refund flow");
    }

    #[test]
    fn a_bare_mention_carries_an_empty_prompt() {
        let parsed = parse("@agent_order").unwrap();
        assert_eq!(parsed.agents, ["agent_order"]);
        assert_eq!(parsed.prompt, "");
    }

    #[test]
    fn only_leading_mentions_count_and_the_rest_stays_verbatim() {
        assert_eq!(parse("ping @agent_order"), None);
        assert_eq!(parse("/dm agent_order"), None);
        assert_eq!(parse("@ agent_order"), None);
        assert!(parse("").is_none());
        let parsed = parse("@agent_order ask @pay about it").unwrap();
        assert_eq!(parsed.agents, ["agent_order"]);
        assert_eq!(parsed.prompt, "ask @pay about it");
    }

    #[test]
    fn punctuation_terminates_a_name_without_swallowing_it() {
        let parsed = parse("@agent_order, then retry").unwrap();
        assert_eq!(parsed.agents, ["agent_order"]);
        assert_eq!(parsed.prompt, ", then retry");
    }
}
