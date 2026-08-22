use crate::domain::{AgentId, ResultId, WorkItem, WorkItemId, WorkResult, WorkStatus};
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssignWorkOwner {
    pub work_id: WorkItemId,
    pub owner_agent_id: AgentId,
    pub assigned_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransitionWork {
    pub work_id: WorkItemId,
    pub target: WorkStatus,
    pub transitioned_at: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CreateWorkResult {
    pub result: WorkResult,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum WorkError {
    #[error("work {0} does not exist")]
    WorkNotFound(WorkItemId),
    #[error("owner agent {0} does not exist")]
    OwnerNotFound(AgentId),
    #[error("owner agent {0} is not active")]
    OwnerInactive(AgentId),
    #[error("agent {owner_agent_id} is not an active member of work {work_id}'s conversation")]
    OwnerOutOfScope {
        work_id: WorkItemId,
        owner_agent_id: AgentId,
    },
    #[error("terminal work {0} cannot change owner")]
    TerminalOwnershipImmutable(WorkItemId),
    #[error("work {work_id} cannot transition from {from} to {to}")]
    InvalidTransition {
        work_id: WorkItemId,
        from: WorkStatus,
        to: WorkStatus,
    },
    #[error("work mutation timestamp must not be blank")]
    InvalidTimestamp,
    #[error("result {0} already exists with different content")]
    ResultConflict(ResultId),
    #[error("superseded result {0} does not exist")]
    SupersededResultNotFound(ResultId),
    #[error("result {result_id} cannot supersede result {supersedes_result_id} from another work")]
    CrossWorkSupersede {
        result_id: ResultId,
        supersedes_result_id: ResultId,
    },
    #[error("work runtime failed: {0}")]
    Runtime(String),
}

#[allow(async_fn_in_trait)]
pub trait WorkRuntime {
    async fn assign_work_owner(
        &mut self,
        work_id: WorkItemId,
        owner_agent_id: AgentId,
        assigned_at: String,
    ) -> Result<WorkItem, WorkError>;

    async fn transition_work(
        &mut self,
        work_id: WorkItemId,
        target: WorkStatus,
        transitioned_at: String,
    ) -> Result<WorkItem, WorkError>;

    async fn create_work_result(&mut self, result: WorkResult) -> Result<WorkResult, WorkError>;
}

pub struct WorkService<R> {
    runtime: R,
}

impl<R: WorkRuntime> WorkService<R> {
    pub fn new(runtime: R) -> Self {
        Self { runtime }
    }

    pub async fn assign_owner(&mut self, command: AssignWorkOwner) -> Result<WorkItem, WorkError> {
        self.runtime
            .assign_work_owner(command.work_id, command.owner_agent_id, command.assigned_at)
            .await
    }

    pub async fn transition(&mut self, command: TransitionWork) -> Result<WorkItem, WorkError> {
        self.runtime
            .transition_work(command.work_id, command.target, command.transitioned_at)
            .await
    }

    pub async fn create_result(
        &mut self,
        command: CreateWorkResult,
    ) -> Result<WorkResult, WorkError> {
        self.runtime.create_work_result(command.result).await
    }
}
