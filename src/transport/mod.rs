//! Transport boundary for external agent protocol implementations.

mod acp;
mod error;

use crate::domain::{AgentId, PermissionOption, PermissionOutcome, SessionBindingId};
use std::collections::BTreeMap;
use std::fmt::{self, Display, Formatter};
use std::future::Future;
use std::path::PathBuf;
use tokio::sync::mpsc;

pub use acp::AcpTransport;
pub use error::TransportError;

const TEXT_EVENT_MAX_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentConnection {
    pub agent_id: AgentId,
    pub project_root: PathBuf,
}

/// Explicit launch configuration consumed only by the ACP adapter.
#[derive(Clone, Eq, PartialEq)]
pub struct AcpAgentConfig {
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub environment: BTreeMap<String, String>,
    pub state_directory: PathBuf,
    pub expected_agent_name: String,
    pub expected_agent_version: String,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TransportCapabilities {
    pub resume_session: bool,
    pub close_session: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionRef {
    pub binding_id: SessionBindingId,
    pub remote_session_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateSession {
    pub binding_id: SessionBindingId,
    pub project_root: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionCreated {
    pub session: SessionRef,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResumeSession {
    pub session: SessionRef,
    pub project_root: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionResumed {
    pub session: SessionRef,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SendMessage {
    pub session: SessionRef,
    pub content: String,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct PermissionRequestId(String);

impl PermissionRequestId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for PermissionRequestId {
    fn from(value: &str) -> Self {
        Self(value.into())
    }
}

impl From<String> for PermissionRequestId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl Display for PermissionRequestId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PermissionRequest {
    pub session: SessionRef,
    pub request_id: PermissionRequestId,
    pub options: Vec<PermissionOption>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PermissionResponse {
    pub session: SessionRef,
    pub request_id: PermissionRequestId,
    pub outcome: PermissionOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransportEvent {
    TurnStarted {
        session: SessionRef,
    },
    AgentTextDelta {
        session: SessionRef,
        text: String,
    },
    AgentMessageCompleted {
        session: SessionRef,
    },
    ToolCallStarted {
        session: SessionRef,
        tool_call_id: String,
        title: String,
    },
    ToolCallFinished {
        session: SessionRef,
        tool_call_id: String,
    },
    PermissionRequested(PermissionRequest),
    TransportDisconnected {
        agent_id: AgentId,
        reason: String,
    },
    TurnCompleted {
        session: SessionRef,
    },
    TurnFailed {
        session: SessionRef,
        failure: TransportFailureKind,
    },
    UsageReported {
        session: SessionRef,
        used_tokens: u64,
        context_window_tokens: u64,
    },
    SessionLost {
        session: SessionRef,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportFailureKind {
    AuthenticationRequired,
    Protocol,
}

/// The single ordered event receiver transferred from a connected transport.
pub struct TransportEvents {
    receiver: mpsc::Receiver<TransportEvent>,
    pending: Option<TransportEvent>,
}

impl TransportEvents {
    pub fn new(receiver: mpsc::Receiver<TransportEvent>) -> Self {
        Self {
            receiver,
            pending: None,
        }
    }

    pub async fn recv(&mut self) -> Option<TransportEvent> {
        let event = match self.pending.take() {
            Some(event) => event,
            None => self.receiver.recv().await?,
        };
        let TransportEvent::AgentTextDelta { session, mut text } = event else {
            return Some(event);
        };

        if text.len() > TEXT_EVENT_MAX_BYTES {
            let split = char_boundary(&text, TEXT_EVENT_MAX_BYTES);
            let remainder = text.split_off(split);
            self.pending = Some(TransportEvent::AgentTextDelta {
                session: session.clone(),
                text: remainder,
            });
            return Some(TransportEvent::AgentTextDelta { session, text });
        }

        while text.len() < TEXT_EVENT_MAX_BYTES {
            let Ok(next) = self.receiver.try_recv() else {
                break;
            };
            match next {
                TransportEvent::AgentTextDelta {
                    session: next_session,
                    text: mut next_text,
                } if next_session == session => {
                    let available = TEXT_EVENT_MAX_BYTES - text.len();
                    if next_text.len() <= available {
                        text.push_str(&next_text);
                    } else {
                        let split = char_boundary(&next_text, available);
                        let remainder = next_text.split_off(split);
                        text.push_str(&next_text);
                        self.pending = Some(TransportEvent::AgentTextDelta {
                            session: next_session,
                            text: remainder,
                        });
                        break;
                    }
                }
                next => {
                    self.pending = Some(next);
                    break;
                }
            }
        }
        Some(TransportEvent::AgentTextDelta { session, text })
    }

    pub fn try_recv(&mut self) -> Option<TransportEvent> {
        self.pending
            .take()
            .or_else(|| self.receiver.try_recv().ok())
    }
}

fn char_boundary(text: &str, maximum: usize) -> usize {
    let mut boundary = maximum.min(text.len());
    while !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    boundary
}

pub trait AgentTransport {
    fn connect(
        &mut self,
        agent: &AgentConnection,
    ) -> impl Future<Output = Result<(), TransportError>> + Send;
    fn create_session(
        &mut self,
        request: CreateSession,
    ) -> impl Future<Output = Result<SessionCreated, TransportError>> + Send;
    fn resume_session(
        &mut self,
        request: ResumeSession,
    ) -> impl Future<Output = Result<SessionResumed, TransportError>> + Send;
    fn send_message(
        &mut self,
        request: SendMessage,
    ) -> impl Future<Output = Result<(), TransportError>> + Send;
    fn cancel_turn(
        &mut self,
        session: SessionRef,
    ) -> impl Future<Output = Result<(), TransportError>> + Send;
    fn respond_permission(
        &mut self,
        response: PermissionResponse,
    ) -> impl Future<Output = Result<(), TransportError>> + Send;
    fn close_session(
        &mut self,
        session: SessionRef,
    ) -> impl Future<Output = Result<(), TransportError>> + Send;
    fn shutdown(&mut self) -> impl Future<Output = Result<(), TransportError>> + Send;
    fn subscribe(&mut self) -> Result<TransportEvents, TransportError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{AgentId, SessionBindingId};
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    #[tokio::test]
    async fn transport_events_transfer_one_ordered_receiver() {
        let binding_id = SessionBindingId::new();
        let session = SessionRef {
            binding_id,
            remote_session_id: "remote-1".into(),
        };
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        sender
            .send(TransportEvent::TurnStarted {
                session: session.clone(),
            })
            .await
            .unwrap();

        let mut events = TransportEvents::new(receiver);
        assert_eq!(
            events.recv().await,
            Some(TransportEvent::TurnStarted { session })
        );
    }

    #[tokio::test]
    async fn transport_events_coalesce_only_consecutive_text() {
        let session = SessionRef {
            binding_id: SessionBindingId::new(),
            remote_session_id: "remote-1".into(),
        };
        let (sender, receiver) = tokio::sync::mpsc::channel(4);
        for text in ["hello ", "world"] {
            sender
                .send(TransportEvent::AgentTextDelta {
                    session: session.clone(),
                    text: text.into(),
                })
                .await
                .unwrap();
        }
        let permission = PermissionRequest {
            session: session.clone(),
            request_id: PermissionRequestId::from("permission-1"),
            options: vec![],
        };
        sender
            .send(TransportEvent::PermissionRequested(permission.clone()))
            .await
            .unwrap();
        sender
            .send(TransportEvent::TurnCompleted {
                session: session.clone(),
            })
            .await
            .unwrap();

        let mut events = TransportEvents::new(receiver);
        assert_eq!(
            events.recv().await,
            Some(TransportEvent::AgentTextDelta {
                session: session.clone(),
                text: "hello world".into(),
            })
        );
        assert_eq!(
            events.recv().await,
            Some(TransportEvent::PermissionRequested(permission))
        );
        assert_eq!(
            events.recv().await,
            Some(TransportEvent::TurnCompleted { session })
        );
    }

    #[test]
    fn contract_uses_july_owned_values() {
        let connection = AgentConnection {
            agent_id: AgentId::new(),
            project_root: PathBuf::from("/workspace/project"),
        };
        let config = AcpAgentConfig {
            executable: PathBuf::from("/opt/july/codex-acp"),
            arguments: Vec::new(),
            environment: BTreeMap::from([("NO_BROWSER".into(), "1".into())]),
            state_directory: PathBuf::from("/workspace/.codex"),
            expected_agent_name: "codex-acp".into(),
            expected_agent_version: "1.1.13".into(),
        };
        let request = PermissionResponse {
            session: SessionRef {
                binding_id: SessionBindingId::new(),
                remote_session_id: "remote-1".into(),
            },
            request_id: PermissionRequestId::from("permission-1"),
            outcome: PermissionOutcome::Selected("allow-once".into()),
        };

        assert!(connection.project_root.is_absolute());
        assert!(config.executable.is_absolute());
        assert!(matches!(request.outcome, PermissionOutcome::Selected(_)));
    }
}
