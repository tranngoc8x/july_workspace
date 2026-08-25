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
    #[error("work dependency result reference does not match its status")]
    DependencyResultStatusMismatch,
    #[error("session generation must be greater than zero")]
    InvalidSessionGeneration,
    #[error("session recovery source and replacement must differ")]
    SessionRecoverySelfReference,
    #[error("membership generation must be greater than zero")]
    InvalidMembershipGeneration,
    #[error("capsule delivery timestamp requires a capsule")]
    CapsuleDeliveryWithoutCapsule,
    #[error("delivered_at must be set exactly when delivery status is delivered")]
    DeliveryTimestampStatusMismatch,
    #[error("completed_at must be set exactly when work status is terminal")]
    WorkCompletionTimestampMismatch,
    #[error("work result cannot supersede itself")]
    ResultSupersedesItself,
    #[error("proposal cannot supersede itself")]
    ProposalSupersedesItself,
    #[error("a {0} response must carry a reason, and evidence when it disagrees")]
    ProposalResponseNotActionable(crate::domain::ProposalResponseType),
    #[error("decision cannot supersede itself")]
    DecisionSupersedesItself,
    #[error("a decision states its outcome exactly when it is decided")]
    DecisionOutcomeStatusMismatch,
    #[error("handoff source and target agent must differ")]
    HandoffSelfTarget,
    #[error("a {0} handoff response requires a reason")]
    HandoffResponseMissingReason(&'static str),
    #[error("a rejected handoff requires at least one piece of evidence")]
    HandoffRejectionMissingEvidence,
    #[error("a partial handoff requires both an owned and a rejected scope")]
    HandoffPartialScopeMissing,
    #[error("only a partial handoff may carry owned or rejected scope")]
    HandoffScopeNotAllowed,
    #[error("permission option was not advertised: {0}")]
    PermissionOptionNotAdvertised(String),
    #[error("invalid {kind}: {value}")]
    InvalidEnum { kind: &'static str, value: String },
}
