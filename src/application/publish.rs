use crate::domain::{ConversationId, Publish, PublishId, ResultId, WorkItemId, WorkResult};
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishResult {
    pub publish_id: PublishId,
    pub result_id: ResultId,
    pub target_conversation_id: ConversationId,
    pub published_at: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PublishedResult {
    pub publish_id: PublishId,
    pub result: WorkResult,
    pub source_conversation_id: ConversationId,
    pub target_conversation_id: ConversationId,
    pub published_at: String,
}

impl From<(Publish, WorkResult)> for PublishedResult {
    fn from((publish, result): (Publish, WorkResult)) -> Self {
        Self {
            publish_id: publish.id,
            result,
            source_conversation_id: publish.source_conversation_id,
            target_conversation_id: publish.target_conversation_id,
            published_at: publish.created_at,
        }
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum PublishError {
    #[error("result {0} does not exist")]
    ResultNotFound(ResultId),
    #[error("work {0} does not exist")]
    WorkNotFound(WorkItemId),
    #[error("source conversation {0} does not exist")]
    SourceNotFound(ConversationId),
    #[error("target conversation {0} does not exist")]
    TargetNotFound(ConversationId),
    #[error("result source and publish target cannot both be conversation {0}")]
    SourceEqualsTarget(ConversationId),
    #[error("publish id {0} already maps a different result or target")]
    PublishIdConflict(PublishId),
    #[error("publish timestamp must not be blank")]
    InvalidTimestamp,
    #[error("publish runtime failed: {0}")]
    Runtime(String),
}

#[allow(async_fn_in_trait)]
pub trait PublishRuntime {
    async fn publish_result(
        &mut self,
        publish_id: PublishId,
        result_id: ResultId,
        target_conversation_id: ConversationId,
        published_at: String,
    ) -> Result<PublishedResult, PublishError>;

    async fn list_published_results(
        &mut self,
        target_conversation_id: ConversationId,
    ) -> Result<Vec<PublishedResult>, PublishError>;
}

pub struct PublishService<R> {
    runtime: R,
}

impl<R: PublishRuntime> PublishService<R> {
    pub fn new(runtime: R) -> Self {
        Self { runtime }
    }

    pub async fn publish(
        &mut self,
        command: PublishResult,
    ) -> Result<PublishedResult, PublishError> {
        self.runtime
            .publish_result(
                command.publish_id,
                command.result_id,
                command.target_conversation_id,
                command.published_at,
            )
            .await
    }

    pub async fn list_for_target(
        &mut self,
        target_conversation_id: ConversationId,
    ) -> Result<Vec<PublishedResult>, PublishError> {
        self.runtime
            .list_published_results(target_conversation_id)
            .await
    }
}
