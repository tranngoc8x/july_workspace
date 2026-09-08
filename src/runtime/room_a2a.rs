//! Storage-backed internal A2A Message boundary. ACP delivery belongs to the caller.

use super::{RuntimeError, StorageHandle, StorageWorker};
use crate::domain::{AgentId, RoomMessage, RoomMessageId};
use crate::transport::a2a::{A2aMessageError, encode_room_message, validate_room_message};
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
        Ok(encode_room_message(&canonical, target)?)
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
        validate_room_message(value, &canonical, target)?;
        Ok(RoomA2aRecipient {
            target,
            message: canonical,
        })
    }
}
