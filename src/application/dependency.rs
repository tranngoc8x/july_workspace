use crate::domain::{WorkDependency, WorkItemId};
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AddWorkDependency {
    pub upstream_work_id: WorkItemId,
    pub downstream_work_id: WorkItemId,
    pub created_at: String,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum DependencyError {
    #[error("work {0} does not exist")]
    WorkNotFound(WorkItemId),
    #[error("work dependency cannot reference itself: {0}")]
    SelfDependency(WorkItemId),
    #[error("work dependency {upstream_work_id} -> {downstream_work_id} would create a cycle")]
    Cycle {
        upstream_work_id: WorkItemId,
        downstream_work_id: WorkItemId,
    },
    #[error(
        "work dependency {upstream_work_id} -> {downstream_work_id} already exists with different content"
    )]
    Conflict {
        upstream_work_id: WorkItemId,
        downstream_work_id: WorkItemId,
    },
    #[error("work dependency timestamp must not be blank")]
    InvalidTimestamp,
    #[error("dependency runtime failed: {0}")]
    Runtime(String),
}

#[allow(async_fn_in_trait)]
pub trait DependencyRuntime {
    async fn add_work_dependency(
        &mut self,
        upstream_work_id: WorkItemId,
        downstream_work_id: WorkItemId,
        created_at: String,
    ) -> Result<WorkDependency, DependencyError>;

    async fn list_work_dependencies_for_downstream(
        &mut self,
        downstream_work_id: WorkItemId,
    ) -> Result<Vec<WorkDependency>, DependencyError>;
}

pub struct DependencyService<R> {
    runtime: R,
}

impl<R: DependencyRuntime> DependencyService<R> {
    pub fn new(runtime: R) -> Self {
        Self { runtime }
    }

    pub async fn add(
        &mut self,
        command: AddWorkDependency,
    ) -> Result<WorkDependency, DependencyError> {
        self.runtime
            .add_work_dependency(
                command.upstream_work_id,
                command.downstream_work_id,
                command.created_at,
            )
            .await
    }

    pub async fn list_for_downstream(
        &mut self,
        downstream_work_id: WorkItemId,
    ) -> Result<Vec<WorkDependency>, DependencyError> {
        self.runtime
            .list_work_dependencies_for_downstream(downstream_work_id)
            .await
    }
}
