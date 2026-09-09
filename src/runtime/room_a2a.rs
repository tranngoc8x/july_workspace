//! Internal A2A Message validation and delivery through the shared runtime owner.

use super::{RoomActivation, RuntimeError, StorageHandle, StorageWorker, WorkspaceRuntime};
use crate::domain::{AgentId, RoomMessage, RoomMessageId};
use crate::transport::AgentTransport;
use crate::transport::a2a::{A2aMessageError, encode_room_task, encode_room_work_message};
use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum RoomA2aError {
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    #[error(transparent)]
    Message(#[from] A2aMessageError),
}

/// A canonical recipient snapshot, not a delivery acknowledgement or lasting
/// authorization. Delivery must revalidate membership when claiming activation.
#[derive(Clone, Debug, PartialEq)]
pub struct RoomA2aRecipient {
    pub target: AgentId,
    pub message: RoomMessage,
}

impl StorageWorker {
    /// Prepare the internal A2A 0.3 Message projection of a persisted shared message.
    pub async fn prepare_room_a2a_message(
        &self,
        message: RoomMessageId,
        target: AgentId,
    ) -> Result<Value, RoomA2aError> {
        self.handle()
            .prepare_room_a2a_message(message, target)
            .await
    }

    /// Read current canonical Work/Result as an internal A2A Task snapshot.
    pub async fn prepare_room_a2a_task(
        &self,
        message: RoomMessageId,
        target: AgentId,
    ) -> Result<Option<Value>, RoomA2aError> {
        let handle = self.handle();
        let canonical = handle.load_room_recipient_message(message, target).await?;
        let shared = handle.get_room_message_work(message).await?;
        shared
            .as_ref()
            .map(|work| encode_room_task(&canonical, target, work))
            .transpose()
            .map_err(Into::into)
    }

    /// Validate an internal message for a recipient supplied by trusted July code.
    /// This neither persists wire data nor claims or activates a runtime turn.
    pub async fn receive_room_a2a_message(
        &self,
        target: AgentId,
        value: &Value,
    ) -> Result<RoomA2aRecipient, RoomA2aError> {
        self.handle().receive_room_a2a_message(target, value).await
    }
}

impl StorageHandle {
    pub(crate) async fn prepare_room_a2a_message(
        &self,
        message: RoomMessageId,
        target: AgentId,
    ) -> Result<Value, RoomA2aError> {
        let canonical = self.load_room_recipient_message(message, target).await?;
        let shared = self.get_room_message_work(message).await?;
        Ok(encode_room_work_message(
            &canonical,
            target,
            shared.as_ref(),
        )?)
    }

    pub(crate) async fn receive_room_a2a_message(
        &self,
        target: AgentId,
        value: &Value,
    ) -> Result<RoomA2aRecipient, RoomA2aError> {
        let message = value["metadata"]["july.room_message_id"]
            .as_str()
            .ok_or(A2aMessageError)?
            .parse()
            .map_err(|_| A2aMessageError)?;
        let canonical = self.load_room_recipient_message(message, target).await?;
        let shared = self.get_room_message_work(message).await?;
        if *value != encode_room_work_message(&canonical, target, shared.as_ref())? {
            return Err(A2aMessageError.into());
        }
        Ok(RoomA2aRecipient {
            target,
            message: canonical,
        })
    }
}

impl<T: AgentTransport + Send + 'static> WorkspaceRuntime<T> {
    /// Validate canonical A2A intent, then claim and deliver through the existing
    /// Room runtime. Activation rechecks current membership and prevents resend.
    pub async fn receive_room_a2a_message(
        &self,
        target: AgentId,
        value: &Value,
        at: String,
    ) -> Result<Option<RoomActivation>, RoomA2aError> {
        let recipient = self
            .storage()
            .receive_room_a2a_message(target, value)
            .await?;
        Ok(self
            .activate_room_message(recipient.message.id, recipient.target, at)
            .await?)
    }
}
