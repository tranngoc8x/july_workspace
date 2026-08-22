use super::{RuntimeError, RuntimeSession, WorkspaceHandle};
use crate::application::{
    CollaborationError, MentionThreadAgent, MentionedThreadAgent, OpenThreadForAgent, OpenedThread,
    ThreadRuntime,
};
use crate::domain::{
    AgentId, MemberType, Message, PermissionOutcome, SessionBinding, SessionBindingStatus,
};
use crate::transport::{
    AgentConnection, AgentTransport, PermissionRequestId, TransportError, TransportEvent,
};
use std::path::PathBuf;

pub struct AgentThreadRuntime<T: AgentTransport + Send + 'static> {
    workspace: WorkspaceHandle<T>,
    transport: Option<T>,
    session: Option<RuntimeSession>,
    expected_agent_id: Option<AgentId>,
    opened: Option<OpenedThread>,
    stopped: bool,
}

impl<T: AgentTransport + Send + 'static> AgentThreadRuntime<T> {
    pub(crate) fn from_workspace(
        workspace: WorkspaceHandle<T>,
        transport: Option<T>,
        expected_agent_id: Option<AgentId>,
    ) -> Self {
        Self {
            workspace,
            transport,
            session: None,
            expected_agent_id,
            opened: None,
            stopped: false,
        }
    }

    async fn open(
        &mut self,
        command: OpenThreadForAgent,
    ) -> Result<OpenedThread, CollaborationError> {
        let (agent, thread, binding) = self
            .workspace
            .storage()
            .admit_thread_session(
                command.thread_id,
                command.agent_id,
                command.opened_at.clone(),
            )
            .await?;
        match binding.as_ref().map(|binding| binding.status) {
            Some(SessionBindingStatus::Lost) => return Err(CollaborationError::SessionLost),
            Some(SessionBindingStatus::Closed) => {
                return Err(CollaborationError::SessionUnavailable(
                    SessionBindingStatus::Closed,
                ));
            }
            _ => {}
        }

        let project_root = PathBuf::from(&agent.project_root);
        if let Some(transport) = self.transport.take() {
            self.workspace
                .register_agent(
                    AgentConnection {
                        agent_id: agent.id,
                        project_root: project_root.clone(),
                    },
                    transport,
                )
                .await
                .map_err(runtime_error)?;
        }
        let binding = binding.unwrap_or_else(|| SessionBinding {
            id: Default::default(),
            conversation_id: thread.id,
            agent_id: agent.id,
            transport_type: agent.transport_type,
            remote_session_id: None,
            generation: 1,
            status: SessionBindingStatus::Active,
            created_at: command.opened_at.clone(),
            last_used_at: command.opened_at.clone(),
        });
        let session = self
            .workspace
            .open_session(agent.id, binding, project_root, command.opened_at)
            .await
            .map_err(runtime_error)?;
        let opened = OpenedThread {
            thread_id: thread.id,
            room_id: thread.room_id.expect("admitted Thread has a Room"),
            agent_id: agent.id,
            session_binding_id: session.session().binding_id,
        };
        self.opened = Some(opened);
        self.session = Some(session);
        Ok(opened)
    }

    pub async fn next_event(&mut self) -> Result<Option<TransportEvent>, RuntimeError> {
        Ok(match self.session.as_mut() {
            Some(session) => session.next_event().await,
            None => return Err(RuntimeError::ChannelClosed),
        })
    }

    pub async fn send_exact(&self, content: String) -> Result<(), RuntimeError> {
        self.session
            .as_ref()
            .ok_or(RuntimeError::ChannelClosed)?
            .send_message(content)
            .await
    }

    pub async fn respond_permission(
        &self,
        request_id: PermissionRequestId,
        outcome: PermissionOutcome,
        decided_at: String,
    ) -> Result<(), RuntimeError> {
        self.session
            .as_ref()
            .ok_or(RuntimeError::ChannelClosed)?
            .respond_permission(request_id, outcome, decided_at)
            .await
    }
}

impl<T: AgentTransport + Send + 'static> ThreadRuntime for AgentThreadRuntime<T> {
    async fn open_thread_for_agent(
        &mut self,
        command: OpenThreadForAgent,
    ) -> Result<OpenedThread, CollaborationError> {
        if self.stopped {
            return Err(CollaborationError::ContextStopped);
        }
        self.workspace.ensure_running().map_err(runtime_error)?;
        if self.opened.is_some() || self.session.is_some() {
            return Err(CollaborationError::ThreadAlreadyOpen);
        }
        if self
            .expected_agent_id
            .is_some_and(|expected| expected != command.agent_id)
        {
            return Err(CollaborationError::AgentNotFound(
                command.agent_id.to_string(),
            ));
        }
        self.open(command).await
    }

    async fn mention_thread_agent(
        &mut self,
        command: MentionThreadAgent,
    ) -> Result<MentionedThreadAgent, CollaborationError> {
        if self.stopped {
            return Err(CollaborationError::ContextStopped);
        }
        self.workspace.ensure_running().map_err(runtime_error)?;
        let expected_agent_id = self
            .expected_agent_id
            .ok_or(CollaborationError::AgentTargetNotBound)?;
        if expected_agent_id != command.target_agent_id {
            return Err(CollaborationError::AgentNotFound(
                command.target_agent_id.to_string(),
            ));
        }
        if command.body.trim().is_empty() {
            return Err(CollaborationError::InvalidCommand(
                "thread mention body cannot be blank".into(),
            ));
        }
        if command.capsule.trim().is_empty() {
            return Err(CollaborationError::InvalidCommand(
                "thread mention capsule cannot be blank".into(),
            ));
        }
        if self.opened.is_some_and(|opened| {
            opened.thread_id != command.thread_id || opened.agent_id != command.target_agent_id
        }) || (self.opened.is_none() && self.session.is_some())
        {
            return Err(CollaborationError::ThreadAlreadyOpen);
        }

        let membership_changed = self
            .workspace
            .storage()
            .persist_thread_mention(
                Message {
                    id: command.message_id,
                    conversation_id: command.thread_id,
                    sender_type: MemberType::Agent,
                    sender_id: command.source_agent_id.to_string(),
                    body: command.body.clone(),
                    reply_to: None,
                    metadata: serde_json::json!({
                        "mention": command.target_agent_id.to_string(),
                    }),
                    created_at: command.mentioned_at.clone(),
                },
                command.source_agent_id,
                command.target_agent_id,
            )
            .await?;
        let opened = match self.opened {
            Some(opened) => opened,
            None => {
                self.open(OpenThreadForAgent {
                    thread_id: command.thread_id,
                    agent_id: command.target_agent_id,
                    opened_at: command.mentioned_at,
                })
                .await?
            }
        };
        if membership_changed {
            self.send_exact(command.capsule)
                .await
                .map_err(runtime_error)?;
        }
        self.send_exact(command.body).await.map_err(runtime_error)?;
        Ok(MentionedThreadAgent {
            opened,
            membership_changed,
        })
    }

    async fn shutdown(&mut self, stopped_at: String) -> Result<(), CollaborationError> {
        if self.stopped && self.session.is_none() {
            return Ok(());
        }
        self.stopped = true;
        self.opened = None;
        if let Some(mut session) = self.session.take() {
            match session.detach(stopped_at).await {
                Ok(()) => Ok(()),
                Err(error) => {
                    self.session = Some(session);
                    Err(runtime_error(error))
                }
            }
        } else {
            Ok(())
        }
    }
}

fn runtime_error(error: RuntimeError) -> CollaborationError {
    match error {
        RuntimeError::Transport(error) => transport_error(error),
        RuntimeError::MissingRemoteSession => CollaborationError::SessionLost,
        RuntimeError::SessionBindingAlreadyAttached(id) => {
            CollaborationError::SessionAlreadyAttached(id)
        }
        other => CollaborationError::Runtime(other.to_string()),
    }
}

fn transport_error(error: TransportError) -> CollaborationError {
    match error {
        TransportError::SessionLost(_) => CollaborationError::SessionLost,
        other => CollaborationError::Runtime(other.to_string()),
    }
}
