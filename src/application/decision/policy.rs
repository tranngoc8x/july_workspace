use super::{AgentCandidate, AgentSelectionDecision};
use crate::domain::AgentId;

/// How much of a judgment July is willing to act on.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RoutingMode {
    /// Pre-JEV behaviour: nothing is routed automatically.
    Disabled,
    /// Judgments are shown; a human still picks. The safe default.
    #[default]
    Suggest,
    /// A confident judgment assigns on its own.
    Automatic,
}

impl RoutingMode {
    fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "disabled" => Some(Self::Disabled),
            "suggest" => Some(Self::Suggest),
            "automatic" => Some(Self::Automatic),
            _ => None,
        }
    }
}

/// What July does with a judgment. Thresholds are a starting point to be
/// calibrated against the eval set, never a claim about accuracy.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AgentSelectionPolicy {
    pub mode: RoutingMode,
    pub auto_assign_threshold: f32,
    pub suggest_threshold: f32,
}

impl Default for AgentSelectionPolicy {
    fn default() -> Self {
        Self {
            mode: RoutingMode::default(),
            auto_assign_threshold: 0.85,
            suggest_threshold: 0.65,
        }
    }
}

impl AgentSelectionPolicy {
    /// Read the operator's overrides. An unset or unparseable variable keeps
    /// the default, so a typo can never widen what July assigns on its own.
    // ponytail: env vars, not a config file - the workspace has no config
    // loader yet, and one knob does not justify inventing one.
    pub fn from_env() -> Self {
        let default = Self::default();
        Self {
            mode: env_value("JULY_ROUTING_MODE")
                .and_then(|value| RoutingMode::parse(&value))
                .unwrap_or(default.mode),
            auto_assign_threshold: env_threshold("JULY_ROUTING_AUTO_THRESHOLD")
                .unwrap_or(default.auto_assign_threshold),
            suggest_threshold: env_threshold("JULY_ROUTING_SUGGEST_THRESHOLD")
                .unwrap_or(default.suggest_threshold),
        }
    }

    /// Turn advice into an action. A selection naming an agent that was never
    /// a candidate is treated as no selection at all.
    pub fn apply(
        &self,
        decision: &AgentSelectionDecision,
        candidates: &[AgentCandidate],
    ) -> RoutingDecision {
        if self.mode == RoutingMode::Disabled {
            return RoutingDecision::Unresolved;
        }
        let Some(agent) = decision.selected.filter(|agent| {
            candidates
                .iter()
                .any(|candidate| candidate.agent_id == *agent)
        }) else {
            return RoutingDecision::Unresolved;
        };
        let confidence = decision.confidence;
        if self.mode == RoutingMode::Automatic && confidence >= self.auto_assign_threshold {
            RoutingDecision::AutoSelected { agent, confidence }
        } else if confidence >= self.suggest_threshold {
            RoutingDecision::Suggested { agent, confidence }
        } else {
            RoutingDecision::Unresolved
        }
    }
}

fn env_value(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn env_threshold(name: &str) -> Option<f32> {
    env_value(name)?
        .trim()
        .parse::<f32>()
        .ok()
        .filter(|value| (0.0..=1.0).contains(value))
}

/// Where a message ended up.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RoutingDecision {
    /// The caller named the agent; no judgment ran.
    Explicit(AgentId),
    AutoSelected {
        agent: AgentId,
        confidence: f32,
    },
    Suggested {
        agent: AgentId,
        confidence: f32,
    },
    /// Ask the caller. Never guess, and never pick at random.
    Unresolved,
}

/// Deterministic answers that make a judgment pointless. `None` means the
/// candidates genuinely need judging.
pub fn shortcut(candidates: &[AgentCandidate]) -> Option<RoutingDecision> {
    match candidates {
        [] => Some(RoutingDecision::Unresolved),
        [only] => Some(RoutingDecision::AutoSelected {
            agent: only.agent_id,
            confidence: 1.0,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::{AgentCapabilities, CandidateScore};

    fn candidate(name: &str) -> AgentCandidate {
        AgentCandidate {
            agent_id: AgentId::new(),
            name: name.to_owned(),
            capabilities: AgentCapabilities::default(),
            active_work_count: 0,
        }
    }

    fn decision(agent: Option<AgentId>, confidence: f32) -> AgentSelectionDecision {
        AgentSelectionDecision {
            selected: agent,
            confidence,
            candidates: agent
                .map(|agent_id| {
                    vec![CandidateScore {
                        agent_id,
                        score: confidence,
                    }]
                })
                .unwrap_or_default(),
        }
    }

    fn policy(mode: RoutingMode) -> AgentSelectionPolicy {
        AgentSelectionPolicy {
            mode,
            ..AgentSelectionPolicy::default()
        }
    }

    #[test]
    fn confident_judgments_only_assign_in_automatic_mode() {
        let backend = candidate("backend");
        let candidates = [backend.clone()];
        let confident = decision(Some(backend.agent_id), 0.91);

        assert_eq!(
            policy(RoutingMode::Automatic).apply(&confident, &candidates),
            RoutingDecision::AutoSelected {
                agent: backend.agent_id,
                confidence: 0.91,
            }
        );
        assert_eq!(
            policy(RoutingMode::Suggest).apply(&confident, &candidates),
            RoutingDecision::Suggested {
                agent: backend.agent_id,
                confidence: 0.91,
            }
        );
        assert_eq!(
            policy(RoutingMode::Disabled).apply(&confident, &candidates),
            RoutingDecision::Unresolved
        );
    }

    #[test]
    fn a_middling_judgment_suggests_and_a_weak_one_resolves_nothing() {
        let backend = candidate("backend");
        let candidates = [backend.clone()];
        let automatic = policy(RoutingMode::Automatic);

        assert_eq!(
            automatic.apply(&decision(Some(backend.agent_id), 0.7), &candidates),
            RoutingDecision::Suggested {
                agent: backend.agent_id,
                confidence: 0.7,
            }
        );
        assert_eq!(
            automatic.apply(&decision(Some(backend.agent_id), 0.4), &candidates),
            RoutingDecision::Unresolved
        );
    }

    #[test]
    fn a_selection_outside_the_candidate_list_is_refused() {
        let candidates = [candidate("backend")];
        let automatic = policy(RoutingMode::Automatic);

        assert_eq!(
            automatic.apply(&decision(Some(AgentId::new()), 0.99), &candidates),
            RoutingDecision::Unresolved
        );
        assert_eq!(
            automatic.apply(&decision(None, 0.99), &candidates),
            RoutingDecision::Unresolved
        );
    }

    #[test]
    fn obvious_candidate_lists_never_reach_an_engine() {
        let backend = candidate("backend");

        assert_eq!(shortcut(&[]), Some(RoutingDecision::Unresolved));
        assert_eq!(
            shortcut(std::slice::from_ref(&backend)),
            Some(RoutingDecision::AutoSelected {
                agent: backend.agent_id,
                confidence: 1.0,
            })
        );
        assert_eq!(shortcut(&[backend, candidate("infra")]), None);
    }

    #[test]
    fn routing_modes_parse_from_their_configured_names_only() {
        assert_eq!(RoutingMode::parse("disabled"), Some(RoutingMode::Disabled));
        assert_eq!(RoutingMode::parse(" suggest "), Some(RoutingMode::Suggest));
        assert_eq!(
            RoutingMode::parse("automatic"),
            Some(RoutingMode::Automatic)
        );
        assert_eq!(RoutingMode::parse("auto"), None);
        assert_eq!(RoutingMode::parse(""), None);
    }
}
