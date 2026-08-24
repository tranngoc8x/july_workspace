use super::chat::{ChatEvent, ChatPermissionRequestId, ChatRuntimeEvent};
use super::collaboration::{CollaborationError, OpenThreadForAgent, OpenedThread};
use crate::domain::{AgentId, ConversationId, MemberType, Message, PermissionOutcome};

/// Interactive Thread chat port. `CollaborationService` stays the owner of the
/// operational Room/Thread commands; this port carries only the interactive
/// operations an open Thread context needs.
#[allow(async_fn_in_trait)]
pub trait ThreadChatRuntime {
    async fn open_thread_chat(
        &mut self,
        command: OpenThreadForAgent,
    ) -> Result<OpenedThread, CollaborationError>;

    async fn persist_message(&mut self, message: Message) -> Result<(), CollaborationError>;

    async fn send_exact_message(&mut self, content: String) -> Result<(), CollaborationError>;

    async fn next_runtime_event(
        &mut self,
        observed_at: String,
    ) -> Result<Option<ChatRuntimeEvent>, CollaborationError>;

    async fn respond_permission(
        &mut self,
        request_id: ChatPermissionRequestId,
        outcome: PermissionOutcome,
        decided_at: String,
    ) -> Result<(), CollaborationError>;

    async fn cancel_turn(&mut self, cancelled_at: String) -> Result<(), CollaborationError>;

    async fn shutdown(&mut self, stopped_at: String) -> Result<(), CollaborationError>;
}

pub struct ThreadChatService<R: ThreadChatRuntime> {
    runtime: R,
    active: Option<ActiveThreadChat>,
    pending_completion: Option<Message>,
}

struct ActiveThreadChat {
    thread_id: ConversationId,
    agent_id: AgentId,
    user_id: String,
    response: String,
}

impl<R: ThreadChatRuntime> ThreadChatService<R> {
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
        command: OpenThreadForAgent,
    ) -> Result<OpenedThread, CollaborationError> {
        if self.active.is_some() {
            return Err(CollaborationError::ThreadAlreadyOpen);
        }
        let opened = self.runtime.open_thread_chat(command).await?;
        self.active = Some(ActiveThreadChat {
            thread_id: opened.thread_id,
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
    ) -> Result<(), CollaborationError> {
        if content.trim().is_empty() {
            return Err(CollaborationError::EmptyMessage);
        }
        let active = self
            .active
            .as_ref()
            .ok_or(CollaborationError::ChatNotOpen)?;
        self.runtime
            .persist_message(Message {
                id: Default::default(),
                conversation_id: active.thread_id,
                sender_type: MemberType::User,
                sender_id: active.user_id.clone(),
                body: content.clone(),
                reply_to: None,
                metadata: message_metadata("outbound"),
                created_at: sent_at,
            })
            .await?;
        self.runtime.send_exact_message(content).await
    }

    pub async fn next_event(
        &mut self,
        observed_at: String,
    ) -> Result<Option<ChatEvent>, CollaborationError> {
        if self.pending_completion.is_some() {
            return self.persist_completion().await.map(Some);
        }

        let event = self.runtime.next_runtime_event(observed_at.clone()).await?;
        let Some(event) = event else {
            self.clear_response()?;
            return Ok(None);
        };
        match event {
            ChatRuntimeEvent::TextDelta(text) => {
                self.active
                    .as_mut()
                    .ok_or(CollaborationError::ChatNotOpen)?
                    .response
                    .push_str(&text);
                Ok(Some(ChatEvent::TextDelta(text)))
            }
            ChatRuntimeEvent::AgentMessageCompleted => {
                let active = self
                    .active
                    .as_mut()
                    .ok_or(CollaborationError::ChatNotOpen)?;
                if active.response.trim().is_empty() {
                    return Err(CollaborationError::EmptyAgentMessage);
                }
                self.pending_completion = Some(Message {
                    id: Default::default(),
                    conversation_id: active.thread_id,
                    sender_type: MemberType::Agent,
                    sender_id: active.agent_id.to_string(),
                    body: std::mem::take(&mut active.response),
                    reply_to: None,
                    metadata: message_metadata("inbound"),
                    created_at: observed_at,
                });
                self.persist_completion().await.map(Some)
            }
            ChatRuntimeEvent::PermissionRequested {
                request_id,
                options,
            } => Ok(Some(ChatEvent::PermissionRequested {
                request_id,
                options,
            })),
            ChatRuntimeEvent::TurnCompleted => Ok(Some(ChatEvent::TurnCompleted)),
            ChatRuntimeEvent::TurnFailed(failure) => {
                self.clear_response()?;
                Ok(Some(ChatEvent::TurnFailed(failure)))
            }
            ChatRuntimeEvent::Disconnected(reason) => {
                self.clear_response()?;
                Ok(Some(ChatEvent::Disconnected(reason)))
            }
            ChatRuntimeEvent::SessionLost => {
                self.clear_response()?;
                Err(CollaborationError::SessionLost)
            }
        }
    }

    pub async fn respond_permission(
        &mut self,
        request_id: ChatPermissionRequestId,
        outcome: PermissionOutcome,
        decided_at: String,
    ) -> Result<(), CollaborationError> {
        self.runtime
            .respond_permission(request_id, outcome, decided_at)
            .await
    }

    pub async fn cancel_turn(&mut self, cancelled_at: String) -> Result<(), CollaborationError> {
        self.runtime.cancel_turn(cancelled_at).await
    }

    pub async fn shutdown(&mut self, stopped_at: String) -> Result<(), CollaborationError> {
        if self.pending_completion.is_some() {
            self.persist_completion().await?;
        }
        self.runtime.shutdown(stopped_at).await?;
        self.active = None;
        Ok(())
    }

    async fn persist_completion(&mut self) -> Result<ChatEvent, CollaborationError> {
        let message = self
            .pending_completion
            .as_ref()
            .expect("pending completion was checked")
            .clone();
        self.runtime.persist_message(message.clone()).await?;
        self.pending_completion = None;
        Ok(ChatEvent::MessageCompleted(message))
    }

    fn clear_response(&mut self) -> Result<(), CollaborationError> {
        self.active
            .as_mut()
            .ok_or(CollaborationError::ChatNotOpen)?
            .response
            .clear();
        Ok(())
    }
}

fn message_metadata(direction: &str) -> serde_json::Value {
    serde_json::json!({"july": {"schema": 1, "channel": "thread", "direction": direction}})
}
