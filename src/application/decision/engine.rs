use super::AgentCandidate;
use crate::domain::AgentId;
use thiserror::Error;

/// Everything a judgment needs, and nothing more: no transcript, no logs,
/// no secrets. Only the task text and the candidates code could not rule out.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentSelectionRequest {
    pub task: String,
    pub candidates: Vec<AgentCandidate>,
}

/// How well one candidate fits the task, in `0.0..=1.0`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CandidateScore {
    pub agent_id: AgentId,
    pub score: f32,
}

/// Advice, not an assignment. `AgentSelectionPolicy` decides what it means.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentSelectionDecision {
    pub selected: Option<AgentId>,
    pub confidence: f32,
    /// Ranked best first, for `/route` and for audit.
    pub candidates: Vec<CandidateScore>,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum DecisionError {
    #[error("decision provider is not configured")]
    ProviderNotConfigured,
    #[error("decision provider timed out")]
    Timeout,
    #[error("decision provider is unavailable: {0}")]
    Unavailable(String),
    #[error("decision provider returned an invalid response: {0}")]
    InvalidResponse(String),
}

/// The only thing the application layer knows about judgment. JEV, rules and
/// LLMs all arrive as implementations; none of them reach past this trait.
#[allow(async_fn_in_trait)]
pub trait DecisionEngine {
    async fn choose_agent(
        &mut self,
        request: AgentSelectionRequest,
    ) -> Result<AgentSelectionDecision, DecisionError>;
}

/// A canned engine: routing policy and callers can be tested without a
/// provider, and `/route` stays exercisable before the adapter exists.
#[derive(Clone, Debug)]
pub struct MockDecisionEngine {
    outcome: Result<AgentSelectionDecision, DecisionError>,
    /// The last request it was asked to judge, so callers can assert on it.
    pub last_request: Option<AgentSelectionRequest>,
}

impl MockDecisionEngine {
    pub fn selecting(agent_id: AgentId, confidence: f32) -> Self {
        Self::deciding(AgentSelectionDecision {
            selected: Some(agent_id),
            confidence,
            candidates: vec![CandidateScore {
                agent_id,
                score: confidence,
            }],
        })
    }

    pub fn deciding(decision: AgentSelectionDecision) -> Self {
        Self {
            outcome: Ok(decision),
            last_request: None,
        }
    }

    pub fn failing(error: DecisionError) -> Self {
        Self {
            outcome: Err(error),
            last_request: None,
        }
    }
}

impl DecisionEngine for MockDecisionEngine {
    async fn choose_agent(
        &mut self,
        request: AgentSelectionRequest,
    ) -> Result<AgentSelectionDecision, DecisionError> {
        self.last_request = Some(request);
        self.outcome.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::AgentCapabilities;

    fn candidate(name: &str) -> AgentCandidate {
        AgentCandidate {
            agent_id: AgentId::new(),
            name: name.to_owned(),
            description: None,
            capabilities: AgentCapabilities::default(),
            active_work_count: 0,
        }
    }

    #[tokio::test]
    async fn the_mock_engine_answers_with_its_canned_decision_and_records_the_request() {
        let backend = candidate("backend");
        let mut engine = MockDecisionEngine::selecting(backend.agent_id, 0.91);
        let request = AgentSelectionRequest {
            task: "fix Redis timeout".into(),
            candidates: vec![backend.clone()],
        };

        let decision = engine.choose_agent(request.clone()).await.unwrap();

        assert_eq!(decision.selected, Some(backend.agent_id));
        assert_eq!(decision.confidence, 0.91);
        assert_eq!(decision.candidates[0].agent_id, backend.agent_id);
        assert_eq!(engine.last_request, Some(request));
    }

    #[tokio::test]
    async fn an_unconfigured_provider_surfaces_as_an_error_not_a_guess() {
        let mut engine = MockDecisionEngine::failing(DecisionError::ProviderNotConfigured);

        let error = engine
            .choose_agent(AgentSelectionRequest {
                task: "fix Redis timeout".into(),
                candidates: vec![candidate("backend")],
            })
            .await
            .unwrap_err();

        assert_eq!(error, DecisionError::ProviderNotConfigured);
    }
}
