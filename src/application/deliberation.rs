use crate::domain::{
    AgentId, Decision, DecisionId, DecisionOutcome, DecisionWork, Handoff, HandoffChallenge,
    HandoffId, HandoffResponse, Proposal, ProposalId, ProposalResponse, WorkItem,
};
use thiserror::Error;

/// Deliberation failures are grouped by what a caller can do about them
/// rather than mirroring every storage invariant: the protocol has one shape
/// of answer per group, and CLI error codes map straight onto it.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum DeliberationError {
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    NotPermitted(String),
    #[error("{0}")]
    InvalidTransition(String),
    #[error("{0}")]
    Invalid(String),
    #[error("deliberation runtime failed: {0}")]
    Runtime(String),
}

#[allow(async_fn_in_trait)]
pub trait DeliberationRuntime {
    async fn propose_handoff(&mut self, handoff: Handoff) -> Result<Handoff, DeliberationError>;

    async fn respond_to_handoff(
        &mut self,
        handoff_id: HandoffId,
        response: HandoffResponse,
        responded_at: String,
    ) -> Result<Handoff, DeliberationError>;

    async fn challenge_handoff(
        &mut self,
        handoff_id: HandoffId,
        challenge: HandoffChallenge,
        challenged_at: String,
    ) -> Result<(Handoff, Option<Decision>), DeliberationError>;

    async fn resolve_handoff(
        &mut self,
        handoff_id: HandoffId,
        agent_id: AgentId,
        resolved_at: String,
    ) -> Result<Handoff, DeliberationError>;

    async fn create_proposal(&mut self, proposal: Proposal) -> Result<Proposal, DeliberationError>;

    async fn respond_to_proposal(
        &mut self,
        response: ProposalResponse,
    ) -> Result<ProposalResponse, DeliberationError>;

    async fn withdraw_proposal(
        &mut self,
        proposal_id: ProposalId,
        agent_id: AgentId,
        withdrawn_at: String,
    ) -> Result<Proposal, DeliberationError>;

    async fn record_decision(&mut self, decision: Decision) -> Result<Decision, DeliberationError>;

    async fn decide(
        &mut self,
        decision_id: DecisionId,
        outcome: DecisionOutcome,
        decided_at: String,
    ) -> Result<Decision, DeliberationError>;

    async fn convert_decision_to_work(
        &mut self,
        decision_id: DecisionId,
        items: Vec<DecisionWork>,
        created_at: String,
    ) -> Result<Vec<WorkItem>, DeliberationError>;
}

/// The deterministic half of a deliberation: July tracks handoff state,
/// dispute rounds, proposals and decisions; no LLM is involved.
pub struct DeliberationService<R> {
    runtime: R,
}

impl<R: DeliberationRuntime> DeliberationService<R> {
    pub fn new(runtime: R) -> Self {
        Self { runtime }
    }

    pub fn into_runtime(self) -> R {
        self.runtime
    }

    pub async fn propose_handoff(
        &mut self,
        handoff: Handoff,
    ) -> Result<Handoff, DeliberationError> {
        self.runtime.propose_handoff(handoff).await
    }

    pub async fn respond_to_handoff(
        &mut self,
        handoff_id: HandoffId,
        response: HandoffResponse,
        responded_at: String,
    ) -> Result<Handoff, DeliberationError> {
        self.runtime
            .respond_to_handoff(handoff_id, response, responded_at)
            .await
    }

    pub async fn challenge_handoff(
        &mut self,
        handoff_id: HandoffId,
        challenge: HandoffChallenge,
        challenged_at: String,
    ) -> Result<(Handoff, Option<Decision>), DeliberationError> {
        self.runtime
            .challenge_handoff(handoff_id, challenge, challenged_at)
            .await
    }

    pub async fn resolve_handoff(
        &mut self,
        handoff_id: HandoffId,
        agent_id: AgentId,
        resolved_at: String,
    ) -> Result<Handoff, DeliberationError> {
        self.runtime
            .resolve_handoff(handoff_id, agent_id, resolved_at)
            .await
    }

    pub async fn create_proposal(
        &mut self,
        proposal: Proposal,
    ) -> Result<Proposal, DeliberationError> {
        self.runtime.create_proposal(proposal).await
    }

    pub async fn respond_to_proposal(
        &mut self,
        response: ProposalResponse,
    ) -> Result<ProposalResponse, DeliberationError> {
        self.runtime.respond_to_proposal(response).await
    }

    pub async fn withdraw_proposal(
        &mut self,
        proposal_id: ProposalId,
        agent_id: AgentId,
        withdrawn_at: String,
    ) -> Result<Proposal, DeliberationError> {
        self.runtime
            .withdraw_proposal(proposal_id, agent_id, withdrawn_at)
            .await
    }

    pub async fn record_decision(
        &mut self,
        decision: Decision,
    ) -> Result<Decision, DeliberationError> {
        self.runtime.record_decision(decision).await
    }

    pub async fn decide(
        &mut self,
        decision_id: DecisionId,
        outcome: DecisionOutcome,
        decided_at: String,
    ) -> Result<Decision, DeliberationError> {
        self.runtime.decide(decision_id, outcome, decided_at).await
    }

    pub async fn convert_decision_to_work(
        &mut self,
        decision_id: DecisionId,
        items: Vec<DecisionWork>,
        created_at: String,
    ) -> Result<Vec<WorkItem>, DeliberationError> {
        self.runtime
            .convert_decision_to_work(decision_id, items, created_at)
            .await
    }
}
