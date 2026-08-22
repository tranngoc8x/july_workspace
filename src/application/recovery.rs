use crate::domain::{
    Agent, AgentId, Checkpoint, Conversation, ConversationId, ConversationKind, Memory, Message,
    Publish, WorkResult,
};
use serde_json::{Map, Value, json};
use thiserror::Error;

pub const RECENT_MESSAGE_LIMIT: usize = 20;
const RECOVERY_VERSION: &str = "JULY_RECOVERY_V1";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BuildRecoveryCapsule {
    pub conversation_id: ConversationId,
    pub agent_id: AgentId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryCapsule {
    pub content: String,
    pub recent_message_count: usize,
    pub messages_truncated: bool,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum RecoveryError {
    #[error("agent {0} does not exist")]
    AgentNotFound(AgentId),
    #[error("conversation {0} does not exist")]
    ConversationNotFound(ConversationId),
    #[error("agent {0} is not active")]
    AgentInactive(AgentId),
    #[error("agent {agent_id} is not an active member of conversation {conversation_id}")]
    AgentNotMember {
        conversation_id: ConversationId,
        agent_id: AgentId,
    },
    #[error("checkpoint {checkpoint_id} has invalid message anchor {message_id}")]
    InvalidCheckpointAnchor {
        checkpoint_id: crate::domain::CheckpointId,
        message_id: crate::domain::MessageId,
    },
    #[error("recovery runtime failed: {0}")]
    Runtime(String),
}

#[allow(async_fn_in_trait)]
pub trait RecoveryRuntime {
    async fn build_recovery_capsule(
        &mut self,
        command: BuildRecoveryCapsule,
    ) -> Result<RecoveryCapsule, RecoveryError>;
}

pub struct RecoveryService<R> {
    runtime: R,
}

impl<R: RecoveryRuntime> RecoveryService<R> {
    pub fn new(runtime: R) -> Self {
        Self { runtime }
    }

    pub async fn build(
        &mut self,
        command: BuildRecoveryCapsule,
    ) -> Result<RecoveryCapsule, RecoveryError> {
        self.runtime.build_recovery_capsule(command).await
    }
}

pub(crate) struct RecoveryInput {
    pub agent: Agent,
    pub conversation: Conversation,
    pub memories: Vec<Memory>,
    pub checkpoint: Option<Checkpoint>,
    pub published_results: Vec<(Publish, WorkResult)>,
    pub messages: Vec<Message>,
    pub messages_truncated: bool,
}

pub(crate) fn format_recovery_capsule(
    input: RecoveryInput,
) -> Result<RecoveryCapsule, RecoveryError> {
    let recent_message_count = input.messages.len();
    let mut document = Map::new();
    document.insert("version".into(), json!(RECOVERY_VERSION));
    document.insert(
        "agent".into(),
        json!({
            "id": input.agent.id.to_string(),
            "name": input.agent.name,
            "project_root": input.agent.project_root,
        }),
    );
    document.insert(
        "conversation".into(),
        json!({
            "id": input.conversation.id.to_string(),
            "kind": input.conversation.kind.to_string(),
            "room_id": input.conversation.room_id.map(|id| id.to_string()),
            "title": input.conversation.title,
            "goal": input.conversation.goal,
        }),
    );
    document.insert(
        "project_memories".into(),
        Value::Array(
            input
                .memories
                .iter()
                .filter(|memory| memory.scope_type == crate::domain::MemoryScopeType::Project)
                .map(memory_value)
                .collect(),
        ),
    );
    if input.conversation.kind == ConversationKind::Thread {
        document.insert(
            "room_memories".into(),
            Value::Array(
                input
                    .memories
                    .iter()
                    .filter(|memory| memory.scope_type == crate::domain::MemoryScopeType::Room)
                    .map(memory_value)
                    .collect(),
            ),
        );
    }
    document.insert(
        "checkpoint".into(),
        input.checkpoint.map_or(Value::Null, |checkpoint| {
            json!({
                "goal": checkpoint.goal,
                "current_state": checkpoint.current_state,
                "decisions": checkpoint.decisions,
                "open_items": checkpoint.open_items,
                "references": checkpoint.references,
                "last_message_id": checkpoint.last_message_id.map(|id| id.to_string()),
            })
        }),
    );
    document.insert(
        "published_results".into(),
        Value::Array(
            input
                .published_results
                .into_iter()
                .map(|(publish, result)| {
                    json!({
                        "publish_id": publish.id.to_string(),
                        "result_id": result.id.to_string(),
                        "source_conversation_id": publish.source_conversation_id.to_string(),
                        "status": result.status,
                        "summary": result.summary,
                    })
                })
                .collect(),
        ),
    );
    document.insert(
        "recent_messages".into(),
        Value::Array(
            input
                .messages
                .into_iter()
                .map(|message| {
                    json!({
                        "id": message.id.to_string(),
                        "sender_type": message.sender_type.to_string(),
                        "sender_id": message.sender_id,
                        "body": message.body,
                        "reply_to": message.reply_to.map(|id| id.to_string()),
                        "created_at": message.created_at,
                    })
                })
                .collect(),
        ),
    );

    Ok(RecoveryCapsule {
        content: serde_json::to_string_pretty(&document)
            .map_err(|error| RecoveryError::Runtime(error.to_string()))?,
        recent_message_count,
        messages_truncated: input.messages_truncated,
    })
}

fn memory_value(memory: &Memory) -> Value {
    json!({
        "id": memory.id.to_string(),
        "scope_type": memory.scope_type.to_string(),
        "scope_id": memory.scope_id,
        "kind": memory.kind.to_string(),
        "content": memory.content,
        "source_conversation_id": memory.source_conversation_id.map(|id| id.to_string()),
        "evidence": memory.evidence,
        "supersedes_memory_id": memory.supersedes_memory_id.map(|id| id.to_string()),
        "created_at": memory.created_at,
    })
}
