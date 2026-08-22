use super::{
    AgentDirectMessageRuntime, AgentThreadRuntime, RuntimeError, SessionManager, StorageHandle,
    StorageWorker, timestamp,
};
use crate::domain::{Agent, AgentId, PermissionOutcome, SessionBinding, SessionBindingId};
use crate::transport::{
    AgentConnection, AgentTransport, PermissionRequestId, PermissionResponse, SendMessage,
    SessionRef, TransportEvent,
};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::{mpsc, oneshot};

const WORKSPACE_CAPACITY: usize = 64;
const OWNER_CAPACITY: usize = 64;
const SESSION_EVENT_CAPACITY: usize = 64;

type Reply<T> = oneshot::Sender<Result<T, RuntimeError>>;

enum WorkspaceCommand<T> {
    RegisterAgent(AgentConnection, T, Reply<()>),
    OpenSession {
        agent_id: AgentId,
        binding: SessionBinding,
        project_root: PathBuf,
        resumed_at: String,
        reply: Reply<RuntimeSession>,
    },
    Shutdown(String, Reply<()>),
}

pub(crate) struct WorkspaceHandle<T> {
    commands: mpsc::Sender<WorkspaceCommand<T>>,
    storage: StorageHandle,
    stopped: Arc<AtomicBool>,
}

impl<T> Clone for WorkspaceHandle<T> {
    fn clone(&self) -> Self {
        Self {
            commands: self.commands.clone(),
            storage: self.storage.clone(),
            stopped: self.stopped.clone(),
        }
    }
}

impl<T: AgentTransport + Send + 'static> WorkspaceHandle<T> {
    pub(crate) fn ensure_running(&self) -> Result<(), RuntimeError> {
        if self.stopped.load(Ordering::Acquire) {
            Err(RuntimeError::WorkspaceStopped)
        } else {
            Ok(())
        }
    }

    pub(crate) fn storage(&self) -> &StorageHandle {
        &self.storage
    }

    pub(crate) async fn register_agent(
        &self,
        connection: AgentConnection,
        transport: T,
    ) -> Result<(), RuntimeError> {
        self.request(|reply| WorkspaceCommand::RegisterAgent(connection, transport, reply))
            .await
    }

    pub(crate) async fn open_session(
        &self,
        agent_id: AgentId,
        binding: SessionBinding,
        project_root: PathBuf,
        resumed_at: String,
    ) -> Result<RuntimeSession, RuntimeError> {
        self.request(|reply| WorkspaceCommand::OpenSession {
            agent_id,
            binding,
            project_root,
            resumed_at,
            reply,
        })
        .await
    }

    async fn request<R>(
        &self,
        build: impl FnOnce(Reply<R>) -> WorkspaceCommand<T>,
    ) -> Result<R, RuntimeError> {
        self.ensure_running()?;
        let (reply, response) = oneshot::channel();
        self.commands
            .send(build(reply))
            .await
            .map_err(|_| RuntimeError::ChannelClosed)?;
        response.await.map_err(|_| RuntimeError::ChannelClosed)?
    }
}

enum OwnerCommand {
    OpenSession {
        binding: SessionBinding,
        project_root: PathBuf,
        resumed_at: String,
        events: mpsc::Sender<TransportEvent>,
        reply: Reply<SessionRef>,
    },
    SendMessage(SendMessage, Reply<()>),
    CancelTurn(SessionRef, String, Reply<()>),
    RespondPermission(PermissionResponse, String, Reply<()>),
    Detach(SessionRef, String, Reply<()>),
    Shutdown(String, Reply<()>),
}

struct AgentOwner {
    commands: mpsc::Sender<OwnerCommand>,
    task: Option<tokio::task::JoinHandle<Result<(), RuntimeError>>>,
}

pub(crate) struct RuntimeSession {
    session: SessionRef,
    commands: mpsc::Sender<OwnerCommand>,
    events: mpsc::Receiver<TransportEvent>,
}

impl RuntimeSession {
    pub(crate) fn session(&self) -> &SessionRef {
        &self.session
    }

    pub(crate) async fn send_message(&self, content: String) -> Result<(), RuntimeError> {
        self.request(|reply| {
            OwnerCommand::SendMessage(
                SendMessage {
                    session: self.session.clone(),
                    content,
                },
                reply,
            )
        })
        .await
    }

    pub(crate) async fn cancel_turn(&self, cancelled_at: String) -> Result<(), RuntimeError> {
        let session = self.session.clone();
        self.request(|reply| OwnerCommand::CancelTurn(session, cancelled_at, reply))
            .await
    }

    pub(crate) async fn next_event(&mut self) -> Option<TransportEvent> {
        self.events.recv().await
    }

    pub(crate) async fn respond_permission(
        &self,
        request_id: PermissionRequestId,
        outcome: PermissionOutcome,
        decided_at: String,
    ) -> Result<(), RuntimeError> {
        self.request(|reply| {
            OwnerCommand::RespondPermission(
                PermissionResponse {
                    session: self.session.clone(),
                    request_id,
                    outcome,
                },
                decided_at,
                reply,
            )
        })
        .await
    }

    pub(crate) async fn detach(&mut self, detached_at: String) -> Result<(), RuntimeError> {
        let session = self.session.clone();
        self.request(|reply| OwnerCommand::Detach(session, detached_at, reply))
            .await
    }

    async fn request(
        &self,
        build: impl FnOnce(Reply<()>) -> OwnerCommand,
    ) -> Result<(), RuntimeError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(build(reply))
            .await
            .map_err(|_| RuntimeError::ChannelClosed)?;
        response.await.map_err(|_| RuntimeError::ChannelClosed)?
    }
}

pub struct WorkspaceRuntime<T: AgentTransport + Send + 'static> {
    handle: WorkspaceHandle<T>,
    storage: StorageWorker,
    task: Option<tokio::task::JoinHandle<Result<(), RuntimeError>>>,
    stopped: Arc<AtomicBool>,
}

impl<T: AgentTransport + Send + 'static> WorkspaceRuntime<T> {
    pub fn new(storage: StorageWorker) -> Result<Self, RuntimeError> {
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| RuntimeError::TokioRuntimeUnavailable)?;
        let (commands, receiver) = mpsc::channel(WORKSPACE_CAPACITY);
        let storage_handle = storage.handle();
        let stopped = Arc::new(AtomicBool::new(false));
        let handle = WorkspaceHandle {
            commands,
            storage: storage_handle.clone(),
            stopped: stopped.clone(),
        };
        Ok(Self {
            handle,
            storage,
            task: Some(runtime.spawn(run_workspace(receiver, storage_handle))),
            stopped,
        })
    }

    pub fn direct_message(
        &self,
        transport: T,
    ) -> Result<AgentDirectMessageRuntime<T>, RuntimeError> {
        self.handle.ensure_running()?;
        Ok(AgentDirectMessageRuntime::from_workspace(
            self.handle.clone(),
            Some(transport),
            None,
        ))
    }

    pub fn direct_message_for_agent(
        &self,
        agent_id: AgentId,
    ) -> Result<AgentDirectMessageRuntime<T>, RuntimeError> {
        self.handle.ensure_running()?;
        Ok(AgentDirectMessageRuntime::from_workspace(
            self.handle.clone(),
            None,
            Some(agent_id),
        ))
    }

    pub fn thread(&self, agent_id: AgentId) -> Result<AgentThreadRuntime<T>, RuntimeError> {
        self.handle.ensure_running()?;
        Ok(AgentThreadRuntime::from_workspace(
            self.handle.clone(),
            None,
            Some(agent_id),
        ))
    }

    pub fn thread_with_transport(
        &self,
        transport: T,
    ) -> Result<AgentThreadRuntime<T>, RuntimeError> {
        self.handle.ensure_running()?;
        Ok(AgentThreadRuntime::from_workspace(
            self.handle.clone(),
            Some(transport),
            None,
        ))
    }

    pub(crate) fn direct_message_with_agent(
        &self,
        transport: T,
        agent: Agent,
    ) -> AgentDirectMessageRuntime<T> {
        AgentDirectMessageRuntime::from_workspace(
            self.handle.clone(),
            Some(transport),
            Some(agent.id),
        )
        .with_agent(agent)
    }

    pub async fn shutdown(&mut self, stopped_at: String) -> Result<(), RuntimeError> {
        if self.stopped.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let (reply, response) = oneshot::channel();
        let requested = self
            .handle
            .commands
            .send(WorkspaceCommand::Shutdown(stopped_at, reply))
            .await
            .is_ok();
        let response_result = if requested {
            response.await.unwrap_or(Ok(()))
        } else {
            Ok(())
        };
        let task_result = match self.task.take() {
            Some(task) => match task.await {
                Ok(result) => result,
                Err(_) => Err(RuntimeError::OwnerTaskPanicked),
            },
            None => Ok(()),
        };
        let storage_result = self.storage.shutdown().await;
        response_result.and(task_result).and(storage_result)
    }
}

async fn run_workspace<T: AgentTransport + Send + 'static>(
    mut commands: mpsc::Receiver<WorkspaceCommand<T>>,
    storage: StorageHandle,
) -> Result<(), RuntimeError> {
    let mut owners = HashMap::<AgentId, AgentOwner>::new();
    while let Some(command) = commands.recv().await {
        match command {
            WorkspaceCommand::RegisterAgent(connection, transport, reply) => {
                if owners.contains_key(&connection.agent_id) {
                    let _ = reply.send(Err(RuntimeError::AgentAlreadyRegistered(
                        connection.agent_id,
                    )));
                    continue;
                }
                let agent_id = connection.agent_id;
                let result = SessionManager::connect(transport, storage.clone(), connection).await;
                match result {
                    Ok(manager) => {
                        let (owner_commands, receiver) = mpsc::channel(OWNER_CAPACITY);
                        owners.insert(
                            agent_id,
                            AgentOwner {
                                commands: owner_commands,
                                task: Some(tokio::spawn(run_owner(manager, receiver))),
                            },
                        );
                        let _ = reply.send(Ok(()));
                    }
                    Err(error) => {
                        let _ = reply.send(Err(error));
                    }
                }
            }
            WorkspaceCommand::OpenSession {
                agent_id,
                binding,
                project_root,
                resumed_at,
                reply,
            } => {
                let result = match owners.get(&agent_id) {
                    Some(owner) => {
                        open_owner_session(owner, binding, project_root, resumed_at).await
                    }
                    None => Err(RuntimeError::AgentNotRegistered(agent_id)),
                };
                let _ = reply.send(result);
            }
            WorkspaceCommand::Shutdown(stopped_at, reply) => {
                let result = shutdown_owners(&mut owners, &stopped_at).await;
                let _ = reply.send(result);
                return Ok(());
            }
        }
    }
    shutdown_owners(&mut owners, &timestamp()).await
}

async fn open_owner_session(
    owner: &AgentOwner,
    binding: SessionBinding,
    project_root: PathBuf,
    resumed_at: String,
) -> Result<RuntimeSession, RuntimeError> {
    let (events, receiver) = mpsc::channel(SESSION_EVENT_CAPACITY);
    let (reply, response) = oneshot::channel();
    owner
        .commands
        .send(OwnerCommand::OpenSession {
            binding,
            project_root,
            resumed_at,
            events,
            reply,
        })
        .await
        .map_err(|_| RuntimeError::ChannelClosed)?;
    let session = response.await.map_err(|_| RuntimeError::ChannelClosed)??;
    Ok(RuntimeSession {
        session,
        commands: owner.commands.clone(),
        events: receiver,
    })
}

async fn shutdown_owners(
    owners: &mut HashMap<AgentId, AgentOwner>,
    stopped_at: &str,
) -> Result<(), RuntimeError> {
    let mut first_error = None;
    for owner in owners.values_mut() {
        let (reply, response) = oneshot::channel();
        let requested = owner
            .commands
            .send(OwnerCommand::Shutdown(stopped_at.into(), reply))
            .await
            .is_ok();
        let response_result = if requested {
            response.await.unwrap_or(Ok(()))
        } else {
            Ok(())
        };
        let task_result = match owner.task.take() {
            Some(task) => match task.await {
                Ok(result) => result,
                Err(_) => Err(RuntimeError::OwnerTaskPanicked),
            },
            None => Ok(()),
        };
        if let Err(error) = response_result.and(task_result)
            && first_error.is_none()
        {
            first_error = Some(error);
        }
    }
    first_error.map_or(Ok(()), Err)
}

enum OwnerInput {
    Command(Option<OwnerCommand>),
    Event(Result<Option<TransportEvent>, RuntimeError>),
}

struct BindingRoute {
    session: SessionRef,
    events: mpsc::Sender<TransportEvent>,
}

struct PendingDelivery {
    event: TransportEvent,
    targets: VecDeque<(SessionBindingId, mpsc::Sender<TransportEvent>)>,
}

enum OwnerExit {
    Shutdown(String, Reply<()>),
    Closed(String),
    Failed(String, RuntimeError),
}

async fn run_owner<T: AgentTransport>(
    mut manager: SessionManager<T>,
    mut commands: mpsc::Receiver<OwnerCommand>,
) -> Result<(), RuntimeError> {
    let mut bindings: HashMap<SessionBindingId, BindingRoute> = HashMap::new();
    let mut pending: Option<PendingDelivery> = None;
    let exit = loop {
        if let Some(delivery) = pending.as_mut() {
            let Some((binding_id, events)) = delivery.targets.front().cloned() else {
                pending = None;
                continue;
            };
            let event = delivery.event.clone();
            tokio::select! {
                command = commands.recv() => {
                    if let Some(exit) = handle_owner_command(
                        command,
                        &mut manager,
                        &mut bindings,
                        &mut pending,
                    ).await {
                        break exit;
                    }
                }
                delivered = events.send(event) => {
                    if let Some(delivery) = pending.as_mut()
                        && delivery.targets.front().is_some_and(|target| target.0 == binding_id)
                    {
                        delivery.targets.pop_front();
                    }
                    if delivered.is_err() {
                        bindings.remove(&binding_id);
                    }
                }
            }
            continue;
        }

        let observed_at = timestamp();
        let input = tokio::select! {
            command = commands.recv() => OwnerInput::Command(command),
            event = manager.next_event(&observed_at) => OwnerInput::Event(event),
        };
        match input {
            OwnerInput::Command(command) => {
                if let Some(exit) =
                    handle_owner_command(command, &mut manager, &mut bindings, &mut pending).await
                {
                    break exit;
                }
            }
            OwnerInput::Event(Ok(Some(event))) => {
                if let Some(session) = event_session(&event) {
                    let is_owned = manager.owns_session(session);
                    if is_owned {
                        if let TransportEvent::PermissionRequested(request) = &event {
                            manager.track_permission(request.clone());
                        }
                        if let TransportEvent::SessionLost { session } = &event
                            && let Err(error) = manager
                                .mark_session_lost(session, observed_at.clone())
                                .await
                        {
                            break OwnerExit::Failed(timestamp(), error);
                        }
                    }
                    let route = bindings.get(&session.binding_id);
                    if let Some(route) = route {
                        let target = (session.binding_id, route.events.clone());
                        if route.session != *session {
                            continue;
                        }
                        pending = Some(PendingDelivery {
                            event,
                            targets: VecDeque::from([target]),
                        });
                    }
                } else if matches!(event, TransportEvent::TransportDisconnected { .. }) {
                    let targets: VecDeque<_> = bindings
                        .iter()
                        .map(|(binding_id, route)| (*binding_id, route.events.clone()))
                        .collect();
                    if !targets.is_empty() {
                        pending = Some(PendingDelivery { event, targets });
                    }
                }
            }
            OwnerInput::Event(Ok(None)) => break OwnerExit::Closed(timestamp()),
            OwnerInput::Event(Err(error)) => break OwnerExit::Failed(timestamp(), error),
        }
    };

    match exit {
        OwnerExit::Shutdown(stopped_at, reply) => {
            let _ = reply.send(manager.shutdown(stopped_at).await);
            Ok(())
        }
        OwnerExit::Closed(stopped_at) => manager.shutdown(stopped_at).await,
        OwnerExit::Failed(stopped_at, error) => {
            let _ = manager.shutdown(stopped_at).await;
            Err(error)
        }
    }
}

async fn handle_owner_command<T: AgentTransport>(
    command: Option<OwnerCommand>,
    manager: &mut SessionManager<T>,
    bindings: &mut HashMap<SessionBindingId, BindingRoute>,
    pending: &mut Option<PendingDelivery>,
) -> Option<OwnerExit> {
    match command {
        Some(OwnerCommand::OpenSession {
            binding,
            project_root,
            resumed_at,
            events,
            reply,
        }) => {
            if bindings.contains_key(&binding.id) {
                let _ = reply.send(Err(RuntimeError::SessionBindingAlreadyAttached(binding.id)));
                return None;
            }
            let result = if binding.remote_session_id.is_some() {
                manager
                    .resume_session(&binding, project_root, resumed_at)
                    .await
            } else {
                manager.create_session(binding, project_root).await
            };
            if let Ok(session) = &result {
                bindings.insert(
                    session.binding_id,
                    BindingRoute {
                        session: session.clone(),
                        events,
                    },
                );
            }
            let _ = reply.send(result);
        }
        Some(OwnerCommand::SendMessage(request, reply)) => {
            let result = require_session(bindings, &request.session);
            let _ = reply.send(match result {
                Ok(()) => manager.send_message(request.session, request.content).await,
                Err(error) => Err(error),
            });
        }
        Some(OwnerCommand::CancelTurn(session, cancelled_at, reply)) => {
            let result = require_session(bindings, &session);
            let _ = reply.send(match result {
                Ok(()) => manager.cancel_turn(session, cancelled_at).await,
                Err(error) => Err(error),
            });
        }
        Some(OwnerCommand::RespondPermission(response, decided_at, reply)) => {
            let result = require_session(bindings, &response.session);
            let _ = reply.send(match result {
                Ok(()) => manager.respond_permission(response, decided_at).await,
                Err(error) => Err(error),
            });
        }
        Some(OwnerCommand::Detach(session, detached_at, reply)) => {
            let result = match require_session(bindings, &session) {
                Ok(()) => manager.detach_session(&session, detached_at).await,
                Err(error) => Err(error),
            };
            if result.is_ok() {
                bindings.remove(&session.binding_id);
                if let Some(delivery) = pending {
                    delivery
                        .targets
                        .retain(|target| target.0 != session.binding_id);
                }
            }
            let _ = reply.send(result);
        }
        Some(OwnerCommand::Shutdown(stopped_at, reply)) => {
            return Some(OwnerExit::Shutdown(stopped_at, reply));
        }
        None => return Some(OwnerExit::Closed(timestamp())),
    }
    None
}

fn require_session(
    bindings: &HashMap<SessionBindingId, BindingRoute>,
    session: &SessionRef,
) -> Result<(), RuntimeError> {
    match bindings.get(&session.binding_id) {
        Some(route) if route.session == *session => Ok(()),
        _ => Err(RuntimeError::SessionBindingNotFound(session.binding_id)),
    }
}

fn event_session(event: &TransportEvent) -> Option<&SessionRef> {
    match event {
        TransportEvent::TurnStarted { session }
        | TransportEvent::AgentTextDelta { session, .. }
        | TransportEvent::AgentMessageCompleted { session }
        | TransportEvent::ToolCallStarted { session, .. }
        | TransportEvent::ToolCallFinished { session, .. }
        | TransportEvent::TurnCompleted { session }
        | TransportEvent::TurnFailed { session, .. }
        | TransportEvent::UsageReported { session, .. }
        | TransportEvent::SessionLost { session } => Some(session),
        TransportEvent::PermissionRequested(request) => Some(&request.session),
        TransportEvent::TransportDisconnected { .. } => None,
    }
}
