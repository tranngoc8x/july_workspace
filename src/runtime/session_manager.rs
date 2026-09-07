use super::{RuntimeError, StorageHandle};
use crate::application::BuildRecoveryCapsule;
use crate::domain::{
    AgentId, PermissionDecision, RoomMessageId, RoomSessionBinding, SessionBinding,
    SessionBindingId, SessionBindingStatus,
};
use crate::transport::{
    AgentConnection, AgentTransport, CreateSession, PermissionRequest, PermissionRequestId,
    PermissionResponse, ResumeSession, SendMessage, SessionRef, TransportEvent, TransportEvents,
};
use std::collections::HashMap;
use std::path::PathBuf;

pub(crate) struct SessionManager<T: AgentTransport> {
    transport: T,
    storage: StorageHandle,
    agent_id: AgentId,
    events: TransportEvents,
    pending_permissions: HashMap<(SessionBindingId, PermissionRequestId), PermissionRequest>,
    owned_bindings: HashMap<SessionBindingId, SessionRef>,
}

impl<T: AgentTransport> SessionManager<T> {
    pub(crate) async fn connect(
        mut transport: T,
        storage: StorageHandle,
        agent: AgentConnection,
    ) -> Result<Self, RuntimeError> {
        if let Err(error) = transport.connect(&agent).await {
            let _ = transport.shutdown().await;
            return Err(error.into());
        }
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
            owned_bindings: HashMap::new(),
        })
    }

    /// Room activation never enters conversation recovery or imports private history.
    pub(crate) async fn activate_room_message(
        &mut self,
        message_id: RoomMessageId,
        at: String,
    ) -> Result<Option<SessionRef>, RuntimeError> {
        let Some(claim) = self
            .storage
            .claim_room_activation(message_id, self.agent_id, at.clone())
            .await?
        else {
            return Ok(None);
        };
        let crate::storage::RoomActivationClaim {
            agent,
            message,
            binding,
            context,
            truncated,
        } = claim;
        if self.owned_bindings.contains_key(&binding.id) {
            self.storage
                .set_room_activation_status(message_id, self.agent_id, "failed", at)
                .await?;
            return Err(RuntimeError::SessionBindingAlreadyAttached(binding.id));
        }
        let opened = self
            .open_room_session(&binding, agent.project_root.into(), at.clone())
            .await;
        let session = match opened {
            Ok(session) => session,
            Err(error) => {
                self.update_binding_status(binding.id, SessionBindingStatus::Lost, at.clone())
                    .await?;
                self.storage
                    .set_room_activation_status(message_id, self.agent_id, "failed", at)
                    .await?;
                return Err(error);
            }
        };
        let mut content = format!("Room: {}\n", message.room_id);
        if truncated {
            content.push_str(
                "Older unseen Room messages omitted: context limited to 50 preceding messages.\n",
            );
        }
        content.push_str("Shared Room context:\n");
        for previous in &context {
            append_room_context_message(&mut content, previous);
        }
        content.push_str("Current message:\n");
        append_room_context_message(&mut content, &message);
        let sent = async {
            self.storage
                .validate_room_activation(message_id, self.agent_id)
                .await?;
            self.send_message(session.clone(), content).await?;
            self.storage
                .set_room_activation_status(message_id, self.agent_id, "sent", at.clone())
                .await
        }
        .await;
        if let Err(error) = sent {
            // A failed send may already have been accepted. Never retry it automatically.
            let _ = self.cancel_turn(session.clone(), at.clone()).await;
            self.update_binding_status(binding.id, SessionBindingStatus::Lost, at.clone())
                .await?;
            self.storage
                .set_room_activation_status(message_id, self.agent_id, "failed", at.clone())
                .await?;
            self.detach_session(&session, at).await?;
            return Err(error);
        }
        Ok(Some(session))
    }

    async fn open_room_session(
        &mut self,
        binding: &RoomSessionBinding,
        project_root: PathBuf,
        at: String,
    ) -> Result<SessionRef, RuntimeError> {
        if binding.agent_id != self.agent_id {
            return Err(RuntimeError::BindingAgentMismatch);
        }
        let session = match &binding.remote_session_id {
            Some(remote) => {
                self.transport
                    .resume_session(ResumeSession {
                        session: SessionRef {
                            binding_id: binding.id,
                            remote_session_id: remote.clone(),
                        },
                        project_root,
                    })
                    .await?
                    .session
            }
            None => {
                self.transport
                    .create_session(CreateSession {
                        binding_id: binding.id,
                        project_root,
                    })
                    .await?
                    .session
            }
        };
        if let Err(error) = self
            .storage
            .attach_room_remote_session(binding.id, session.remote_session_id.clone(), at)
            .await
        {
            let _ = self.transport.close_session(session).await;
            return Err(error);
        }
        self.owned_bindings
            .insert(session.binding_id, session.clone());
        Ok(session)
    }

    pub(crate) async fn create_session(
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
        self.owned_bindings
            .insert(created.session.binding_id, created.session.clone());
        Ok(created.session)
    }

    pub(crate) async fn open_session(
        &mut self,
        mut binding: SessionBinding,
        project_root: PathBuf,
        opened_at: String,
        recover_missing: bool,
    ) -> Result<SessionRef, RuntimeError> {
        self.require_own_binding(&binding)?;
        if !recover_missing {
            return if binding.remote_session_id.is_some() {
                self.resume_session(&binding, project_root, opened_at).await
            } else {
                self.create_session(binding, project_root).await
            };
        }
        let latest = self
            .storage
            .get_latest_session_binding(binding.conversation_id, binding.agent_id)
            .await?;
        let Some(latest) = latest else {
            return self.create_session(binding, project_root).await;
        };
        if latest.id != binding.id {
            return Err(RuntimeError::SessionBindingNotFound(binding.id));
        }
        binding = latest;

        loop {
            if binding.status == SessionBindingStatus::Closed {
                return Err(RuntimeError::SessionUnavailable(binding.status));
            }
            if binding.status == SessionBindingStatus::Lost {
                binding = self.begin_replacement(&binding, opened_at.clone()).await?;
                continue;
            }

            let recovery = self.storage.get_session_recovery(binding.id).await?;
            let (result, replace_on_lost) = if recovery.is_some() {
                if binding.remote_session_id.is_some() {
                    (
                        self.resume_session(&binding, project_root.clone(), opened_at.clone())
                            .await,
                        true,
                    )
                } else {
                    (
                        self.create_replacement_session(
                            &binding,
                            project_root.clone(),
                            opened_at.clone(),
                        )
                        .await,
                        false,
                    )
                }
            } else if binding.remote_session_id.is_none() {
                binding = self.begin_replacement(&binding, opened_at.clone()).await?;
                continue;
            } else {
                (
                    self.resume_session(&binding, project_root.clone(), opened_at.clone())
                        .await,
                    true,
                )
            };

            match result {
                Ok(session) => return Ok(session),
                Err(RuntimeError::Transport(crate::transport::TransportError::SessionLost(_)))
                    if replace_on_lost =>
                {
                    binding = self.begin_replacement(&binding, opened_at.clone()).await?;
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub(crate) async fn resume_session(
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
                self.owned_bindings
                    .insert(resumed.session.binding_id, resumed.session.clone());
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

    async fn begin_replacement(
        &mut self,
        source: &SessionBinding,
        replaced_at: String,
    ) -> Result<SessionBinding, RuntimeError> {
        let capsule = self
            .storage
            .build_recovery_capsule(BuildRecoveryCapsule {
                conversation_id: source.conversation_id,
                agent_id: source.agent_id,
            })
            .await?;
        let (replacement, _) = self
            .storage
            .begin_session_replacement(
                source.id,
                SessionBindingId::new(),
                capsule.content,
                replaced_at,
            )
            .await?;
        Ok(replacement)
    }

    async fn create_replacement_session(
        &mut self,
        binding: &SessionBinding,
        project_root: PathBuf,
        attached_at: String,
    ) -> Result<SessionRef, RuntimeError> {
        self.require_own_binding(binding)?;
        let created = self
            .transport
            .create_session(CreateSession {
                binding_id: binding.id,
                project_root,
            })
            .await?;
        if let Err(error) = self
            .storage
            .attach_replacement_remote_session(
                binding.id,
                created.session.remote_session_id.clone(),
                attached_at,
            )
            .await
        {
            let _ = self.transport.close_session(created.session.clone()).await;
            return Err(error);
        }
        self.owned_bindings
            .insert(created.session.binding_id, created.session.clone());
        Ok(created.session)
    }

    pub(crate) async fn send_message(
        &mut self,
        session: SessionRef,
        content: String,
    ) -> Result<(), RuntimeError> {
        self.transport
            .send_message(SendMessage { session, content })
            .await?;
        Ok(())
    }

    pub(crate) async fn deliver_recovery_capsule(
        &mut self,
        session: &SessionRef,
        delivered_at: String,
    ) -> Result<(), RuntimeError> {
        let Some(recovery) = self
            .storage
            .get_session_recovery(session.binding_id)
            .await?
        else {
            return Ok(());
        };
        if recovery.capsule_delivered_at.is_some() {
            return Ok(());
        }
        self.send_message(session.clone(), recovery.capsule).await?;
        self.storage
            .mark_session_recovery_capsule_delivered(session.binding_id, delivered_at)
            .await?;
        Ok(())
    }

    pub(crate) async fn cancel_turn(
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

    pub(crate) async fn detach_session(
        &mut self,
        session: &SessionRef,
        detached_at: String,
    ) -> Result<(), RuntimeError> {
        if self.owned_bindings.get(&session.binding_id) != Some(session) {
            return Err(RuntimeError::SessionBindingNotFound(session.binding_id));
        }
        self.audit_cancelled_permissions(Some(session), &detached_at)
            .await?;
        let disconnected = self
            .storage
            .mark_binding_disconnected(session.binding_id, detached_at)
            .await?;
        if disconnected {
            self.owned_bindings.remove(&session.binding_id);
            Ok(())
        } else {
            Err(RuntimeError::SessionBindingNotFound(session.binding_id))
        }
    }

    pub(crate) async fn abort_session_recovery(
        &mut self,
        session: &SessionRef,
        detached_at: String,
    ) -> Result<(), RuntimeError> {
        if self.owned_bindings.get(&session.binding_id) != Some(session) {
            return Err(RuntimeError::SessionBindingNotFound(session.binding_id));
        }
        self.owned_bindings.remove(&session.binding_id);
        let audit = self
            .audit_cancelled_permissions(Some(session), &detached_at)
            .await;
        let disconnected = self
            .storage
            .mark_binding_disconnected(session.binding_id, detached_at)
            .await;
        audit?;
        if disconnected? {
            Ok(())
        } else {
            Err(RuntimeError::SessionBindingNotFound(session.binding_id))
        }
    }

    pub(crate) async fn next_event(
        &mut self,
        observed_at: &str,
    ) -> Result<Option<TransportEvent>, RuntimeError> {
        let Some(event) = self.events.recv().await else {
            return Ok(None);
        };
        match &event {
            TransportEvent::TransportDisconnected { agent_id, .. }
                if *agent_id == self.agent_id =>
            {
                tracing::warn!(agent_id = %agent_id, transport = "acp", "marking owned session bindings disconnected");
                let audit = self.audit_cancelled_permissions(None, observed_at).await;
                let disconnected =
                    disconnect_owned_bindings(&self.storage, &self.owned_bindings, observed_at)
                        .await;
                audit?;
                disconnected?;
            }
            _ => {}
        }
        Ok(Some(event))
    }

    pub(super) fn track_permission(&mut self, request: PermissionRequest) {
        self.pending_permissions.insert(
            (request.session.binding_id, request.request_id.clone()),
            request,
        );
    }

    pub(super) fn owns_session(&self, session: &SessionRef) -> bool {
        self.owned_bindings.get(&session.binding_id) == Some(session)
    }

    pub(super) async fn mark_session_lost(
        &mut self,
        session: &SessionRef,
        observed_at: String,
    ) -> Result<(), RuntimeError> {
        self.update_binding_status(session.binding_id, SessionBindingStatus::Lost, observed_at)
            .await
    }

    pub(crate) async fn respond_permission(
        &mut self,
        mut response: PermissionResponse,
        decided_at: String,
    ) -> Result<(), RuntimeError> {
        let request = self
            .pending_permissions
            .get(&(response.session.binding_id, response.request_id.clone()))
            .cloned()
            .ok_or_else(|| {
                RuntimeError::PermissionRequestNotFound(response.request_id.to_string())
            })?;
        if request.session != response.session {
            return Err(RuntimeError::PermissionRequestNotFound(
                response.request_id.to_string(),
            ));
        }
        let invalid_option = match &response.outcome {
            crate::domain::PermissionOutcome::Selected(option_id)
                if !request.options.iter().any(|option| option.id == *option_id) =>
            {
                Some(option_id.clone())
            }
            _ => None,
        };
        if invalid_option.is_some() {
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
            let key = (request.session.binding_id, response.request_id.clone());
            let _ = self
                .transport
                .respond_permission(PermissionResponse {
                    session: request.session,
                    request_id: response.request_id.clone(),
                    outcome: crate::domain::PermissionOutcome::Cancelled,
                })
                .await;
            self.pending_permissions.remove(&key);
            return Err(error);
        }
        self.pending_permissions
            .remove(&(request.session.binding_id, request.request_id.clone()));
        self.transport.respond_permission(response).await?;
        if let Some(option_id) = invalid_option {
            return Err(
                crate::transport::TransportError::PermissionOptionNotAdvertised(option_id).into(),
            );
        }
        Ok(())
    }

    pub(crate) async fn shutdown(&mut self, stopped_at: String) -> Result<(), RuntimeError> {
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
                            &self.owned_bindings,
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
            if let Err(error) = apply_shutdown_event(
                &self.storage,
                self.agent_id,
                &self.owned_bindings,
                event,
                &stopped_at,
            )
            .await
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        let disconnected =
            disconnect_owned_bindings(&self.storage, &self.owned_bindings, &stopped_at).await;
        if let Some(error) = first_error {
            return Err(error);
        }
        transport?;
        disconnected
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
            match self.storage.insert_permission_decision(decision).await {
                Ok(()) => {
                    self.pending_permissions
                        .remove(&(request.session.binding_id, request.request_id.clone()));
                }
                Err(error) if first_error.is_none() => first_error = Some(error),
                Err(_) => {}
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    async fn update_binding_status(
        &mut self,
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
    storage: &StorageHandle,
    agent_id: AgentId,
    owned_bindings: &HashMap<SessionBindingId, SessionRef>,
    event: TransportEvent,
    observed_at: &str,
) -> Result<(), RuntimeError> {
    match event {
        TransportEvent::PermissionRequested(request)
            if owned_bindings.get(&request.session.binding_id) == Some(&request.session) =>
        {
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
        TransportEvent::SessionLost { session }
            if owned_bindings.get(&session.binding_id) == Some(&session) =>
        {
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
        } if disconnected_agent == agent_id => {
            disconnect_owned_bindings(storage, owned_bindings, observed_at).await
        }
        _ => Ok(()),
    }
}

async fn disconnect_owned_bindings(
    storage: &StorageHandle,
    owned_bindings: &HashMap<SessionBindingId, SessionRef>,
    observed_at: &str,
) -> Result<(), RuntimeError> {
    let mut first_error = None;
    for binding_id in owned_bindings.keys() {
        if let Err(error) = storage
            .mark_binding_disconnected(*binding_id, observed_at.into())
            .await
            && first_error.is_none()
        {
            first_error = Some(error);
        }
    }
    first_error.map_or(Ok(()), Err)
}

fn append_room_context_message(content: &mut String, message: &crate::domain::RoomMessage) {
    use std::fmt::Write;
    writeln!(
        content,
        "Message: {}\nSender: {} ({})\nReply-to: {}\n{}\n",
        message.id,
        message.sender_id,
        message.sender_type,
        message
            .reply_to
            .map(|id| id.to_string())
            .unwrap_or_else(|| "none".into()),
        message.body
    )
    .expect("writing to String cannot fail");
}
