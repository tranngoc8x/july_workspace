use super::{RuntimeError, StorageWorker};
use crate::domain::{AgentId, PermissionDecision, SessionBinding, SessionBindingStatus};
use crate::transport::{
    AgentConnection, AgentTransport, CreateSession, PermissionRequest, PermissionRequestId,
    PermissionResponse, ResumeSession, SendMessage, SessionRef, TransportEvent, TransportEvents,
};
use std::collections::HashMap;
use std::path::PathBuf;

pub struct SessionManager<T: AgentTransport> {
    transport: T,
    storage: StorageWorker,
    agent_id: AgentId,
    events: TransportEvents,
    pending_permissions: HashMap<PermissionRequestId, PermissionRequest>,
}

impl<T: AgentTransport> SessionManager<T> {
    pub(super) fn storage(&self) -> &StorageWorker {
        &self.storage
    }

    pub async fn connect(
        mut transport: T,
        storage: StorageWorker,
        agent: AgentConnection,
    ) -> Result<Self, RuntimeError> {
        transport.connect(&agent).await?;
        let events = match transport.subscribe() {
            Ok(events) => events,
            Err(error) => {
                let _ = transport.shutdown().await;
                return Err(error.into());
            }
        };
        Ok(Self {
            transport,
            storage,
            agent_id: agent.agent_id,
            events,
            pending_permissions: HashMap::new(),
        })
    }

    pub async fn create_session(
        &mut self,
        mut binding: SessionBinding,
        project_root: PathBuf,
    ) -> Result<SessionRef, RuntimeError> {
        self.require_own_binding(&binding)?;
        let created = self
            .transport
            .create_session(CreateSession {
                binding_id: binding.id,
                project_root,
            })
            .await?;
        binding.remote_session_id = Some(created.session.remote_session_id.clone());
        binding.status = SessionBindingStatus::Active;
        if let Err(error) = self.storage.insert_session_binding(binding).await {
            let _ = self.transport.close_session(created.session.clone()).await;
            return Err(error);
        }
        Ok(created.session)
    }

    pub async fn resume_session(
        &mut self,
        binding: &SessionBinding,
        project_root: PathBuf,
        resumed_at: String,
    ) -> Result<SessionRef, RuntimeError> {
        self.require_own_binding(binding)?;
        let current = self
            .storage
            .get_current_session_binding(binding.conversation_id, binding.agent_id)
            .await?;
        let current = current
            .filter(|current| current.id == binding.id)
            .ok_or(RuntimeError::SessionBindingNotFound(binding.id))?;
        let session = binding_session(&current)?;
        match self
            .transport
            .resume_session(ResumeSession {
                session: session.clone(),
                project_root,
            })
            .await
        {
            Ok(resumed) => {
                self.update_binding_status(binding.id, SessionBindingStatus::Active, resumed_at)
                    .await?;
                Ok(resumed.session)
            }
            Err(crate::transport::TransportError::SessionLost(_)) => {
                self.update_binding_status(binding.id, SessionBindingStatus::Lost, resumed_at)
                    .await?;
                Err(crate::transport::TransportError::SessionLost(session.remote_session_id).into())
            }
            Err(error) => Err(error.into()),
        }
    }

    pub async fn send_message(
        &mut self,
        session: SessionRef,
        content: String,
    ) -> Result<(), RuntimeError> {
        self.transport
            .send_message(SendMessage { session, content })
            .await?;
        Ok(())
    }

    pub async fn cancel_turn(
        &mut self,
        session: SessionRef,
        cancelled_at: String,
    ) -> Result<(), RuntimeError> {
        let audit = self
            .audit_cancelled_permissions(Some(&session), &cancelled_at)
            .await;
        let transport = self.transport.cancel_turn(session).await;
        audit?;
        transport?;
        Ok(())
    }

    pub async fn close_session(
        &mut self,
        session: SessionRef,
        closed_at: String,
    ) -> Result<(), RuntimeError> {
        self.transport.close_session(session.clone()).await?;
        self.update_binding_status(session.binding_id, SessionBindingStatus::Closed, closed_at)
            .await?;
        Ok(())
    }

    pub async fn next_event(
        &mut self,
        observed_at: &str,
    ) -> Result<Option<TransportEvent>, RuntimeError> {
        let Some(event) = self.events.recv().await else {
            return Ok(None);
        };
        match &event {
            TransportEvent::PermissionRequested(request) => {
                self.pending_permissions
                    .insert(request.request_id.clone(), request.clone());
            }
            TransportEvent::TransportDisconnected { agent_id, .. }
                if *agent_id == self.agent_id =>
            {
                tracing::warn!(agent_id = %agent_id, transport = "acp", "marking current session bindings disconnected");
                let audit = self.audit_cancelled_permissions(None, observed_at).await;
                let disconnected = self
                    .storage
                    .mark_current_bindings_disconnected(self.agent_id, observed_at.into())
                    .await;
                audit?;
                disconnected?;
            }
            TransportEvent::SessionLost { session } => {
                self.update_binding_status(
                    session.binding_id,
                    SessionBindingStatus::Lost,
                    observed_at.into(),
                )
                .await?;
            }
            _ => {}
        }
        Ok(Some(event))
    }

    pub async fn respond_permission(
        &mut self,
        mut response: PermissionResponse,
        decided_at: String,
    ) -> Result<(), RuntimeError> {
        let request = self
            .pending_permissions
            .get(&response.request_id)
            .cloned()
            .ok_or_else(|| {
                RuntimeError::PermissionRequestNotFound(response.request_id.to_string())
            })?;
        let session_mismatch = request.session != response.session;
        let invalid_option = match &response.outcome {
            crate::domain::PermissionOutcome::Selected(option_id)
                if !request.options.iter().any(|option| option.id == *option_id) =>
            {
                Some(option_id.clone())
            }
            _ => None,
        };
        if session_mismatch || invalid_option.is_some() {
            response.session = request.session.clone();
            response.outcome = crate::domain::PermissionOutcome::Cancelled;
        }
        let decision = PermissionDecision {
            id: ulid::Ulid::generate().to_string(),
            session_binding_id: request.session.binding_id,
            correlation_id: response.request_id.to_string(),
            options: request.options,
            outcome: response.outcome.clone(),
            decided_at,
        };
        if let Err(error) = self.storage.insert_permission_decision(decision).await {
            let _ = self
                .transport
                .respond_permission(PermissionResponse {
                    session: request.session,
                    request_id: response.request_id.clone(),
                    outcome: crate::domain::PermissionOutcome::Cancelled,
                })
                .await;
            self.pending_permissions.remove(&response.request_id);
            return Err(error);
        }
        self.pending_permissions.remove(&request.request_id);
        self.transport.respond_permission(response).await?;
        if session_mismatch {
            return Err(RuntimeError::PermissionRequestNotFound(
                request.request_id.to_string(),
            ));
        }
        if let Some(option_id) = invalid_option {
            return Err(
                crate::transport::TransportError::PermissionOptionNotAdvertised(option_id).into(),
            );
        }
        Ok(())
    }

    pub async fn shutdown(&mut self, stopped_at: String) -> Result<(), RuntimeError> {
        let mut first_error = self
            .audit_cancelled_permissions(None, &stopped_at)
            .await
            .err();
        let transport = {
            let shutdown = self.transport.shutdown();
            tokio::pin!(shutdown);
            loop {
                tokio::select! {
                    result = &mut shutdown => break result.map_err(RuntimeError::from),
                    event = self.events.recv() => {
                        let Some(event) = event else {
                            break shutdown.await.map_err(RuntimeError::from);
                        };
                        if let Err(error) = apply_shutdown_event(
                            &self.storage,
                            self.agent_id,
                            event,
                            &stopped_at,
                        ).await && first_error.is_none() {
                            first_error = Some(error);
                        }
                    }
                }
            }
        };
        while let Some(event) = self.events.try_recv() {
            if let Err(error) =
                apply_shutdown_event(&self.storage, self.agent_id, event, &stopped_at).await
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        let disconnected = self
            .storage
            .mark_current_bindings_disconnected(self.agent_id, stopped_at)
            .await
            .map(|_| ());
        let storage = self.storage.shutdown().await;
        if let Some(error) = first_error {
            return Err(error);
        }
        transport?;
        disconnected?;
        storage
    }

    async fn audit_cancelled_permissions(
        &mut self,
        session: Option<&SessionRef>,
        decided_at: &str,
    ) -> Result<(), RuntimeError> {
        let requests = self
            .pending_permissions
            .values()
            .filter(|request| session.is_none_or(|session| request.session == *session))
            .cloned()
            .collect::<Vec<_>>();
        let mut first_error = None;
        for request in requests {
            let decision = PermissionDecision {
                id: ulid::Ulid::generate().to_string(),
                session_binding_id: request.session.binding_id,
                correlation_id: request.request_id.to_string(),
                options: request.options,
                outcome: crate::domain::PermissionOutcome::Cancelled,
                decided_at: decided_at.into(),
            };
            if let Err(error) = self.storage.insert_permission_decision(decision).await
                && first_error.is_none()
            {
                first_error = Some(error);
            }
            self.pending_permissions.remove(&request.request_id);
        }
        first_error.map_or(Ok(()), Err)
    }

    async fn update_binding_status(
        &self,
        id: crate::domain::SessionBindingId,
        status: SessionBindingStatus,
        changed_at: String,
    ) -> Result<(), RuntimeError> {
        if self
            .storage
            .update_session_binding_status(id, status, changed_at)
            .await?
        {
            Ok(())
        } else {
            Err(RuntimeError::SessionBindingNotFound(id))
        }
    }

    fn require_own_binding(&self, binding: &SessionBinding) -> Result<(), RuntimeError> {
        if binding.agent_id == self.agent_id {
            Ok(())
        } else {
            Err(RuntimeError::BindingAgentMismatch)
        }
    }
}

fn binding_session(binding: &SessionBinding) -> Result<SessionRef, RuntimeError> {
    Ok(SessionRef {
        binding_id: binding.id,
        remote_session_id: binding
            .remote_session_id
            .clone()
            .ok_or(RuntimeError::MissingRemoteSession)?,
    })
}

async fn apply_shutdown_event(
    storage: &StorageWorker,
    agent_id: AgentId,
    event: TransportEvent,
    observed_at: &str,
) -> Result<(), RuntimeError> {
    match event {
        TransportEvent::PermissionRequested(request) => {
            storage
                .insert_permission_decision(PermissionDecision {
                    id: ulid::Ulid::generate().to_string(),
                    session_binding_id: request.session.binding_id,
                    correlation_id: request.request_id.to_string(),
                    options: request.options,
                    outcome: crate::domain::PermissionOutcome::Cancelled,
                    decided_at: observed_at.into(),
                })
                .await
        }
        TransportEvent::SessionLost { session } => {
            if storage
                .update_session_binding_status(
                    session.binding_id,
                    SessionBindingStatus::Lost,
                    observed_at.into(),
                )
                .await?
            {
                Ok(())
            } else {
                Err(RuntimeError::SessionBindingNotFound(session.binding_id))
            }
        }
        TransportEvent::TransportDisconnected {
            agent_id: disconnected_agent,
            ..
        } if disconnected_agent == agent_id => storage
            .mark_current_bindings_disconnected(agent_id, observed_at.into())
            .await
            .map(|_| ()),
        _ => Ok(()),
    }
}
