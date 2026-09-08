use super::{RuntimeError, RuntimeSession, StorageHandle, timestamp};
use crate::domain::{AgentId, PermissionOutcome, RoomMessageId, SessionBindingStatus};
use crate::transport::{
    PermissionRequest, PermissionRequestId, SessionRef, TransportEvent, TransportFailureKind,
};

/// Control events only. Private text, tool traces and usage never leave the owner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RoomRuntimeEvent {
    PermissionRequested(PermissionRequest),
    Completed,
    Cancelled,
    Failed(TransportFailureKind),
}

/// A single claimed Room activation. Drop cancels and quarantines an unfinished
/// session; explicit terminal consumption detaches it for a later fresh message.
pub struct RoomActivation {
    publication_alive: std::sync::Arc<std::sync::atomic::AtomicBool>,
    runtime: Option<RuntimeSession>,
    session: SessionRef,
    storage: StorageHandle,
    message_id: RoomMessageId,
    agent_id: AgentId,
    cancelled: bool,
    completion: Option<RoomCompletion>,
}

struct RoomCompletion {
    event: Result<RoomRuntimeEvent, RuntimeError>,
    cleanup: tokio::task::JoinHandle<Result<(), RuntimeError>>,
}

impl RoomActivation {
    pub(crate) fn new(
        runtime: RuntimeSession,
        storage: StorageHandle,
        message_id: RoomMessageId,
        agent_id: AgentId,
        publication_alive: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        Self {
            publication_alive,
            session: runtime.session().clone(),
            runtime: Some(runtime),
            storage,
            message_id,
            agent_id,
            cancelled: false,
            completion: None,
        }
    }

    pub fn session(&self) -> &SessionRef {
        &self.session
    }

    pub async fn cancel(&mut self, at: String) -> Result<(), RuntimeError> {
        self.publication_alive
            .store(false, std::sync::atomic::Ordering::SeqCst);
        if let Some(runtime) = &self.runtime
            && !self.cancelled
        {
            runtime.cancel_turn(at).await?;
            self.cancelled = true;
        }
        Ok(())
    }

    pub async fn respond_permission(
        &self,
        request: PermissionRequestId,
        outcome: PermissionOutcome,
        at: String,
    ) -> Result<(), RuntimeError> {
        self.runtime
            .as_ref()
            .ok_or(RuntimeError::SessionBindingNotFound(
                self.session.binding_id,
            ))?
            .respond_permission(request, outcome, at)
            .await
    }

    /// Cancellation-safe: once a terminal event is consumed, its cleanup keeps
    /// running and a subsequent poll receives that same terminal result.
    pub async fn next_event(
        &mut self,
        at: String,
    ) -> Result<Option<RoomRuntimeEvent>, RuntimeError> {
        loop {
            if let Some(completion) = self.completion.as_mut() {
                let cleaned = (&mut completion.cleanup).await;
                let completion = self
                    .completion
                    .take()
                    .expect("completion is retained while awaiting cleanup");
                cleaned.map_err(|_| RuntimeError::OwnerTaskPanicked)??;
                return completion.event.map(Some);
            }
            let Some(runtime) = self.runtime.as_mut() else {
                return Ok(None);
            };
            let terminal = match runtime.next_event().await {
                Some(TransportEvent::PermissionRequested(request)) => {
                    return Ok(Some(RoomRuntimeEvent::PermissionRequested(request)));
                }
                Some(TransportEvent::TurnCompleted { .. }) => Ok(if self.cancelled {
                    RoomRuntimeEvent::Cancelled
                } else {
                    RoomRuntimeEvent::Completed
                }),
                Some(TransportEvent::TurnFailed { failure, .. }) => {
                    Ok(RoomRuntimeEvent::Failed(failure))
                }
                Some(
                    TransportEvent::SessionLost { .. }
                    | TransportEvent::TransportDisconnected { .. },
                )
                | None => Err(RuntimeError::ChannelClosed),
                Some(_) => continue, // No private transport event becomes a Room event.
            };
            self.begin_finish(terminal, at.clone());
        }
    }

    fn begin_finish(&mut self, event: Result<RoomRuntimeEvent, RuntimeError>, at: String) {
        let mut runtime = self
            .runtime
            .take()
            .expect("terminal event has a live session");
        let storage = self.storage.clone();
        let message = self.message_id;
        let agent = self.agent_id;
        let binding = self.session.binding_id;
        let disconnected = event.is_err();
        let status = if matches!(event, Ok(RoomRuntimeEvent::Completed)) {
            "completed"
        } else {
            "failed"
        };
        // The task owns the session until detach, even if next_event or the
        // activation handle is dropped while SQLite or the owner is busy.
        let cleanup = tokio::spawn(async move {
            let quarantined = if disconnected {
                storage
                    .update_session_binding_status(binding, SessionBindingStatus::Lost, at.clone())
                    .await
                    .map(|_| ())
            } else {
                Ok(())
            };
            let recorded = storage
                .set_room_activation_status(message, agent, status, at.clone())
                .await;
            let detached = runtime.detach(at).await;
            quarantined.and(recorded).and(detached)
        });
        self.completion = Some(RoomCompletion { event, cleanup });
    }
}

impl Drop for RoomActivation {
    fn drop(&mut self) {
        self.publication_alive
            .store(false, std::sync::atomic::Ordering::SeqCst);
        let Some(mut runtime) = self.runtime.take() else {
            return;
        };
        let storage = self.storage.clone();
        let message = self.message_id;
        let agent = self.agent_id;
        let binding = self.session.binding_id;
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let at = timestamp();
                let _ = runtime.cancel_turn(at.clone()).await;
                let _ = storage
                    .update_session_binding_status(binding, SessionBindingStatus::Lost, at.clone())
                    .await;
                let _ = storage
                    .set_room_activation_status(message, agent, "failed", at.clone())
                    .await;
                let _ = runtime.detach(at).await;
            });
        }
    }
}
