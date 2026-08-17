use super::{RuntimeError, SessionManager, StorageWorker};
use crate::application::{CollaborationError, OpenThreadForAgent, OpenedThread, ThreadRuntime};
use crate::domain::{SessionBinding, SessionBindingStatus};
use crate::transport::{AgentConnection, AgentTransport, TransportError};
use std::path::PathBuf;

pub struct AgentThreadRuntime<T: AgentTransport> {
    transport: Option<T>,
    storage: Option<StorageWorker>,
    manager: Option<SessionManager<T>>,
    opened: bool,
    stopped: bool,
}

impl<T: AgentTransport> AgentThreadRuntime<T> {
    pub fn new(transport: T, storage: StorageWorker) -> Self {
        Self {
            transport: Some(transport),
            storage: Some(storage),
            manager: None,
            opened: false,
            stopped: false,
        }
    }
}

impl<T: AgentTransport> ThreadRuntime for AgentThreadRuntime<T> {
    async fn open_thread_for_agent(
        &mut self,
        command: OpenThreadForAgent,
    ) -> Result<OpenedThread, CollaborationError> {
        if self.opened || self.manager.is_some() {
            return Err(CollaborationError::ThreadAlreadyOpen);
        }
        let storage = self.storage.as_ref().ok_or_else(not_open)?;
        let (agent, thread, binding) = storage
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
        let transport = self.transport.take().ok_or_else(not_open)?;
        let storage = self.storage.take().ok_or_else(not_open)?;
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
                    .resume_session(&binding, project_root, command.opened_at.clone())
                    .await
            }
            None => {
                manager
                    .create_session(
                        SessionBinding {
                            id: Default::default(),
                            conversation_id: thread.id,
                            agent_id: agent.id,
                            transport_type: agent.transport_type,
                            remote_session_id: None,
                            generation: 1,
                            status: SessionBindingStatus::Active,
                            created_at: command.opened_at.clone(),
                            last_used_at: command.opened_at.clone(),
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
                let _ = manager.shutdown(command.opened_at).await;
                self.stopped = true;
                return Err(error);
            }
        };
        self.opened = true;
        self.manager = Some(manager);
        Ok(OpenedThread {
            thread_id: thread.id,
            room_id: thread.room_id.expect("admitted Thread has a Room"),
            agent_id: agent.id,
            session_binding_id: session.binding_id,
        })
    }

    async fn shutdown(&mut self, stopped_at: String) -> Result<(), CollaborationError> {
        if self.stopped {
            return Ok(());
        }
        self.stopped = true;
        self.opened = false;
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

fn not_open() -> CollaborationError {
    CollaborationError::Runtime("Thread runtime is unavailable".into())
}

fn runtime_error(error: RuntimeError) -> CollaborationError {
    match error {
        RuntimeError::Transport(error) => transport_error(error),
        RuntimeError::MissingRemoteSession => CollaborationError::SessionLost,
        other => CollaborationError::Runtime(other.to_string()),
    }
}

fn transport_error(error: TransportError) -> CollaborationError {
    match error {
        TransportError::SessionLost(_) => CollaborationError::SessionLost,
        other => CollaborationError::Runtime(other.to_string()),
    }
}
