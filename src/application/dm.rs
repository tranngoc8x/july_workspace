use crate::domain::{
    AgentId, ConversationId, Message, PermissionOption, PermissionOutcome, SessionBindingStatus,
};
use std::fmt::{self, Display, Formatter};
use thiserror::Error;

#[derive(Clone, Debug, PartialEq)]
pub struct OpenedDirectMessage {
    pub conversation_id: ConversationId,
    pub agent_id: AgentId,
    pub agent_name: String,
    pub messages: Vec<Message>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct DirectMessagePermissionRequestId(String);

impl DirectMessagePermissionRequestId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for DirectMessagePermissionRequestId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl Display for DirectMessagePermissionRequestId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DirectMessageFailureKind {
    AuthenticationRequired,
    Protocol,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DirectMessageEvent {
    TextDelta(String),
    MessageCompleted(Message),
    PermissionRequested {
        request_id: DirectMessagePermissionRequestId,
        options: Vec<PermissionOption>,
    },
    TurnCompleted,
    TurnFailed(DirectMessageFailureKind),
    Disconnected(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum DirectMessageRuntimeEvent {
    TextDelta(String),
    AgentMessageCompleted,
    PermissionRequested {
        request_id: DirectMessagePermissionRequestId,
        options: Vec<PermissionOption>,
    },
    TurnCompleted,
    TurnFailed(DirectMessageFailureKind),
    Disconnected(String),
    SessionLost,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum DirectMessageError {
    #[error("agent {0} does not exist")]
    AgentNotFound(String),
    #[error("direct message content cannot be blank")]
    EmptyMessage,
    #[error("direct message is already open")]
    AlreadyOpen,
    #[error("direct message is not open")]
    NotOpen,
    #[error("the durable agent session was lost")]
    SessionLost,
    #[error("the durable agent session is unavailable with status {0}")]
    SessionUnavailable(SessionBindingStatus),
    #[error("transport event does not belong to the open direct message")]
    SessionMismatch,
    #[error("permission request {0} is not pending")]
    PermissionRequestNotFound(String),
    #[error("agent completed an empty message")]
    EmptyAgentMessage,
    #[error("direct message runtime failed: {0}")]
    Runtime(String),
}

#[allow(async_fn_in_trait)]
pub trait DirectMessageRuntime {
    async fn open(
        &mut self,
        user_id: String,
        agent_name: String,
        opened_at: String,
    ) -> Result<OpenedDirectMessage, DirectMessageError>;

    async fn persist_message(&mut self, message: Message) -> Result<(), DirectMessageError>;

    async fn send_exact(&mut self, content: String) -> Result<(), DirectMessageError>;

    async fn next_runtime_event(
        &mut self,
        observed_at: String,
    ) -> Result<Option<DirectMessageRuntimeEvent>, DirectMessageError>;

    async fn respond_permission(
        &mut self,
        request_id: DirectMessagePermissionRequestId,
        outcome: PermissionOutcome,
        decided_at: String,
    ) -> Result<(), DirectMessageError>;

    async fn cancel_turn(&mut self, cancelled_at: String) -> Result<(), DirectMessageError>;

    async fn shutdown(&mut self, stopped_at: String) -> Result<(), DirectMessageError>;
}

pub struct DirectMessageService<R: DirectMessageRuntime> {
    runtime: R,
    active: Option<ActiveDirectMessage>,
    pending_completion: Option<Message>,
}

struct ActiveDirectMessage {
    conversation_id: ConversationId,
    agent_id: AgentId,
    user_id: String,
    response: String,
}

impl<R: DirectMessageRuntime> DirectMessageService<R> {
    pub fn new(runtime: R) -> Self {
        Self {
            runtime,
            active: None,
            pending_completion: None,
        }
    }

    pub async fn open(
        &mut self,
        user_id: String,
        agent_name: String,
        opened_at: String,
    ) -> Result<OpenedDirectMessage, DirectMessageError> {
        if self.active.is_some() {
            return Err(DirectMessageError::AlreadyOpen);
        }
        let opened = self
            .runtime
            .open(user_id.clone(), agent_name, opened_at)
            .await?;
        self.active = Some(ActiveDirectMessage {
            conversation_id: opened.conversation_id,
            agent_id: opened.agent_id,
            user_id,
            response: String::new(),
        });
        Ok(opened)
    }

    pub async fn send_message(
        &mut self,
        content: String,
        sent_at: String,
    ) -> Result<(), DirectMessageError> {
        if content.trim().is_empty() {
            return Err(DirectMessageError::EmptyMessage);
        }
        let active = self.active.as_ref().ok_or(DirectMessageError::NotOpen)?;
        self.runtime
            .persist_message(Message {
                id: Default::default(),
                conversation_id: active.conversation_id,
                sender_type: crate::domain::MemberType::User,
                sender_id: active.user_id.clone(),
                body: content.clone(),
                reply_to: None,
                metadata: message_metadata("outbound"),
                created_at: sent_at,
            })
            .await?;
        self.runtime.send_exact(content).await
    }

    pub async fn next_event(
        &mut self,
        observed_at: String,
    ) -> Result<Option<DirectMessageEvent>, DirectMessageError> {
        if self.pending_completion.is_some() {
            return self.persist_completion().await.map(Some);
        }

        let event = self.runtime.next_runtime_event(observed_at.clone()).await?;
        let Some(event) = event else {
            self.active
                .as_mut()
                .ok_or(DirectMessageError::NotOpen)?
                .response
                .clear();
            return Ok(None);
        };
        match event {
            DirectMessageRuntimeEvent::TextDelta(text) => {
                self.active
                    .as_mut()
                    .ok_or(DirectMessageError::NotOpen)?
                    .response
                    .push_str(&text);
                Ok(Some(DirectMessageEvent::TextDelta(text)))
            }
            DirectMessageRuntimeEvent::AgentMessageCompleted => {
                let active = self.active.as_mut().ok_or(DirectMessageError::NotOpen)?;
                if active.response.trim().is_empty() {
                    return Err(DirectMessageError::EmptyAgentMessage);
                }
                self.pending_completion = Some(Message {
                    id: Default::default(),
                    conversation_id: active.conversation_id,
                    sender_type: crate::domain::MemberType::Agent,
                    sender_id: active.agent_id.to_string(),
                    body: std::mem::take(&mut active.response),
                    reply_to: None,
                    metadata: message_metadata("inbound"),
                    created_at: observed_at,
                });
                self.persist_completion().await.map(Some)
            }
            DirectMessageRuntimeEvent::PermissionRequested {
                request_id,
                options,
            } => Ok(Some(DirectMessageEvent::PermissionRequested {
                request_id,
                options,
            })),
            DirectMessageRuntimeEvent::TurnCompleted => Ok(Some(DirectMessageEvent::TurnCompleted)),
            DirectMessageRuntimeEvent::TurnFailed(failure) => {
                self.clear_response()?;
                Ok(Some(DirectMessageEvent::TurnFailed(failure)))
            }
            DirectMessageRuntimeEvent::Disconnected(reason) => {
                self.clear_response()?;
                Ok(Some(DirectMessageEvent::Disconnected(reason)))
            }
            DirectMessageRuntimeEvent::SessionLost => {
                self.clear_response()?;
                Err(DirectMessageError::SessionLost)
            }
        }
    }

    pub async fn respond_permission(
        &mut self,
        request_id: DirectMessagePermissionRequestId,
        outcome: PermissionOutcome,
        decided_at: String,
    ) -> Result<(), DirectMessageError> {
        self.runtime
            .respond_permission(request_id, outcome, decided_at)
            .await
    }

    pub async fn cancel_turn(&mut self, cancelled_at: String) -> Result<(), DirectMessageError> {
        self.runtime.cancel_turn(cancelled_at).await
    }

    pub async fn shutdown(&mut self, stopped_at: String) -> Result<(), DirectMessageError> {
        let persistence = if self.pending_completion.is_some() {
            self.persist_completion().await.map(|_| ())
        } else {
            Ok(())
        };
        self.active = None;
        let shutdown = self.runtime.shutdown(stopped_at).await;
        persistence?;
        shutdown
    }

    async fn persist_completion(&mut self) -> Result<DirectMessageEvent, DirectMessageError> {
        let message = self
            .pending_completion
            .as_ref()
            .expect("pending completion was checked")
            .clone();
        self.runtime.persist_message(message.clone()).await?;
        self.pending_completion = None;
        Ok(DirectMessageEvent::MessageCompleted(message))
    }

    fn clear_response(&mut self) -> Result<(), DirectMessageError> {
        self.active
            .as_mut()
            .ok_or(DirectMessageError::NotOpen)?
            .response
            .clear();
        Ok(())
    }
}

fn message_metadata(direction: &str) -> serde_json::Value {
    serde_json::json!({"july": {"schema": 1, "channel": "dm", "direction": direction}})
}
