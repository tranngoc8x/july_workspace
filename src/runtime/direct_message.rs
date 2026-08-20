use super::{RuntimeError, RuntimeSession, StorageWorker, WorkspaceHandle, WorkspaceRuntime};
use crate::application::{
    DirectMessageError, DirectMessageFailureKind, DirectMessagePermissionRequestId,
    DirectMessageRuntime, DirectMessageRuntimeEvent, OpenAgentDirectMessage, OpenedDirectMessage,
};
use crate::domain::{
    Agent, AgentId, ConversationId, Message, PermissionOutcome, SessionBinding,
    SessionBindingStatus,
};
use crate::storage::SqliteStore;
use crate::transport::{
    AcpAgentConfig, AcpTransport, AgentConnection, AgentTransport, PermissionRequestId, SessionRef,
    TransportError, TransportEvent, TransportFailureKind,
};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use thiserror::Error;

struct ActiveDirectMessage {
    agent_id: AgentId,
    session: SessionRef,
    permissions: HashMap<String, SessionRef>,
}

pub struct AgentDirectMessageRuntime<T: AgentTransport + Send + 'static> {
    workspace: WorkspaceHandle<T>,
    transport: Option<T>,
    session: Option<RuntimeSession>,
    active: Option<ActiveDirectMessage>,
    agent: Option<Agent>,
    expected_agent_id: Option<AgentId>,
    stopped: bool,
}

impl<T: AgentTransport + Send + 'static> AgentDirectMessageRuntime<T> {
    pub(crate) fn from_workspace(
        workspace: WorkspaceHandle<T>,
        transport: Option<T>,
        expected_agent_id: Option<AgentId>,
    ) -> Self {
        Self {
            workspace,
            transport,
            session: None,
            active: None,
            agent: None,
            expected_agent_id,
            stopped: false,
        }
    }

    pub(crate) fn with_agent(mut self, agent: Agent) -> Self {
        self.agent = Some(agent);
        self
    }

    fn require_active(&self) -> Result<&ActiveDirectMessage, DirectMessageError> {
        self.active.as_ref().ok_or(DirectMessageError::NotOpen)
    }

    fn require_session(&self, session: &SessionRef) -> Result<(), DirectMessageError> {
        let active = self.active.as_ref().ok_or(DirectMessageError::NotOpen)?;
        if active.session == *session {
            Ok(())
        } else {
            Err(DirectMessageError::SessionMismatch)
        }
    }

    async fn finish_open(
        &mut self,
        agent: Agent,
        conversation_id: ConversationId,
        messages: Vec<Message>,
        opened_at: String,
    ) -> Result<OpenedDirectMessage, DirectMessageError> {
        let storage = self.workspace.storage();
        let binding = storage
            .get_latest_session_binding(conversation_id, agent.id)
            .await
            .map_err(runtime_error)?;

        match binding.as_ref().map(|binding| binding.status) {
            Some(SessionBindingStatus::Lost) => return Err(DirectMessageError::SessionLost),
            Some(SessionBindingStatus::Closed) => {
                return Err(DirectMessageError::SessionUnavailable(
                    SessionBindingStatus::Closed,
                ));
            }
            Some(SessionBindingStatus::Active | SessionBindingStatus::Disconnected)
                if binding
                    .as_ref()
                    .is_some_and(|binding| binding.remote_session_id.is_none()) =>
            {
                let binding = binding.as_ref().expect("binding status was present");
                storage
                    .update_session_binding_status(
                        binding.id,
                        SessionBindingStatus::Lost,
                        opened_at,
                    )
                    .await
                    .map_err(runtime_error)?;
                return Err(DirectMessageError::SessionLost);
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
            conversation_id,
            agent_id: agent.id,
            transport_type: agent.transport_type.clone(),
            remote_session_id: None,
            generation: 1,
            status: SessionBindingStatus::Active,
            created_at: opened_at.clone(),
            last_used_at: opened_at.clone(),
        });
        let session = self
            .workspace
            .open_session(agent.id, binding, project_root, opened_at)
            .await
            .map_err(runtime_error)?;
        let session_ref = session.session().clone();

        self.active = Some(ActiveDirectMessage {
            agent_id: agent.id,
            session: session_ref,
            permissions: HashMap::new(),
        });
        self.session = Some(session);
        Ok(OpenedDirectMessage {
            conversation_id,
            agent_id: agent.id,
            agent_name: agent.name,
            messages,
        })
    }
}

#[derive(Debug, Error)]
pub enum DirectMessageBootstrapError {
    #[error("agent {0} does not exist")]
    AgentNotFound(String),
    #[error("agent {agent} is not active")]
    AgentInactive { agent: String },
    #[error("agent {agent} uses unsupported transport {transport}")]
    UnsupportedTransport { agent: String, transport: String },
    #[error("invalid ACP transport configuration: {0}")]
    InvalidConfiguration(String),
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    #[error(transparent)]
    Storage(#[from] crate::storage::StoreError),
}

pub fn open_acp_direct_message(
    database_path: impl AsRef<Path>,
    agent_name: &str,
) -> Result<
    (
        WorkspaceRuntime<AcpTransport>,
        AgentDirectMessageRuntime<AcpTransport>,
    ),
    DirectMessageBootstrapError,
> {
    let store = SqliteStore::open(database_path.as_ref())?;
    let agent = store
        .get_agent_by_name(agent_name)?
        .ok_or_else(|| DirectMessageBootstrapError::AgentNotFound(agent_name.into()))?;
    if agent.status != "active" {
        return Err(DirectMessageBootstrapError::AgentInactive { agent: agent.name });
    }
    if agent.transport_type != "acp" {
        return Err(DirectMessageBootstrapError::UnsupportedTransport {
            agent: agent.name,
            transport: agent.transport_type,
        });
    }
    let config = parse_acp_config(&agent.transport_config)?;
    drop(store);
    let storage = StorageWorker::open(database_path)?;
    let workspace = WorkspaceRuntime::new(storage)?;
    let runtime = workspace.direct_message_with_agent(AcpTransport::new(config), agent);
    Ok((workspace, runtime))
}

fn parse_acp_config(value: &Value) -> Result<AcpAgentConfig, DirectMessageBootstrapError> {
    const FIELDS: [&str; 6] = [
        "executable",
        "arguments",
        "environment",
        "state_directory",
        "expected_agent_name",
        "expected_agent_version",
    ];
    let object = value
        .as_object()
        .ok_or_else(|| invalid("expected an object"))?;
    if let Some(field) = object
        .keys()
        .find(|field| !FIELDS.contains(&field.as_str()))
    {
        return Err(invalid(format!("unknown field `{field}`")));
    }
    Ok(AcpAgentConfig {
        executable: string(object, "executable")?.into(),
        arguments: strings(object, "arguments")?,
        environment: string_map(object, "environment")?,
        state_directory: string(object, "state_directory")?.into(),
        expected_agent_name: string(object, "expected_agent_name")?,
        expected_agent_version: string(object, "expected_agent_version")?,
    })
}

fn string(object: &Map<String, Value>, field: &str) -> Result<String, DirectMessageBootstrapError> {
    object
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| invalid(format!("field `{field}` must be a non-empty string")))
}

fn strings(
    object: &Map<String, Value>,
    field: &str,
) -> Result<Vec<String>, DirectMessageBootstrapError> {
    object
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| invalid(format!("field `{field}` must be an array")))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| invalid(format!("field `{field}` must contain only strings")))
        })
        .collect()
}

fn string_map(
    object: &Map<String, Value>,
    field: &str,
) -> Result<BTreeMap<String, String>, DirectMessageBootstrapError> {
    object
        .get(field)
        .and_then(Value::as_object)
        .ok_or_else(|| invalid(format!("field `{field}` must be an object")))?
        .iter()
        .map(|(key, value)| {
            value
                .as_str()
                .map(|value| (key.clone(), value.to_owned()))
                .ok_or_else(|| invalid(format!("field `{field}` values must be strings")))
        })
        .collect()
}

fn invalid(message: impl Into<String>) -> DirectMessageBootstrapError {
    DirectMessageBootstrapError::InvalidConfiguration(message.into())
}

impl<T: AgentTransport + Send + 'static> DirectMessageRuntime for AgentDirectMessageRuntime<T> {
    async fn open(
        &mut self,
        user_id: String,
        agent_name: String,
        opened_at: String,
    ) -> Result<OpenedDirectMessage, DirectMessageError> {
        self.workspace.ensure_running().map_err(runtime_error)?;
        if self.active.is_some() || self.session.is_some() {
            return Err(DirectMessageError::AlreadyOpen);
        }
        let storage = self.workspace.storage();
        let agent = match self.agent.take() {
            Some(agent) if agent.name == agent_name => agent,
            Some(agent) => {
                self.agent = Some(agent);
                return Err(DirectMessageError::AgentNotFound(agent_name));
            }
            None => storage
                .get_agent_by_name(agent_name.clone())
                .await
                .map_err(runtime_error)?
                .ok_or_else(|| DirectMessageError::AgentNotFound(agent_name.clone()))?,
        };
        if self
            .expected_agent_id
            .is_some_and(|expected| expected != agent.id)
        {
            return Err(DirectMessageError::AgentNotFound(agent_name));
        }
        let conversation = storage
            .get_or_create_dm(user_id, agent.id, opened_at.clone())
            .await
            .map_err(runtime_error)?;
        let messages = storage
            .list_messages(conversation.id)
            .await
            .map_err(runtime_error)?;
        self.finish_open(agent, conversation.id, messages, opened_at)
            .await
    }

    async fn open_agent(
        &mut self,
        command: OpenAgentDirectMessage,
    ) -> Result<OpenedDirectMessage, DirectMessageError> {
        self.workspace.ensure_running().map_err(runtime_error)?;
        if self.active.is_some() || self.session.is_some() {
            return Err(DirectMessageError::AlreadyOpen);
        }
        if self
            .expected_agent_id
            .is_some_and(|expected| expected != command.target_agent_id)
        {
            return Err(DirectMessageError::AgentNotFound(
                command.target_agent_id.to_string(),
            ));
        }
        let storage = self.workspace.storage();
        let conversation = storage
            .get_or_create_agent_dm(
                command.source_agent_id,
                command.target_agent_id,
                command.opened_at.clone(),
            )
            .await
            .map_err(runtime_error)?;
        let agent = storage
            .get_agent(command.target_agent_id)
            .await
            .map_err(runtime_error)?
            .ok_or_else(|| {
                DirectMessageError::AgentNotFound(command.target_agent_id.to_string())
            })?;
        let messages = storage
            .list_messages(conversation.id)
            .await
            .map_err(runtime_error)?;
        self.finish_open(agent, conversation.id, messages, command.opened_at)
            .await
    }

    async fn persist_message(&mut self, message: Message) -> Result<(), DirectMessageError> {
        self.workspace
            .storage()
            .insert_message(message)
            .await
            .map_err(runtime_error)
    }

    async fn send_exact(&mut self, content: String) -> Result<(), DirectMessageError> {
        self.require_active()?;
        self.session
            .as_ref()
            .ok_or(DirectMessageError::NotOpen)?
            .send_message(content)
            .await
            .map_err(runtime_error)
    }

    async fn next_runtime_event(
        &mut self,
        _observed_at: String,
    ) -> Result<Option<DirectMessageRuntimeEvent>, DirectMessageError> {
        loop {
            let event = self
                .session
                .as_mut()
                .ok_or(DirectMessageError::NotOpen)?
                .next_event()
                .await;
            let Some(event) = event else { return Ok(None) };
            match event {
                TransportEvent::AgentTextDelta { session, text } => {
                    self.require_session(&session)?;
                    return Ok(Some(DirectMessageRuntimeEvent::TextDelta(text)));
                }
                TransportEvent::AgentMessageCompleted { session } => {
                    self.require_session(&session)?;
                    return Ok(Some(DirectMessageRuntimeEvent::AgentMessageCompleted));
                }
                TransportEvent::PermissionRequested(request) => {
                    self.require_session(&request.session)?;
                    let request_id = request.request_id.to_string();
                    self.active
                        .as_mut()
                        .expect("active checked")
                        .permissions
                        .insert(request_id.clone(), request.session);
                    return Ok(Some(DirectMessageRuntimeEvent::PermissionRequested {
                        request_id: request_id.into(),
                        options: request.options,
                    }));
                }
                TransportEvent::TurnCompleted { session } => {
                    self.require_session(&session)?;
                    return Ok(Some(DirectMessageRuntimeEvent::TurnCompleted));
                }
                TransportEvent::TurnFailed { session, failure } => {
                    self.require_session(&session)?;
                    return Ok(Some(DirectMessageRuntimeEvent::TurnFailed(match failure {
                        TransportFailureKind::AuthenticationRequired => {
                            DirectMessageFailureKind::AuthenticationRequired
                        }
                        TransportFailureKind::Protocol => DirectMessageFailureKind::Protocol,
                    })));
                }
                TransportEvent::TransportDisconnected { agent_id, reason } => {
                    if self.require_active()?.agent_id != agent_id {
                        return Err(DirectMessageError::SessionMismatch);
                    }
                    return Ok(Some(DirectMessageRuntimeEvent::Disconnected(reason)));
                }
                TransportEvent::SessionLost { session } => {
                    self.require_session(&session)?;
                    return Ok(Some(DirectMessageRuntimeEvent::SessionLost));
                }
                TransportEvent::TurnStarted { session }
                | TransportEvent::ToolCallStarted { session, .. }
                | TransportEvent::ToolCallFinished { session, .. }
                | TransportEvent::UsageReported { session, .. } => {
                    self.require_session(&session)?;
                }
            }
        }
    }

    async fn respond_permission(
        &mut self,
        request_id: DirectMessagePermissionRequestId,
        outcome: PermissionOutcome,
        decided_at: String,
    ) -> Result<(), DirectMessageError> {
        self.active
            .as_mut()
            .ok_or(DirectMessageError::NotOpen)?
            .permissions
            .remove(request_id.as_str())
            .ok_or_else(|| DirectMessageError::PermissionRequestNotFound(request_id.to_string()))?;
        self.session
            .as_ref()
            .ok_or(DirectMessageError::NotOpen)?
            .respond_permission(
                PermissionRequestId::from(request_id.to_string()),
                outcome,
                decided_at,
            )
            .await
            .map_err(runtime_error)
    }

    async fn cancel_turn(&mut self, cancelled_at: String) -> Result<(), DirectMessageError> {
        self.require_active()?;
        self.session
            .as_ref()
            .ok_or(DirectMessageError::NotOpen)?
            .cancel_turn(cancelled_at)
            .await
            .map_err(runtime_error)
    }

    async fn shutdown(&mut self, stopped_at: String) -> Result<(), DirectMessageError> {
        if self.stopped {
            return Ok(());
        }
        self.stopped = true;
        self.active = None;
        if let Some(mut session) = self.session.take() {
            session.detach(stopped_at).await.map_err(runtime_error)
        } else {
            Ok(())
        }
    }
}

fn runtime_error(error: RuntimeError) -> DirectMessageError {
    match error {
        RuntimeError::Transport(error) => transport_error(error),
        RuntimeError::MissingRemoteSession => DirectMessageError::SessionLost,
        RuntimeError::PermissionRequestNotFound(id) => {
            DirectMessageError::PermissionRequestNotFound(id)
        }
        RuntimeError::SessionBindingAlreadyAttached(id) => {
            DirectMessageError::SessionAlreadyAttached(id)
        }
        other => DirectMessageError::Runtime(other.to_string()),
    }
}

fn transport_error(error: TransportError) -> DirectMessageError {
    match error {
        TransportError::SessionLost(_) => DirectMessageError::SessionLost,
        TransportError::SessionReferenceMismatch(_) => DirectMessageError::SessionMismatch,
        TransportError::PermissionRequestNotFound(id) => {
            DirectMessageError::PermissionRequestNotFound(id)
        }
        other => DirectMessageError::Runtime(other.to_string()),
    }
}
