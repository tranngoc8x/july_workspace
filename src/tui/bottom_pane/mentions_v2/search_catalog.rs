//! What the `@` popup can offer besides files.
//!
//! Codex builds this from skills, plugins, and threads on other hosts. July's non-file mentions are
//! its project agents, so the catalog is a list of agent names turned into candidates. Files are not
//! here: they arrive asynchronously from [`file_search`](crate::tui::file_search) and are merged in
//! by [`filter`](super::filter).

use super::candidate::Candidate;
use super::candidate::MentionType;
use super::candidate::Selection;

/// Builds the static half of the `@` catalog from the agents in this workspace.
pub(crate) fn build_search_catalog(agents: &[String]) -> Vec<Candidate> {
    agents.iter().map(|agent| agent_candidate(agent)).collect()
}

fn agent_candidate(agent: &str) -> Candidate {
    Candidate {
        display_name: agent.to_string(),
        description: None,
        search_terms: vec![agent.to_string()],
        mention_type: MentionType::Agent,
        selection: Selection::Tool {
            insert_text: format!("@{agent}"),
            path: Some(format!("agent://{agent}")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::super::candidate::{MentionType, Selection};
    use super::build_search_catalog;

    #[test]
    fn every_agent_becomes_one_mention_candidate() {
        let catalog = build_search_catalog(&["agent_order".to_string(), "cashflow".to_string()]);

        assert_eq!(catalog.len(), 2);
        assert_eq!(catalog[0].display_name, "agent_order");
        assert_eq!(catalog[0].mention_type, MentionType::Agent);
        assert_eq!(
            catalog[0].selection,
            Selection::Tool {
                insert_text: "@agent_order".to_string(),
                path: Some("agent://agent_order".to_string()),
            }
        );
    }

    #[test]
    fn no_agents_means_an_empty_catalog() {
        assert!(build_search_catalog(&[]).is_empty());
    }
}
