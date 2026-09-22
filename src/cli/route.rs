//! `/route`: what auto-routing *would* do, without doing it.
//!
//! Stage 2 of the rollout. It exists to collect eval cases and to check agent
//! metadata against real tasks before any judgment is allowed to assign work.

use crate::application::{
    AgentCandidate, AgentSelectionPolicy, AgentSelectionRequest, DecisionEngine, DecisionError,
    RoutingDecision, shortcut,
};

/// A judgment plus what policy would make of it. Nothing here assigns.
#[derive(Clone, Debug, PartialEq)]
pub struct RoutePreview {
    pub verdict: RoutingDecision,
    /// Whom the engine picked, named - reported even when policy refuses it,
    /// because a rejected pick is exactly what this command exists to show.
    pub selected: Option<String>,
    pub confidence: f32,
    /// Agent name and score, best first.
    pub scores: Vec<(String, f32)>,
}

/// Preview routing for `task`. Deterministic answers never reach the engine.
pub async fn preview<E: DecisionEngine>(
    engine: &mut E,
    policy: &AgentSelectionPolicy,
    task: String,
    candidates: Vec<AgentCandidate>,
) -> Result<RoutePreview, DecisionError> {
    if let Some(verdict) = shortcut(&candidates) {
        return Ok(RoutePreview {
            verdict,
            selected: candidates.first().map(|only| only.name.clone()),
            confidence: candidates.first().map_or(0.0, |_| 1.0),
            scores: candidates
                .iter()
                .map(|only| (only.name.clone(), 1.0))
                .collect(),
        });
    }

    let decision = engine
        .choose_agent(AgentSelectionRequest {
            task,
            candidates: candidates.clone(),
        })
        .await?;
    let name_of = |agent_id| {
        candidates
            .iter()
            .find(|candidate| candidate.agent_id == agent_id)
            .map(|candidate| candidate.name.clone())
    };
    Ok(RoutePreview {
        verdict: policy.apply(&decision, &candidates),
        selected: decision.selected.and_then(name_of),
        confidence: decision.confidence,
        scores: decision
            .candidates
            .iter()
            .filter_map(|scored| Some((name_of(scored.agent_id)?, scored.score)))
            .collect(),
    })
}

/// One status line, then the ranking. `-` where there is nothing to report.
pub fn render(preview: &RoutePreview) -> String {
    let verdict = match preview.verdict {
        RoutingDecision::Explicit(_) => "explicit",
        RoutingDecision::AutoSelected { .. } => "auto",
        RoutingDecision::Suggested { .. } => "suggest",
        RoutingDecision::Unresolved => "unresolved",
    };
    let selected = preview.selected.as_deref().unwrap_or("-");
    let confidence = match preview.selected {
        Some(_) => format!("{:.2}", preview.confidence),
        None => "-".to_owned(),
    };
    let mut output = format!("route\t{verdict}\t{selected}\t{confidence}\n");
    let table = super::render_table(
        ["AGENT", "SCORE"],
        preview
            .scores
            .iter()
            .map(|(name, score)| [name.clone(), format!("{score:.2}")])
            .collect(),
    );
    if !table.is_empty() {
        output.push_str(&table);
        output.push('\n');
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::{
        AgentCapabilities, AgentSelectionDecision, CandidateScore, MockDecisionEngine, RoutingMode,
    };
    use crate::domain::AgentId;

    fn candidate(name: &str) -> AgentCandidate {
        AgentCandidate {
            agent_id: AgentId::new(),
            name: name.to_owned(),
            description: None,
            capabilities: AgentCapabilities::default(),
            active_work_count: 0,
        }
    }

    fn policy(mode: RoutingMode) -> AgentSelectionPolicy {
        AgentSelectionPolicy {
            mode,
            ..AgentSelectionPolicy::default()
        }
    }

    #[tokio::test]
    async fn an_empty_room_resolves_nothing_without_asking_an_engine() {
        let mut engine = MockDecisionEngine::failing(DecisionError::ProviderNotConfigured);

        let preview = preview(
            &mut engine,
            &policy(RoutingMode::Suggest),
            "task".into(),
            vec![],
        )
        .await
        .unwrap();

        assert_eq!(preview.verdict, RoutingDecision::Unresolved);
        assert_eq!(preview.selected, None);
        assert!(preview.scores.is_empty());
        assert_eq!(engine.last_request, None);
        assert_eq!(render(&preview), "route\tunresolved\t-\t-\n");
    }

    #[tokio::test]
    async fn a_lone_candidate_is_the_answer_without_asking_an_engine() {
        let infra = candidate("infra");
        let mut engine = MockDecisionEngine::failing(DecisionError::ProviderNotConfigured);

        let preview = preview(
            &mut engine,
            &policy(RoutingMode::Suggest),
            "fix Redis timeout".into(),
            vec![infra.clone()],
        )
        .await
        .unwrap();

        assert_eq!(
            preview.verdict,
            RoutingDecision::AutoSelected {
                agent: infra.agent_id,
                confidence: 1.0,
            }
        );
        assert_eq!(engine.last_request, None);
        assert_eq!(
            render(&preview),
            "route\tauto\tinfra\t1.00\nAGENT  SCORE\n-----  -----\ninfra  1.00\n"
        );
    }

    #[tokio::test]
    async fn a_judgment_is_reported_with_the_ranking_and_the_policy_verdict() {
        let infra = candidate("infra");
        let backend = candidate("backend");
        let mut engine = MockDecisionEngine::deciding(AgentSelectionDecision {
            selected: Some(infra.agent_id),
            confidence: 0.91,
            candidates: vec![
                CandidateScore {
                    agent_id: infra.agent_id,
                    score: 0.82,
                },
                CandidateScore {
                    agent_id: backend.agent_id,
                    score: 0.18,
                },
            ],
        });

        let preview = preview(
            &mut engine,
            &policy(RoutingMode::Suggest),
            "implement Redis caching".into(),
            vec![infra.clone(), backend],
        )
        .await
        .unwrap();

        assert_eq!(
            preview.verdict,
            RoutingDecision::Suggested {
                agent: infra.agent_id,
                confidence: 0.91,
            }
        );
        assert_eq!(preview.selected.as_deref(), Some("infra"));
        assert_eq!(
            render(&preview),
            "route\tsuggest\tinfra\t0.91\nAGENT    SCORE\n-------  -----\ninfra    0.82\nbackend  0.18\n"
        );
        assert_eq!(
            engine.last_request.map(|request| request.task).as_deref(),
            Some("implement Redis caching")
        );
    }

    #[tokio::test]
    async fn a_refused_pick_is_still_reported_so_the_ranking_can_be_inspected() {
        let infra = candidate("infra");
        let backend = candidate("backend");
        let mut engine = MockDecisionEngine::deciding(AgentSelectionDecision {
            selected: Some(infra.agent_id),
            confidence: 0.40,
            candidates: vec![CandidateScore {
                agent_id: infra.agent_id,
                score: 0.40,
            }],
        });

        let preview = preview(
            &mut engine,
            &policy(RoutingMode::Automatic),
            "write a poem".into(),
            vec![infra, backend],
        )
        .await
        .unwrap();

        assert_eq!(preview.verdict, RoutingDecision::Unresolved);
        assert_eq!(preview.selected.as_deref(), Some("infra"));
        assert!(render(&preview).starts_with("route\tunresolved\tinfra\t0.40\n"));
    }

    #[tokio::test]
    async fn an_engine_failure_surfaces_instead_of_a_guess() {
        let mut engine = MockDecisionEngine::failing(DecisionError::Timeout);

        let error = preview(
            &mut engine,
            &policy(RoutingMode::Suggest),
            "task".into(),
            vec![candidate("infra"), candidate("backend")],
        )
        .await
        .unwrap_err();

        assert_eq!(error, DecisionError::Timeout);
    }
}
