use crate::domain::{ConversationId, DomainError, MessageId, RoomId};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Domain(#[from] DomainError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Id(#[from] ulid::DecodeError),
    #[error("integer value {value} for {field} is out of range")]
    IntegerOutOfRange { field: &'static str, value: i128 },
    #[error("invalid stored value for {0}")]
    InvalidStoredValue(&'static str),
    #[error("room member parent {found} does not match batch room {expected}")]
    RoomMemberParentMismatch { expected: RoomId, found: RoomId },
    #[error("conversation member parent {found} does not match batch conversation {expected}")]
    ConversationMemberParentMismatch {
        expected: ConversationId,
        found: ConversationId,
    },
    #[error("message {id} already exists with different content")]
    MessageConflict { id: MessageId },
    #[error("database schema version {found} is newer than supported version {supported}")]
    DatabaseTooNew { found: i64, supported: i64 },
}
