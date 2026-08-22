use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DomainError {
    #[error("{0} must not be empty")]
    EmptyField(&'static str),
    #[error("DM conversation must not have a room")]
    DmHasRoom,
    #[error("thread conversation requires a room")]
    ThreadMissingRoom,
    #[error("thread conversation requires a non-empty title")]
    ThreadMissingTitle,
    #[error("work dependency cannot reference itself")]
    SelfDependency,
    #[error("session generation must be greater than zero")]
    InvalidSessionGeneration,
    #[error("membership generation must be greater than zero")]
    InvalidMembershipGeneration,
    #[error("capsule delivery timestamp requires a capsule")]
    CapsuleDeliveryWithoutCapsule,
    #[error("delivered_at must be set exactly when delivery status is delivered")]
    DeliveryTimestampStatusMismatch,
    #[error("completed_at must be set exactly when work status is terminal")]
    WorkCompletionTimestampMismatch,
    #[error("permission option was not advertised: {0}")]
    PermissionOptionNotAdvertised(String),
    #[error("invalid {kind}: {value}")]
    InvalidEnum { kind: &'static str, value: String },
}
