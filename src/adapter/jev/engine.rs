use super::{JevClient, mapper};
use crate::application::{
    AgentSelectionDecision, AgentSelectionRequest, DecisionEngine, DecisionError,
};

const DEFAULT_MODEL: &str = "jev-latest";

/// `DecisionEngine` backed by TypeSafe's judgment API.
///
/// July keeps the policy: this only turns candidates into a ranked opinion.
#[derive(Clone, Debug)]
pub struct JevDecisionEngine {
    client: Option<JevClient>,
    model: String,
}

impl JevDecisionEngine {
    /// Reads `TYPESAFE_API_KEY`, `JULY_JEV_BASE_URL` and `JULY_JEV_MODEL`.
    /// An unconfigured provider stays constructible and fails per call, so
    /// routing degrades instead of the workspace failing to start.
    pub fn from_env() -> Self {
        Self {
            client: JevClient::from_env(),
            model: std::env::var("JULY_JEV_MODEL")
                .ok()
                .map(|model| model.trim().to_owned())
                .filter(|model| !model.is_empty())
                .unwrap_or_else(|| DEFAULT_MODEL.to_owned()),
        }
    }

    pub fn is_configured(&self) -> bool {
        self.client.is_some()
    }
}

impl DecisionEngine for JevDecisionEngine {
    async fn choose_agent(
        &mut self,
        request: AgentSelectionRequest,
    ) -> Result<AgentSelectionDecision, DecisionError> {
        let client = self
            .client
            .as_ref()
            .ok_or(DecisionError::ProviderNotConfigured)?;
        if request.candidates.is_empty() {
            return Err(DecisionError::InvalidResponse(
                "no candidates to judge".to_owned(),
            ));
        }
        let body = client
            .systemone(&mapper::request_body(&request, &self.model))
            .await?;
        mapper::decision(&body, &request.candidates)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::{AgentCandidate, AgentCapabilities};
    use crate::domain::AgentId;

    #[tokio::test]
    async fn an_unconfigured_provider_refuses_instead_of_guessing() {
        let mut engine = JevDecisionEngine {
            client: None,
            model: DEFAULT_MODEL.to_owned(),
        };
        assert!(!engine.is_configured());

        let error = engine
            .choose_agent(AgentSelectionRequest {
                task: "fix Redis timeout".into(),
                candidates: vec![AgentCandidate {
                    agent_id: AgentId::new(),
                    name: "infra".into(),
                    capabilities: AgentCapabilities::default(),
                    active_work_count: 0,
                }],
            })
            .await
            .unwrap_err();

        assert_eq!(error, DecisionError::ProviderNotConfigured);
    }
}
