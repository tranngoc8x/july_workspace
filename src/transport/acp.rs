use super::{
    AcpAgentConfig, AgentConnection, AgentTransport, CreateSession, PermissionRequest,
    PermissionRequestId, PermissionResponse, ResumeSession, SendMessage, SessionCreated,
    SessionRef, SessionResumed, TransportError, TransportEvent, TransportEvents,
    TransportFailureKind,
};
use crate::domain::{AgentId, PermissionOption, PermissionOutcome};
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    CancelNotification, CloseSessionRequest, ContentBlock, InitializeRequest, NewSessionRequest,
    PromptRequest, RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    ResumeSessionRequest, SelectedPermissionOutcome, SessionModeState, SessionNotification,
    SessionUpdate, SetSessionModeRequest, TextContent, ToolCallStatus,
};
use agent_client_protocol::{AcpAgent, Agent, ConnectionTo};
use std::collections::{HashMap, HashSet};
use std::fs::OpenOptions;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

const COMMAND_CAPACITY: usize = 32;
const EVENT_CAPACITY: usize = 256;
const CANCEL_GRACE: Duration = Duration::from_secs(10);

type Sessions = Arc<Mutex<HashMap<String, SessionRef>>>;
type ActiveTurns = Arc<Mutex<HashSet<String>>>;
type PendingPermissions = Arc<Mutex<HashMap<PermissionRequestId, PendingPermission>>>;

#[derive(Clone)]
struct ConnectionState {
    agent_id: AgentId,
    requires_default_mode: bool,
    events: mpsc::Sender<TransportEvent>,
    sessions: Sessions,
    active_turns: ActiveTurns,
    pending_permissions: PendingPermissions,
    permission_responses: Arc<AtomicUsize>,
}

struct PendingPermission {
    session: SessionRef,
    option_ids: HashSet<String>,
    response: oneshot::Sender<PermissionOutcome>,
}

enum Command {
    Create(
        CreateSession,
        oneshot::Sender<Result<SessionCreated, TransportError>>,
    ),
    Resume(
        ResumeSession,
        oneshot::Sender<Result<SessionResumed, TransportError>>,
    ),
    Send(SendMessage, oneshot::Sender<Result<(), TransportError>>),
    Cancel(SessionRef, oneshot::Sender<Result<(), TransportError>>),
    Permission(
        PermissionResponse,
        oneshot::Sender<Result<(), TransportError>>,
    ),
    Close(SessionRef, oneshot::Sender<Result<(), TransportError>>),
    Shutdown(oneshot::Sender<()>),
}

pub struct AcpTransport {
    config: AcpAgentConfig,
    commands: Option<mpsc::Sender<Command>>,
    events: Option<mpsc::Receiver<TransportEvent>>,
    event_sender: mpsc::Sender<TransportEvent>,
    task: Option<JoinHandle<()>>,
    project_root: Option<std::path::PathBuf>,
    shutting_down: Arc<AtomicBool>,
    pending_permissions: PendingPermissions,
}

impl AcpTransport {
    pub fn new(config: AcpAgentConfig) -> Self {
        let (event_sender, events) = mpsc::channel(EVENT_CAPACITY);
        Self {
            config,
            commands: None,
            events: Some(events),
            event_sender,
            task: None,
            project_root: None,
            shutting_down: Arc::new(AtomicBool::new(false)),
            pending_permissions: PendingPermissions::default(),
        }
    }

    async fn request<R>(
        &self,
        build: impl FnOnce(oneshot::Sender<Result<R, TransportError>>) -> Command,
    ) -> Result<R, TransportError> {
        let commands = self.commands.as_ref().ok_or(TransportError::NotConnected)?;
        let (response, received) = oneshot::channel();
        commands
            .send(build(response))
            .await
            .map_err(|_| TransportError::ChannelClosed)?;
        received.await.map_err(|_| TransportError::ChannelClosed)?
    }

    fn validate_session_root(&self, root: &Path) -> Result<(), TransportError> {
        if self.project_root.as_deref() == Some(root) {
            Ok(())
        } else {
            Err(TransportError::InvalidConfiguration(
                "session project root must match the connected agent",
            ))
        }
    }
}

impl Drop for AcpTransport {
    fn drop(&mut self) {
        self.shutting_down.store(true, Ordering::Release);
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

impl AgentTransport for AcpTransport {
    async fn connect(&mut self, agent: &AgentConnection) -> Result<(), TransportError> {
        validate_configuration(&self.config, agent)?;
        if self.commands.is_some() {
            return Err(TransportError::InvalidConfiguration(
                "ACP transport is already connected",
            ));
        }

        let sdk_config = agent_client_protocol::AcpAgentConfig::new(&self.config.executable)
            .args(self.config.arguments.clone())
            .envs(self.config.environment.clone());
        let acp_agent = AcpAgent::new(sdk_config);
        let (commands, command_receiver) = mpsc::channel(COMMAND_CAPACITY);
        let (ready, connected) = oneshot::channel();
        let config = self.config.clone();
        let agent_id = agent.agent_id;
        let event_sender = self.event_sender.clone();
        let shutting_down = self.shutting_down.clone();
        self.pending_permissions = PendingPermissions::default();
        let pending_permissions = self.pending_permissions.clone();
        shutting_down.store(false, Ordering::Release);

        self.task = Some(tokio::spawn(async move {
            let result = run_connection(
                acp_agent,
                config,
                agent_id,
                event_sender.clone(),
                command_receiver,
                ready,
                (pending_permissions, shutting_down.clone()),
            )
            .await;
            if let Err(error) = result {
                tracing::warn!(agent_id = %agent_id, transport = "acp", error_code = %error.code, "ACP transport disconnected");
                let event = TransportEvent::TransportDisconnected {
                    agent_id,
                    reason: sanitized_sdk_reason(&error),
                };
                if shutting_down.load(Ordering::Acquire) {
                    let _ = event_sender.try_send(event);
                } else {
                    let _ = event_sender.send(event).await;
                }
            }
        }));

        match connected.await {
            Ok(Ok(())) => {
                self.commands = Some(commands);
                self.project_root = Some(agent.project_root.clone());
                tracing::info!(agent_id = %agent.agent_id, transport = "acp", "ACP transport connected");
                Ok(())
            }
            Ok(Err(error)) => {
                self.task.take().expect("connection task exists").await.ok();
                Err(error)
            }
            Err(_) => {
                self.task.take().expect("connection task exists").await.ok();
                Err(TransportError::ChannelClosed)
            }
        }
    }

    async fn create_session(
        &mut self,
        request: CreateSession,
    ) -> Result<SessionCreated, TransportError> {
        self.validate_session_root(&request.project_root)?;
        self.request(|response| Command::Create(request, response))
            .await
    }

    async fn resume_session(
        &mut self,
        request: ResumeSession,
    ) -> Result<SessionResumed, TransportError> {
        self.validate_session_root(&request.project_root)?;
        self.request(|response| Command::Resume(request, response))
            .await
    }

    async fn send_message(&mut self, request: SendMessage) -> Result<(), TransportError> {
        self.request(|response| Command::Send(request, response))
            .await
    }

    async fn cancel_turn(&mut self, session: SessionRef) -> Result<(), TransportError> {
        self.request(|response| Command::Cancel(session, response))
            .await
    }

    async fn respond_permission(
        &mut self,
        response: PermissionResponse,
    ) -> Result<(), TransportError> {
        self.request(|reply| Command::Permission(response, reply))
            .await
    }

    async fn close_session(&mut self, session: SessionRef) -> Result<(), TransportError> {
        self.request(|response| Command::Close(session, response))
            .await
    }

    async fn shutdown(&mut self) -> Result<(), TransportError> {
        self.shutting_down.store(true, Ordering::Release);
        cancel_all_permissions(&self.pending_permissions);
        let Some(commands) = self.commands.take() else {
            return Ok(());
        };
        let (done, stopped) = oneshot::channel();
        let mut task = self.task.take();
        let graceful = async {
            if commands.send(Command::Shutdown(done)).await.is_ok() {
                let _ = stopped.await;
            }
            if let Some(task) = task.as_mut() {
                task.await.map_err(|error| {
                    TransportError::Disconnected(format!("ACP owner task failed: {error}"))
                })?;
            }
            Ok(())
        };
        let result = match tokio::time::timeout(CANCEL_GRACE, graceful).await {
            Ok(result) => result,
            Err(_) => {
                cancel_all_permissions(&self.pending_permissions);
                if let Some(task) = task {
                    task.abort();
                    let _ = task.await;
                }
                Ok(())
            }
        };
        self.project_root = None;
        result
    }

    fn subscribe(&mut self) -> Result<TransportEvents, TransportError> {
        self.events
            .take()
            .map(TransportEvents::new)
            .ok_or(TransportError::AlreadySubscribed)
    }
}

async fn run_connection(
    agent: AcpAgent,
    config: AcpAgentConfig,
    agent_id: AgentId,
    events: mpsc::Sender<TransportEvent>,
    commands: mpsc::Receiver<Command>,
    ready: oneshot::Sender<Result<(), TransportError>>,
    shutdown: (PendingPermissions, Arc<AtomicBool>),
) -> Result<(), agent_client_protocol::Error> {
    let (pending_permissions, shutting_down) = shutdown;
    let sessions = Sessions::default();
    let active_turns = ActiveTurns::default();
    let permission_responses = Arc::new(AtomicUsize::new(0));
    let state = ConnectionState {
        agent_id,
        requires_default_mode: config.expected_agent_name.to_lowercase().contains("claude"),
        events: events.clone(),
        sessions: sessions.clone(),
        active_turns,
        pending_permissions: pending_permissions.clone(),
        permission_responses: permission_responses.clone(),
    };

    let notification_sessions = sessions.clone();
    let notification_events = events.clone();
    let permission_sessions = sessions.clone();
    let permission_events = events.clone();
    let handler_permissions = pending_permissions.clone();
    let handler_responses = permission_responses;
    let handler_shutting_down = shutting_down;

    agent_client_protocol::Client
        .builder()
        .on_receive_notification(
            async move |notification: SessionNotification, _connection| {
                forward_notification(notification, &notification_sessions, &notification_events)
                    .await;
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            async move |request: RequestPermissionRequest, responder, connection| {
                if handler_shutting_down.load(Ordering::Acquire) {
                    return responder.respond(RequestPermissionResponse::new(
                        RequestPermissionOutcome::Cancelled,
                    ));
                }
                let Some(session) =
                    session_for(&permission_sessions, &request.session_id.to_string())
                else {
                    return responder.respond(RequestPermissionResponse::new(
                        RequestPermissionOutcome::Cancelled,
                    ));
                };

                let request_id = PermissionRequestId::from(ulid::Ulid::generate().to_string());
                let options = request
                    .options
                    .iter()
                    .map(|option| PermissionOption {
                        id: option.option_id.to_string(),
                        label: option.name.clone(),
                    })
                    .collect::<Vec<_>>();
                let option_ids = options.iter().map(|option| option.id.clone()).collect();
                let (response, decision) = oneshot::channel();
                let mut permissions = lock(&handler_permissions);
                if handler_shutting_down.load(Ordering::Acquire) {
                    return responder.respond(RequestPermissionResponse::new(
                        RequestPermissionOutcome::Cancelled,
                    ));
                }
                permissions.insert(
                    request_id.clone(),
                    PendingPermission {
                        session: session.clone(),
                        option_ids,
                        response,
                    },
                );
                drop(permissions);
                handler_responses.fetch_add(1, Ordering::AcqRel);

                let event = TransportEvent::PermissionRequested(PermissionRequest {
                    session,
                    request_id: request_id.clone(),
                    options,
                });
                let events = permission_events.clone();
                let responses = handler_responses.clone();
                let spawn = connection.spawn(async move {
                    let outcome = if events.send(event).await.is_ok() {
                        decision.await.unwrap_or(PermissionOutcome::Cancelled)
                    } else {
                        PermissionOutcome::Cancelled
                    };
                    let result = responder.respond(RequestPermissionResponse::new(match outcome {
                        PermissionOutcome::Selected(option_id) => {
                            RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                                option_id,
                            ))
                        }
                        PermissionOutcome::Cancelled => RequestPermissionOutcome::Cancelled,
                    }));
                    responses.fetch_sub(1, Ordering::AcqRel);
                    result
                });
                if let Err(error) = spawn {
                    handler_responses.fetch_sub(1, Ordering::AcqRel);
                    lock(&handler_permissions).remove(&request_id);
                    return Err(error);
                }
                Ok(())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(agent, async move |connection: ConnectionTo<Agent>| {
            let initialized = match connection
                .send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await
            {
                Ok(initialized) => initialized,
                Err(error) => {
                    let _ = ready.send(Err(map_sdk_error(error)));
                    return Ok(());
                }
            };

            if let Err(error) = validate_handshake(&config, &initialized) {
                let _ = ready.send(Err(error));
                return Ok(());
            }
            let _ = ready.send(Ok(()));
            command_loop(connection, commands, state).await
        })
        .await
}

fn validate_configuration(
    config: &AcpAgentConfig,
    agent: &AgentConnection,
) -> Result<(), TransportError> {
    if !config.executable.is_absolute() {
        return Err(TransportError::InvalidConfiguration(
            "ACP executable must be an absolute path",
        ));
    }
    if !config.executable.is_file() {
        return Err(TransportError::InvalidConfiguration(
            "ACP executable must exist and be a file",
        ));
    }
    if config
        .arguments
        .iter()
        .any(|argument| argument.contains("@latest"))
    {
        return Err(TransportError::InvalidConfiguration(
            "ACP adapter arguments must not use @latest",
        ));
    }
    if !agent.project_root.is_absolute() || !agent.project_root.is_dir() {
        return Err(TransportError::InvalidConfiguration(
            "agent project root must be an existing absolute directory",
        ));
    }
    if !config.state_directory.is_absolute() || !config.state_directory.is_dir() {
        return Err(TransportError::InvalidConfiguration(
            "agent state directory must be an existing absolute directory",
        ));
    }
    verify_writable(&config.state_directory)
}

fn verify_writable(directory: &Path) -> Result<(), TransportError> {
    let probe = directory.join(format!(".july-write-probe-{}", ulid::Ulid::generate()));
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)?;
    std::fs::remove_file(probe)?;
    Ok(())
}

fn validate_handshake(
    config: &AcpAgentConfig,
    initialized: &agent_client_protocol::schema::v1::InitializeResponse,
) -> Result<(), TransportError> {
    if initialized.protocol_version != ProtocolVersion::V1 {
        return Err(TransportError::UnsupportedProtocol {
            expected: 1,
            actual: initialized.protocol_version.as_u16(),
        });
    }
    let info =
        initialized
            .agent_info
            .as_ref()
            .ok_or_else(|| TransportError::UnexpectedAgentIdentity {
                expected: format!(
                    "{} {}",
                    config.expected_agent_name, config.expected_agent_version
                ),
                actual: "missing agentInfo".into(),
            })?;
    let actual = format!("{} {}", info.name, info.version);
    let expected = format!(
        "{} {}",
        config.expected_agent_name, config.expected_agent_version
    );
    if info.name != config.expected_agent_name || info.version != config.expected_agent_version {
        return Err(TransportError::UnexpectedAgentIdentity { expected, actual });
    }
    let capabilities = &initialized.agent_capabilities.session_capabilities;
    if capabilities.resume.is_none() {
        return Err(TransportError::UnsupportedCapability("session/resume"));
    }
    if capabilities.close.is_none() {
        return Err(TransportError::UnsupportedCapability("session/close"));
    }
    Ok(())
}

async fn command_loop(
    connection: ConnectionTo<Agent>,
    mut commands: mpsc::Receiver<Command>,
    state: ConnectionState,
) -> Result<(), agent_client_protocol::Error> {
    loop {
        let command = tokio::select! {
            _ = connection.incoming_closed() => {
                cancel_all_permissions(&state.pending_permissions);
                return Err(agent_client_protocol::util::internal_error(
                    "ACP subprocess closed its output stream",
                ));
            }
            command = commands.recv() => command,
        };
        let Some(command) = command else {
            cancel_all_permissions(&state.pending_permissions);
            let _ = state
                .events
                .send(TransportEvent::TransportDisconnected {
                    agent_id: state.agent_id,
                    reason: "command owner was dropped".into(),
                })
                .await;
            return Ok(());
        };
        match command {
            Command::Create(request, response) => {
                let result = create_session(
                    &connection,
                    request,
                    &state.sessions,
                    state.requires_default_mode,
                )
                .await;
                let _ = response.send(result);
            }
            Command::Resume(request, response) => {
                let session = request.session.clone();
                let result = match validate_resumable_ref(&state.sessions, &session) {
                    Ok(()) => {
                        resume_session(
                            &connection,
                            request,
                            &state.sessions,
                            state.requires_default_mode,
                        )
                        .await
                    }
                    Err(error) => Err(error),
                };
                if matches!(&result, Err(TransportError::SessionLost(_))) {
                    let _ = response.send(result);
                    let events = state.events.clone();
                    connection.spawn(async move {
                        let _ = events.send(TransportEvent::SessionLost { session }).await;
                        Ok(())
                    })?;
                } else {
                    let _ = response.send(result);
                }
            }
            Command::Send(request, response) => {
                let result =
                    validate_session_ref(&state.sessions, &request.session).and_then(|()| {
                        start_turn(&connection, request, &state.events, &state.active_turns)
                    });
                let _ = response.send(result);
            }
            Command::Cancel(session, response) => {
                let result = validate_session_ref(&state.sessions, &session).and_then(|()| {
                    cancel_turn(
                        &connection,
                        session,
                        &state.active_turns,
                        &state.pending_permissions,
                    )
                });
                let _ = response.send(result);
            }
            Command::Permission(response_value, response) => {
                let result = respond_permission(response_value, &state.pending_permissions);
                let _ = response.send(result);
            }
            Command::Close(session, response) => {
                let result = match validate_session_ref(&state.sessions, &session) {
                    Ok(()) => close_session(&connection, session, &state.sessions).await,
                    Err(error) => Err(error),
                };
                let _ = response.send(result);
            }
            Command::Shutdown(done) => {
                let result = shutdown_connection(&connection, &state).await;
                let _ = done.send(());
                return result;
            }
        }
    }
}

async fn create_session(
    connection: &ConnectionTo<Agent>,
    request: CreateSession,
    sessions: &Sessions,
    requires_default_mode: bool,
) -> Result<SessionCreated, TransportError> {
    let created = connection
        .send_request(NewSessionRequest::new(request.project_root))
        .block_task()
        .await
        .map_err(map_sdk_error)?;
    let session = SessionRef {
        binding_id: request.binding_id,
        remote_session_id: created.session_id.to_string(),
    };
    if session_for(sessions, &session.remote_session_id).is_some() {
        return Err(TransportError::SessionReferenceMismatch(
            session.remote_session_id,
        ));
    }
    if let Err(error) = enforce_default_mode(
        connection,
        &session.remote_session_id,
        created.modes.as_ref(),
        requires_default_mode,
    )
    .await
    {
        let _ = connection
            .send_request(CloseSessionRequest::new(created.session_id.clone()))
            .block_task()
            .await;
        return Err(error);
    }
    lock(sessions).insert(session.remote_session_id.clone(), session.clone());
    Ok(SessionCreated { session })
}

async fn resume_session(
    connection: &ConnectionTo<Agent>,
    request: ResumeSession,
    sessions: &Sessions,
    requires_default_mode: bool,
) -> Result<SessionResumed, TransportError> {
    let resumed = connection
        .send_request(ResumeSessionRequest::new(
            request.session.remote_session_id.clone(),
            request.project_root,
        ))
        .block_task()
        .await
        .map_err(|error| {
            if error.code == agent_client_protocol::schema::v1::ErrorCode::ResourceNotFound {
                TransportError::SessionLost(request.session.remote_session_id.clone())
            } else {
                map_sdk_error(error)
            }
        })?;
    enforce_default_mode(
        connection,
        &request.session.remote_session_id,
        resumed.modes.as_ref(),
        requires_default_mode,
    )
    .await?;
    lock(sessions).insert(
        request.session.remote_session_id.clone(),
        request.session.clone(),
    );
    Ok(SessionResumed {
        session: request.session,
    })
}

async fn enforce_default_mode(
    connection: &ConnectionTo<Agent>,
    session_id: &str,
    modes: Option<&SessionModeState>,
    required: bool,
) -> Result<(), TransportError> {
    let Some(modes) = modes else {
        return if required {
            Err(TransportError::UnsupportedCapability(
                "manual default session mode",
            ))
        } else {
            Ok(())
        };
    };
    let has_default = modes
        .available_modes
        .iter()
        .any(|mode| mode.id.to_string() == "default");
    if required && !has_default {
        return Err(TransportError::UnsupportedCapability(
            "manual default session mode",
        ));
    }
    if has_default && modes.current_mode_id.to_string() != "default" {
        connection
            .send_request(SetSessionModeRequest::new(session_id.to_owned(), "default"))
            .block_task()
            .await
            .map_err(map_sdk_error)?;
    }
    Ok(())
}

fn start_turn(
    connection: &ConnectionTo<Agent>,
    request: SendMessage,
    events: &mpsc::Sender<TransportEvent>,
    active_turns: &ActiveTurns,
) -> Result<(), TransportError> {
    if request.content.trim() == "/clear" {
        return Err(TransportError::InvalidConfiguration(
            "the /clear command is not supported",
        ));
    }
    let remote_id = request.session.remote_session_id.clone();
    if !lock(active_turns).insert(remote_id.clone()) {
        return Err(TransportError::TurnAlreadyActive(remote_id));
    }

    let connection_for_prompt = connection.clone();
    let events = events.clone();
    let active_turns = active_turns.clone();
    let session = request.session;
    connection
        .spawn(async move {
            let _ = events
                .send(TransportEvent::TurnStarted {
                    session: session.clone(),
                })
                .await;
            let result = connection_for_prompt
                .send_request(PromptRequest::new(
                    remote_id.clone(),
                    vec![ContentBlock::Text(TextContent::new(request.content))],
                ))
                .block_task()
                .await;
            match result {
                Ok(_) => {
                    let _ = events
                        .send(TransportEvent::AgentMessageCompleted {
                            session: session.clone(),
                        })
                        .await;
                    let _ = events.send(TransportEvent::TurnCompleted { session }).await;
                }
                Err(error) => {
                    let _ = events
                        .send(TransportEvent::TurnFailed {
                            session,
                            failure: sdk_failure_kind(&error),
                        })
                        .await;
                }
            }
            lock(&active_turns).remove(&remote_id);
            Ok(())
        })
        .map_err(map_sdk_error)
}

fn cancel_turn(
    connection: &ConnectionTo<Agent>,
    session: SessionRef,
    active_turns: &ActiveTurns,
    pending_permissions: &PendingPermissions,
) -> Result<(), TransportError> {
    connection
        .send_notification(CancelNotification::new(session.remote_session_id.clone()))
        .map_err(map_sdk_error)?;
    cancel_session_permissions(pending_permissions, &session);

    let active_turns = active_turns.clone();
    let remote_id = session.remote_session_id;
    connection
        .spawn(async move {
            tokio::time::sleep(CANCEL_GRACE).await;
            if lock(&active_turns).contains(&remote_id) {
                return Err(agent_client_protocol::util::internal_error(
                    "turn did not stop within July's cancellation grace period",
                ));
            }
            Ok(())
        })
        .map_err(map_sdk_error)
}

fn respond_permission(
    response: PermissionResponse,
    pending_permissions: &PendingPermissions,
) -> Result<(), TransportError> {
    let mut pending = lock(pending_permissions);
    let Some(request) = pending.remove(&response.request_id) else {
        return Err(TransportError::PermissionRequestNotFound(
            response.request_id.to_string(),
        ));
    };
    if request.session != response.session {
        let _ = request.response.send(PermissionOutcome::Cancelled);
        return Err(TransportError::PermissionRequestNotFound(
            response.request_id.to_string(),
        ));
    }
    if let PermissionOutcome::Selected(option_id) = &response.outcome
        && !request.option_ids.contains(option_id)
    {
        let _ = request.response.send(PermissionOutcome::Cancelled);
        return Err(TransportError::PermissionOptionNotAdvertised(
            option_id.clone(),
        ));
    }
    request
        .response
        .send(response.outcome)
        .map_err(|_| TransportError::ChannelClosed)
}

async fn close_session(
    connection: &ConnectionTo<Agent>,
    session: SessionRef,
    sessions: &Sessions,
) -> Result<(), TransportError> {
    connection
        .send_request(CloseSessionRequest::new(session.remote_session_id.clone()))
        .block_task()
        .await
        .map_err(map_sdk_error)?;
    lock(sessions).remove(&session.remote_session_id);
    Ok(())
}

async fn forward_notification(
    notification: SessionNotification,
    sessions: &Sessions,
    events: &mpsc::Sender<TransportEvent>,
) {
    let Some(session) = session_for(sessions, &notification.session_id.to_string()) else {
        return;
    };
    let event = match notification.update {
        SessionUpdate::AgentMessageChunk(chunk) => match chunk.content {
            ContentBlock::Text(text) => Some(TransportEvent::AgentTextDelta {
                session,
                text: text.text,
            }),
            _ => None,
        },
        SessionUpdate::ToolCall(tool) => Some(TransportEvent::ToolCallStarted {
            session,
            tool_call_id: tool.tool_call_id.to_string(),
            title: tool.title,
        }),
        SessionUpdate::ToolCallUpdate(update)
            if matches!(
                update.fields.status,
                Some(ToolCallStatus::Completed | ToolCallStatus::Failed)
            ) =>
        {
            Some(TransportEvent::ToolCallFinished {
                session,
                tool_call_id: update.tool_call_id.to_string(),
            })
        }
        SessionUpdate::UsageUpdate(usage) => Some(TransportEvent::UsageReported {
            session,
            used_tokens: usage.used,
            context_window_tokens: usage.size,
        }),
        _ => None,
    };
    if let Some(event) = event {
        let _ = events.send(event).await;
    }
}

fn cancel_session_permissions(pending: &PendingPermissions, session: &SessionRef) {
    let ids = lock(pending)
        .iter()
        .filter(|(_, request)| request.session == *session)
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    let mut pending = lock(pending);
    for id in ids {
        if let Some(request) = pending.remove(&id) {
            let _ = request.response.send(PermissionOutcome::Cancelled);
        }
    }
}

fn cancel_all_permissions(pending: &PendingPermissions) {
    for (_, request) in lock(pending).drain() {
        let _ = request.response.send(PermissionOutcome::Cancelled);
    }
}

async fn shutdown_connection(
    connection: &ConnectionTo<Agent>,
    state: &ConnectionState,
) -> Result<(), agent_client_protocol::Error> {
    let active = lock(&state.active_turns)
        .iter()
        .cloned()
        .collect::<Vec<_>>();
    for remote_id in active {
        let _ = connection.send_notification(CancelNotification::new(remote_id));
    }
    cancel_all_permissions(&state.pending_permissions);
    tokio::time::timeout(CANCEL_GRACE, async {
        while !lock(&state.active_turns).is_empty()
            || state.permission_responses.load(Ordering::Acquire) != 0
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| {
        agent_client_protocol::util::internal_error(
            "active turns did not stop within July's shutdown grace period",
        )
    })?;
    Ok(())
}

fn session_for(sessions: &Sessions, remote_id: &str) -> Option<SessionRef> {
    lock(sessions).get(remote_id).cloned()
}

fn validate_session_ref(sessions: &Sessions, session: &SessionRef) -> Result<(), TransportError> {
    match session_for(sessions, &session.remote_session_id) {
        Some(admitted) if admitted == *session => Ok(()),
        _ => Err(TransportError::SessionReferenceMismatch(
            session.remote_session_id.clone(),
        )),
    }
}

fn validate_resumable_ref(sessions: &Sessions, session: &SessionRef) -> Result<(), TransportError> {
    match session_for(sessions, &session.remote_session_id) {
        Some(admitted) if admitted != *session => Err(TransportError::SessionReferenceMismatch(
            session.remote_session_id.clone(),
        )),
        _ => Ok(()),
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn map_sdk_error(error: agent_client_protocol::Error) -> TransportError {
    use agent_client_protocol::schema::v1::ErrorCode;
    match error.code {
        ErrorCode::AuthRequired => TransportError::AuthenticationRequired,
        _ => TransportError::Protocol(sanitized_sdk_reason(&error)),
    }
}

fn sanitized_sdk_reason(error: &agent_client_protocol::Error) -> String {
    format!("ACP request failed ({})", error.code)
}

fn sdk_failure_kind(error: &agent_client_protocol::Error) -> TransportFailureKind {
    if error.code == agent_client_protocol::schema::v1::ErrorCode::AuthRequired {
        TransportFailureKind::AuthenticationRequired
    } else {
        TransportFailureKind::Protocol
    }
}
