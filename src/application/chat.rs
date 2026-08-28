use crate::domain::{Message, PermissionOption};
use std::fmt::{self, Display, Formatter};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ChatPermissionRequestId(String);

impl ChatPermissionRequestId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for ChatPermissionRequestId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl Display for ChatPermissionRequestId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChatFailureKind {
    AuthenticationRequired,
    Protocol,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ChatEvent {
    TextDelta(String),
    MessageCompleted(Message),
    PermissionRequested {
        request_id: ChatPermissionRequestId,
        prompt: String,
        options: Vec<PermissionOption>,
    },
    TurnCompleted,
    TurnFailed(ChatFailureKind),
    Disconnected(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum ChatRuntimeEvent {
    TextDelta(String),
    AgentMessageCompleted,
    PermissionRequested {
        request_id: ChatPermissionRequestId,
        prompt: String,
        options: Vec<PermissionOption>,
    },
    TurnCompleted,
    TurnFailed(ChatFailureKind),
    Disconnected(String),
    SessionLost,
}
