use super::{RuntimeError, SessionManager, StorageWorker};
use crate::application::{
    DirectMessageError, DirectMessageFailureKind, DirectMessagePermissionRequestId,
    DirectMessageRuntime, DirectMessageRuntimeEvent, OpenedDirectMessage,
};
use crate::domain::{
    Agent, AgentId, Message, PermissionOutcome, SessionBinding, SessionBindingStatus,
};
use crate::storage::SqliteStore;
use crate::transport::{
    AcpAgentConfig, AcpTransport, AgentConnection, AgentTransport, PermissionRequestId,
    PermissionResponse, SessionRef, TransportError, TransportEvent, TransportFailureKind,
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

pub struct AgentDirectMessageRuntime<T: AgentTransport> {
    transport: Option<T>,
    storage: Option<StorageWorker>,
    manager: Option<SessionManager<T>>,
    active: Option<ActiveDirectMessage>,
    agent: Option<Agent>,
    stopped: bool,
}

impl<T: AgentTransport> AgentDirectMessageRuntime<T> {
    pub fn new(transport: T, storage: StorageWorker) -> Self {
        Self {
            transport: Some(transport),
            storage: Some(storage),
            manager: None,
            active: None,
            agent: None,
            stopped: false,
        }
    }

    fn with_agent(transport: T, storage: StorageWorker, agent: Agent) -> Self {
        let mut runtime = Self::new(transport, storage);
        runtime.agent = Some(agent);
        runtime
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
) -> Result<AgentDirectMessageRuntime<AcpTransport>, DirectMessageBootstrapError> {
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
    Ok(AgentDirectMessageRuntime::with_agent(
        AcpTransport::new(config),
        storage,
        agent,
    ))
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

impl<T: AgentTransport> DirectMessageRuntime for AgentDirectMessageRuntime<T> {
    async fn open(
        &mut self,
        user_id: String,
        agent_name: String,
        opened_at: String,
    ) -> Result<OpenedDirectMessage, DirectMessageError> {
        if self.active.is_some() || self.manager.is_some() {
            return Err(DirectMessageError::AlreadyOpen);
        }
        let storage = self.storage.as_ref().ok_or(DirectMessageError::NotOpen)?;
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
                .ok_or(DirectMessageError::AgentNotFound(agent_name))?,
        };
        let conversation = storage
            .get_or_create_dm(user_id, agent.id, opened_at.clone())
            .await
            .map_err(runtime_error)?;
        let messages = storage
            .list_messages(conversation.id)
            .await
            .map_err(runtime_error)?;
        let binding = storage
            .get_latest_session_binding(conversation.id, agent.id)
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

        let transport = self.transport.take().ok_or(DirectMessageError::NotOpen)?;
        let storage = self.storage.take().ok_or(DirectMessageError::NotOpen)?;
        let project_root = PathBuf::from(&agent.project_root);
        let mut manager = SessionManager::connect(
            transport,
            storage,
            AgentConnection {
                agent_id: agent.id,
                project_root: project_root.clone(),
            },
        )
        .await
        .map_err(runtime_error)?;

        let session = match binding {
            Some(binding) => {
                manager
                    .resume_session(&binding, project_root, opened_at.clone())
                    .await
            }
            None => {
                manager
                    .create_session(
                        SessionBinding {
                            id: Default::default(),
                            conversation_id: conversation.id,
                            agent_id: agent.id,
                            transport_type: agent.transport_type.clone(),
                            remote_session_id: None,
                            generation: 1,
                            status: SessionBindingStatus::Active,
                            created_at: opened_at.clone(),
                            last_used_at: opened_at.clone(),
                        },
                        project_root,
                    )
                    .await
            }
        };
        let session = match session {
            Ok(session) => session,
            Err(error) => {
                let error = runtime_error(error);
                let _ = manager.shutdown(opened_at).await;
                self.stopped = true;
                return Err(error);
            }
        };

        self.active = Some(ActiveDirectMessage {
            agent_id: agent.id,
            session,
            permissions: HashMap::new(),
        });
        self.manager = Some(manager);
        Ok(OpenedDirectMessage {
            conversation_id: conversation.id,
            agent_id: agent.id,
            agent_name: agent.name,
            messages,
        })
    }

    async fn persist_message(&mut self, message: Message) -> Result<(), DirectMessageError> {
        self.manager
            .as_ref()
            .ok_or(DirectMessageError::NotOpen)?
            .storage()
            .insert_message(message)
            .await
            .map_err(runtime_error)
    }

    async fn send_exact(&mut self, content: String) -> Result<(), DirectMessageError> {
        let session = self.require_active()?.session.clone();
        self.manager
            .as_mut()
            .ok_or(DirectMessageError::NotOpen)?
            .send_message(session, content)
            .await
            .map_err(runtime_error)
    }

    async fn next_runtime_event(
        &mut self,
        observed_at: String,
    ) -> Result<Option<DirectMessageRuntimeEvent>, DirectMessageError> {
        loop {
            let event = self
                .manager
                .as_mut()
                .ok_or(DirectMessageError::NotOpen)?
                .next_event(&observed_at)
                .await
                .map_err(runtime_error)?;
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
                    self.manager
                        .as_mut()
                        .expect("manager checked")
                        .track_permission(request.clone());
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
                    self.manager
                        .as_ref()
                        .expect("manager checked")
                        .mark_session_lost(&session, observed_at.clone())
                        .await
                        .map_err(runtime_error)?;
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
        let session = self
            .active
            .as_mut()
            .ok_or(DirectMessageError::NotOpen)?
            .permissions
            .remove(request_id.as_str())
            .ok_or_else(|| DirectMessageError::PermissionRequestNotFound(request_id.to_string()))?;
        self.manager
            .as_mut()
            .ok_or(DirectMessageError::NotOpen)?
            .respond_permission(
                PermissionResponse {
                    session,
                    request_id: PermissionRequestId::from(request_id.to_string()),
                    outcome,
                },
                decided_at,
            )
            .await
            .map_err(runtime_error)
    }

    async fn cancel_turn(&mut self, cancelled_at: String) -> Result<(), DirectMessageError> {
        let session = self.require_active()?.session.clone();
        self.manager
            .as_mut()
            .ok_or(DirectMessageError::NotOpen)?
            .cancel_turn(session, cancelled_at)
            .await
            .map_err(runtime_error)
    }

    async fn shutdown(&mut self, stopped_at: String) -> Result<(), DirectMessageError> {
        if self.stopped {
            return Ok(());
        }
        self.stopped = true;
        self.active = None;
        if let Some(mut manager) = self.manager.take() {
            return manager.shutdown(stopped_at).await.map_err(runtime_error);
        }

        let transport_result = if let Some(mut transport) = self.transport.take() {
            transport.shutdown().await.map_err(transport_error)
        } else {
            Ok(())
        };
        let storage_result = if let Some(mut storage) = self.storage.take() {
            storage.shutdown().await.map_err(runtime_error)
        } else {
            Ok(())
        };
        transport_result?;
        storage_result
    }
}

fn runtime_error(error: RuntimeError) -> DirectMessageError {
    match error {
        RuntimeError::Transport(error) => transport_error(error),
        RuntimeError::MissingRemoteSession => DirectMessageError::SessionLost,
        RuntimeError::PermissionRequestNotFound(id) => {
            DirectMessageError::PermissionRequestNotFound(id)
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
