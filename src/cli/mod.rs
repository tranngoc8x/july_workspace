use crate::application::{
    AddAgent, AddRoomMember, AddThreadMember, AgentRef, ChatEvent, ChatFailureKind,
    ChatPermissionRequestId, CollaborationError, CollaborationService, CreateRoom, CreateThread,
    DirectMessageError, DirectMessageRuntime, DirectMessageService, MembershipChange,
    MembershipState, OpenThreadForAgent, PublishError, PublishResult, PublishService,
    RemoveRoomMember, RemoveThreadMember, RoomRef, ThreadChatRuntime, ThreadChatService,
};
use crate::domain::{
    AgentId, ConversationId, MemberType, PermissionOption, PermissionOutcome, PublishId, ResultId,
    RoomId, WorkItemId,
};
use crate::runtime::{
    AgentDirectMessageRuntime, AgentThreadRuntime, DirectMessageBootstrapError, StorageWorker,
    WorkspaceRuntime, open_acp_direct_message, register_acp_agent,
};
use crate::transport::AcpTransport;
use chrono::{SecondsFormat, Utc};
use serde_json::json;
use std::collections::HashSet;
use std::ffi::OsString;
use std::fmt;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::str::FromStr;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, BufReader, Lines, Stdin};
use tokio::sync::mpsc;

// ponytail: no caller yet - Task 8 (onboarding) wires this up next. Remove
// once that lands and the dead_code lint finds a real caller on its own.
mod init;
#[allow(dead_code)]
mod keys;
pub mod registry;

use registry::CommandScope;

const LOCAL_USER_ID: &str = "local-user";
const USAGE: &str = "usage: july dm <agent>";
const NO_AGENTS: &str = "no agents configured; add one with: \
                         july agent add <name> --project <path> --runtime <runtime>";

#[derive(Debug, Error)]
pub enum CliError {
    #[error("{USAGE}")]
    Usage,
    #[error("environment variable HOME is not set")]
    MissingHome,
    #[error("agent name must be valid UTF-8")]
    InvalidAgentName,
    #[error("command arguments must be valid UTF-8")]
    InvalidUtf8,
    #[error("invalid command")]
    InvalidCommand,
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Bootstrap(#[from] DirectMessageBootstrapError),
    #[error(transparent)]
    DirectMessage(#[from] DirectMessageError),
    #[error(transparent)]
    Collaboration(#[from] CollaborationError),
    #[error(transparent)]
    Publish(#[from] PublishError),
    #[error("runtime error: {0}")]
    Runtime(String),
    #[error("agent turn failed: {0}")]
    TurnFailed(&'static str),
    #[error("agent transport disconnected: {0}")]
    Disconnected(String),
    #[error("agent event stream closed")]
    EventStreamClosed,
    #[error("{operation}; storage shutdown failed: {shutdown}")]
    OperationAndShutdown {
        operation: Box<CliError>,
        shutdown: String,
    },
    #[error("{operation}; context shutdown failed: {shutdown}")]
    OperationAndContextShutdown {
        operation: Box<CliError>,
        shutdown: String,
    },
    #[error("{operation}; context restore failed: {restore}")]
    OperationAndRestore {
        operation: Box<CliError>,
        restore: String,
    },
    #[error("{0}")]
    Json(String),
}

pub async fn run(args: impl IntoIterator<Item = OsString>) -> Result<(), CliError> {
    let mut args = args.into_iter();
    let _program = args.next();
    let args: Vec<OsString> = args.collect();
    if args.len() == 2 && args[0] == "dm" && args[1].to_str().is_none() {
        return Err(CliError::InvalidAgentName);
    }
    let json_requested = args.iter().any(|arg| arg == "--json");
    let args: Vec<String> = args
        .into_iter()
        .map(|arg| arg.into_string().map_err(|_| CliError::InvalidUtf8))
        .collect::<Result<_, _>>()
        .map_err(|error| error.json_if_requested(json_requested))?;
    let command = parse_command(args).map_err(|error| error.json_if_requested(json_requested))?;
    let json = command.json();
    let result = match command {
        Command::Repl => run_repl().await,
        Command::Version { json } => run_version(json),
        Command::Dm(agent_name) => run_dm(agent_name).await,
        Command::ThreadOpen { thread_id, agent } => run_thread_open(thread_id, agent).await,
        Command::Agent { operation, .. } => run_agent(operation, json).await,
        Command::Room { operation, .. } => run_room(operation, json).await,
        Command::Thread { operation, .. } => run_thread(operation, json).await,
        Command::Publish {
            result_id,
            target_conversation_id,
            ..
        } => run_publish(result_id, target_conversation_id, json).await,
    };
    result.map_err(|error| error.json_if_requested(json))
}

impl CliError {
    fn json_if_requested(self, json_requested: bool) -> Self {
        if !json_requested || matches!(&self, Self::Json(_)) {
            return self;
        }
        Self::Json(
            json!({ "error": {
            "code": self.error_code(), "message": self.to_string()
        } })
            .to_string(),
        )
    }

    fn error_code(&self) -> &'static str {
        match self {
            Self::Usage | Self::InvalidAgentName | Self::InvalidUtf8 => "usage",
            Self::InvalidCommand => "invalid_command",
            Self::Io(_) => "io_error",
            Self::Runtime(_) | Self::Bootstrap(_) | Self::DirectMessage(_) => "runtime_error",
            Self::Collaboration(error) => match error {
                CollaborationError::RoomNotFound(_) => "room_not_found",
                CollaborationError::AgentNotFound(_) => "agent_not_found",
                CollaborationError::RoomIdConflict(_) => "room_id_conflict",
                CollaborationError::RoomNameConflict(_) => "room_name_conflict",
                CollaborationError::AgentNameConflict(_) => "agent_name_conflict",
                CollaborationError::RoomInactive(_) => "room_inactive",
                CollaborationError::AgentInactive(_) => "agent_inactive",
                CollaborationError::RoomRemovalBlocked { .. } => "room_removal_blocked",
                CollaborationError::ThreadNotFound(_) => "thread_not_found",
                CollaborationError::ThreadIdConflict(_) => "thread_id_conflict",
                CollaborationError::PrimaryWorkIdConflict(_) => "primary_work_id_conflict",
                CollaborationError::ThreadNotOpen(_) => "thread_not_open",
                CollaborationError::RoomMembershipRequired { .. } => "room_membership_required",
                CollaborationError::ThreadMembershipRequired { .. } => "thread_membership_required",
                CollaborationError::InvalidCommand(_) => "invalid_command",
                _ => "runtime_error",
            },
            Self::Publish(error) => match error {
                PublishError::ResultNotFound(_) => "result_not_found",
                PublishError::WorkNotFound(_) => "work_not_found",
                PublishError::SourceNotFound(_) => "source_not_found",
                PublishError::TargetNotFound(_) => "target_not_found",
                PublishError::PublishIdConflict(_) => "publish_id_conflict",
                PublishError::InvalidTimestamp => "invalid_timestamp",
                PublishError::NoTarget(_) => "publish_target_missing",
                PublishError::AmbiguousTarget { .. } => "publish_target_ambiguous",
                PublishError::Runtime(_) => "runtime_error",
            },
            Self::MissingHome
            | Self::TurnFailed(_)
            | Self::Disconnected(_)
            | Self::EventStreamClosed => "runtime_error",
            Self::OperationAndShutdown { operation, .. }
            | Self::OperationAndContextShutdown { operation, .. }
            | Self::OperationAndRestore { operation, .. } => operation.error_code(),
            Self::Json(_) => unreachable!(),
        }
    }
}

enum Command {
    Repl,
    Version {
        json: bool,
    },
    Dm(String),
    ThreadOpen {
        thread_id: ConversationId,
        agent: AgentRef,
    },
    Agent {
        operation: AgentOperation,
        json: bool,
    },
    Room {
        operation: RoomOperation,
        json: bool,
    },
    Thread {
        operation: ThreadOperation,
        json: bool,
    },
    Publish {
        result_id: ResultId,
        target_conversation_id: ConversationId,
        json: bool,
    },
}

#[derive(Clone)]
enum ReplContext {
    Root,
    Room(RoomId),
    Dm {
        conversation_id: ConversationId,
        agent_id: AgentId,
        agent_name: String,
    },
    Thread {
        conversation_id: ConversationId,
        room_id: RoomId,
        agent_id: AgentId,
        agent_name: String,
    },
}

impl ReplContext {
    fn scope(&self) -> CommandScope {
        match self {
            Self::Root => CommandScope::Root,
            Self::Room(_) => CommandScope::Room,
            Self::Dm { .. } => CommandScope::Dm,
            Self::Thread { .. } => CommandScope::Thread,
        }
    }

    /// Publish targets a Conversation; a Room is not one.
    fn conversation_id(&self) -> Option<ConversationId> {
        match self {
            Self::Root | Self::Room(_) => None,
            Self::Dm {
                conversation_id, ..
            }
            | Self::Thread {
                conversation_id, ..
            } => Some(*conversation_id),
        }
    }

    /// The Room a Thread is created in; a Thread inherits its parent Room.
    fn room_id(&self) -> Option<RoomId> {
        match self {
            Self::Room(room_id) | Self::Thread { room_id, .. } => Some(*room_id),
            Self::Root | Self::Dm { .. } => None,
        }
    }
}

type ReplDirectMessage = DirectMessageService<AgentDirectMessageRuntime<AcpTransport>>;
type ReplThreadChat = ThreadChatService<AgentThreadRuntime<AcpTransport>>;

/// One interactive context at a time; lower stack entries stay cold descriptors.
enum ReplChat {
    Dm(Box<ReplDirectMessage>),
    Thread(Box<ReplThreadChat>),
}

/// Presentation-side view of an open DM or Thread turn.
#[allow(async_fn_in_trait)]
trait ChatContext {
    async fn send(&mut self, body: String, sent_at: String) -> Result<(), CliError>;
    async fn next(&mut self, observed_at: String) -> Result<Option<ChatEvent>, CliError>;
    async fn permit(
        &mut self,
        request_id: ChatPermissionRequestId,
        outcome: PermissionOutcome,
        decided_at: String,
    ) -> Result<(), CliError>;
    async fn cancel(&mut self, cancelled_at: String) -> Result<(), CliError>;
    async fn stop(&mut self, stopped_at: String) -> Result<(), CliError>;
}

impl<R: DirectMessageRuntime> ChatContext for DirectMessageService<R> {
    async fn send(&mut self, body: String, sent_at: String) -> Result<(), CliError> {
        Ok(self.send_message(body, sent_at).await?)
    }

    async fn next(&mut self, observed_at: String) -> Result<Option<ChatEvent>, CliError> {
        Ok(self.next_event(observed_at).await?)
    }

    async fn permit(
        &mut self,
        request_id: ChatPermissionRequestId,
        outcome: PermissionOutcome,
        decided_at: String,
    ) -> Result<(), CliError> {
        Ok(self
            .respond_permission(request_id, outcome, decided_at)
            .await?)
    }

    async fn cancel(&mut self, cancelled_at: String) -> Result<(), CliError> {
        Ok(self.cancel_turn(cancelled_at).await?)
    }

    async fn stop(&mut self, stopped_at: String) -> Result<(), CliError> {
        Ok(self.shutdown(stopped_at).await?)
    }
}

impl<R: ThreadChatRuntime> ChatContext for ThreadChatService<R> {
    async fn send(&mut self, body: String, sent_at: String) -> Result<(), CliError> {
        Ok(self.send_message(body, sent_at).await?)
    }

    async fn next(&mut self, observed_at: String) -> Result<Option<ChatEvent>, CliError> {
        Ok(self.next_event(observed_at).await?)
    }

    async fn permit(
        &mut self,
        request_id: ChatPermissionRequestId,
        outcome: PermissionOutcome,
        decided_at: String,
    ) -> Result<(), CliError> {
        Ok(self
            .respond_permission(request_id, outcome, decided_at)
            .await?)
    }

    async fn cancel(&mut self, cancelled_at: String) -> Result<(), CliError> {
        Ok(self.cancel_turn(cancelled_at).await?)
    }

    async fn stop(&mut self, stopped_at: String) -> Result<(), CliError> {
        Ok(self.shutdown(stopped_at).await?)
    }
}

impl ChatContext for ReplChat {
    async fn send(&mut self, body: String, sent_at: String) -> Result<(), CliError> {
        match self {
            Self::Dm(dm) => dm.send(body, sent_at).await,
            Self::Thread(thread) => thread.send(body, sent_at).await,
        }
    }

    async fn next(&mut self, observed_at: String) -> Result<Option<ChatEvent>, CliError> {
        match self {
            Self::Dm(dm) => dm.next(observed_at).await,
            Self::Thread(thread) => thread.next(observed_at).await,
        }
    }

    async fn permit(
        &mut self,
        request_id: ChatPermissionRequestId,
        outcome: PermissionOutcome,
        decided_at: String,
    ) -> Result<(), CliError> {
        match self {
            Self::Dm(dm) => dm.permit(request_id, outcome, decided_at).await,
            Self::Thread(thread) => thread.permit(request_id, outcome, decided_at).await,
        }
    }

    async fn cancel(&mut self, cancelled_at: String) -> Result<(), CliError> {
        match self {
            Self::Dm(dm) => dm.cancel(cancelled_at).await,
            Self::Thread(thread) => thread.cancel(cancelled_at).await,
        }
    }

    async fn stop(&mut self, stopped_at: String) -> Result<(), CliError> {
        match self {
            Self::Dm(dm) => dm.stop(stopped_at).await,
            Self::Thread(thread) => thread.stop(stopped_at).await,
        }
    }
}

impl Command {
    fn json(&self) -> bool {
        matches!(
            self,
            Self::Version { json: true }
                | Self::Agent { json: true, .. }
                | Self::Room { json: true, .. }
                | Self::Thread { json: true, .. }
                | Self::Publish { json: true, .. }
        )
    }
}

/// Agent lifecycle is administrative: it configures identity, never sessions.
enum AgentOperation {
    Add {
        name: String,
        project: String,
        runtime: Option<String>,
        transport: String,
        config: Option<PathBuf>,
    },
    List,
    Show(AgentRef),
    Remove(AgentRef),
}

enum RoomOperation {
    Create {
        name: String,
        description: Option<String>,
    },
    List,
    Members(RoomRef),
    AddMember {
        room: RoomRef,
        agent: AgentRef,
    },
    RemoveMember {
        room: RoomRef,
        agent: AgentRef,
    },
}

enum ThreadOperation {
    Create {
        title: String,
        room: RoomRef,
        goal: Option<String>,
        members: Vec<AgentRef>,
    },
    List(RoomRef),
    Members(ConversationId),
    AddMember {
        thread_id: ConversationId,
        agent: AgentRef,
    },
    RemoveMember {
        thread_id: ConversationId,
        agent: AgentRef,
    },
}

fn parse_command(mut args: Vec<String>) -> Result<Command, CliError> {
    let json = remove_json(&mut args)?;
    match args.first().map(String::as_str) {
        Some("dm") => match args.as_slice() {
            [_, agent] if !json => Ok(Command::Dm(positional(agent)?)),
            _ => Err(CliError::Usage),
        },
        Some("agent") => parse_agent(args, json),
        Some("room") => parse_room(args, json),
        Some("thread") => parse_thread(args, json),
        Some("publish") => parse_publish(args, json),
        Some("--version") | Some("-V") if args.len() == 1 => Ok(Command::Version { json }),
        Some(command) if command.starts_with("--") => Err(CliError::Usage),
        Some(_) => Err(CliError::InvalidCommand),
        None if json => Err(CliError::Usage),
        None => Ok(Command::Repl),
    }
}

fn parse_publish(args: Vec<String>, json: bool) -> Result<Command, CliError> {
    match args.as_slice() {
        [_, result, flag, target] if flag == "--to" => Ok(Command::Publish {
            result_id: result_id(result)?,
            target_conversation_id: thread_id(target)?,
            json,
        }),
        _ => Err(CliError::Usage),
    }
}

fn parse_thread(args: Vec<String>, json: bool) -> Result<Command, CliError> {
    let operation = match args.as_slice() {
        // `thread open` streams an interactive session, so it never frames JSON.
        [_, command, thread, flag, agent] if command == "open" && flag == "--agent" && !json => {
            return Ok(Command::ThreadOpen {
                thread_id: thread_id(thread)?,
                agent: agent_ref(agent)?,
            });
        }
        [_, command, title, rest @ ..] if command == "create" => {
            let (room, goal, members) = parse_thread_create(rest)?;
            ThreadOperation::Create {
                title: positional(title)?,
                room,
                goal,
                members,
            }
        }
        [_, command, rest @ ..] if command == "list" => {
            ThreadOperation::List(parse_thread_room(rest)?)
        }
        [_, command, thread] if command == "members" => {
            ThreadOperation::Members(thread_id(thread)?)
        }
        [_, member, action, thread, agent] if member == "member" && action == "add" => {
            ThreadOperation::AddMember {
                thread_id: thread_id(thread)?,
                agent: agent_ref(agent)?,
            }
        }
        [_, member, action, thread, agent] if member == "member" && action == "remove" => {
            ThreadOperation::RemoveMember {
                thread_id: thread_id(thread)?,
                agent: agent_ref(agent)?,
            }
        }
        _ if matches!(
            args.get(1).map(String::as_str),
            Some("create" | "list" | "members" | "member" | "open")
        ) =>
        {
            return Err(CliError::Usage);
        }
        _ => return Err(CliError::InvalidCommand),
    };
    Ok(Command::Thread { operation, json })
}

fn parse_thread_room(args: &[String]) -> Result<RoomRef, CliError> {
    match args {
        [flag, room] if flag == "--room" => room_ref(room),
        _ => Err(CliError::Usage),
    }
}

fn parse_thread_create(
    args: &[String],
) -> Result<(RoomRef, Option<String>, Vec<AgentRef>), CliError> {
    let mut room = None;
    let mut goal = None;
    let mut members = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let Some(value) = args.get(index + 1) else {
            return Err(CliError::Usage);
        };
        match args[index].as_str() {
            "--room" if room.is_none() => room = Some(room_ref(value)?),
            "--goal" if goal.is_none() && !value.starts_with("--") => goal = Some(value.clone()),
            "--member" => members.push(agent_ref(value)?),
            _ => return Err(CliError::Usage),
        }
        index += 2;
    }
    room.map(|room| (room, goal, members))
        .ok_or(CliError::Usage)
}

fn remove_json(args: &mut Vec<String>) -> Result<bool, CliError> {
    let count = args.iter().filter(|arg| arg.as_str() == "--json").count();
    if count > 1 {
        return Err(CliError::Usage);
    }
    if count == 1 {
        args.retain(|arg| arg != "--json");
    }
    Ok(count == 1)
}

fn parse_agent(args: Vec<String>, json: bool) -> Result<Command, CliError> {
    let operation = match args.as_slice() {
        [_, command] if command == "list" => AgentOperation::List,
        [_, command, agent] if command == "show" => AgentOperation::Show(agent_ref(agent)?),
        [_, command, agent] if command == "remove" => AgentOperation::Remove(agent_ref(agent)?),
        [_, command, name, rest @ ..] if command == "add" => {
            let (project, runtime, transport, config) = parse_agent_add(rest)?;
            AgentOperation::Add {
                name: positional(name)?,
                project,
                runtime,
                transport,
                config,
            }
        }
        _ if matches!(
            args.get(1).map(String::as_str),
            Some("add" | "list" | "show" | "remove")
        ) =>
        {
            return Err(CliError::Usage);
        }
        _ => return Err(CliError::InvalidCommand),
    };
    Ok(Command::Agent { operation, json })
}

/// `--project` is required; `--runtime` is a preference, `--adapter` picks the
/// transport, and `--config` supplies its connection details.
fn parse_agent_add(
    args: &[String],
) -> Result<(String, Option<String>, String, Option<PathBuf>), CliError> {
    let mut project = None;
    let mut runtime = None;
    let mut transport = None;
    let mut config = None;
    let mut index = 0;
    while index < args.len() {
        let Some(value) = args.get(index + 1).filter(|value| !value.starts_with("--")) else {
            return Err(CliError::Usage);
        };
        match args[index].as_str() {
            "--project" if project.is_none() => project = Some(value.clone()),
            "--runtime" if runtime.is_none() => runtime = Some(value.clone()),
            "--adapter" if transport.is_none() => transport = Some(value.clone()),
            "--config" if config.is_none() => config = Some(PathBuf::from(value)),
            _ => return Err(CliError::Usage),
        }
        index += 2;
    }
    let project = project.ok_or(CliError::Usage)?;
    Ok((
        project,
        runtime,
        transport.unwrap_or_else(|| "acp".into()),
        config,
    ))
}

fn parse_room(args: Vec<String>, json: bool) -> Result<Command, CliError> {
    let operation = match args.as_slice() {
        [_, command] if command == "list" => RoomOperation::List,
        [_, command, room] if command == "members" => RoomOperation::Members(room_ref(room)?),
        [_, command, name, rest @ ..] if command == "create" => RoomOperation::Create {
            name: positional(name)?,
            description: parse_description(rest)?,
        },
        [_, member, action, room, agent] if member == "member" && action == "add" => {
            RoomOperation::AddMember {
                room: room_ref(room)?,
                agent: agent_ref(agent)?,
            }
        }
        [_, member, action, room, agent] if member == "member" && action == "remove" => {
            RoomOperation::RemoveMember {
                room: room_ref(room)?,
                agent: agent_ref(agent)?,
            }
        }
        _ if matches!(
            args.get(1).map(String::as_str),
            Some("create" | "list" | "members" | "member")
        ) =>
        {
            return Err(CliError::Usage);
        }
        _ => return Err(CliError::InvalidCommand),
    };
    Ok(Command::Room { operation, json })
}

fn positional(value: &str) -> Result<String, CliError> {
    if value.starts_with("--") {
        return Err(CliError::Usage);
    }
    Ok(value.into())
}

fn parse_description(args: &[String]) -> Result<Option<String>, CliError> {
    match args {
        [] => Ok(None),
        [flag, description] if flag == "--description" && !description.starts_with("--") => {
            Ok(Some(description.clone()))
        }
        _ => Err(CliError::Usage),
    }
}

fn room_ref(value: &str) -> Result<RoomRef, CliError> {
    if value.starts_with("--") {
        return Err(CliError::Usage);
    }
    if let Ok(id) = RoomId::from_str(value)
        && id.to_string() == value
    {
        return Ok(RoomRef::Id(id));
    }
    Ok(RoomRef::Name(value.into()))
}

fn agent_ref(value: &str) -> Result<AgentRef, CliError> {
    if value.starts_with("--") {
        return Err(CliError::Usage);
    }
    if let Ok(id) = AgentId::from_str(value)
        && id.to_string() == value
    {
        return Ok(AgentRef::Id(id));
    }
    Ok(AgentRef::Name(value.into()))
}

fn thread_id(value: &str) -> Result<ConversationId, CliError> {
    let id = ConversationId::from_str(value).map_err(|_| CliError::Usage)?;
    (id.to_string() == value)
        .then_some(id)
        .ok_or(CliError::Usage)
}

fn result_id(value: &str) -> Result<ResultId, CliError> {
    let id = ResultId::from_str(value).map_err(|_| CliError::Usage)?;
    (id.to_string() == value)
        .then_some(id)
        .ok_or(CliError::Usage)
}

async fn run_repl() -> Result<(), CliError> {
    let database = database_path()?;
    if let Some(parent) = database
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let worker =
        StorageWorker::open(&database).map_err(|error| CliError::Runtime(error.to_string()))?;
    let mut workspace =
        WorkspaceRuntime::new(worker).map_err(|error| CliError::Runtime(error.to_string()))?;
    let mut service = CollaborationService::new(workspace.storage());
    let interaction = interact_repl(&mut service, &workspace).await;
    let shutdown = workspace
        .shutdown(timestamp())
        .await
        .map_err(|error| CliError::Runtime(error.to_string()));
    match (interaction, shutdown) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(operation), Ok(())) => Err(operation),
        (Ok(()), Err(shutdown)) => Err(shutdown),
        (Err(operation), Err(shutdown)) => Err(CliError::OperationAndShutdown {
            operation: Box::new(operation),
            shutdown: shutdown.to_string(),
        }),
    }
}

async fn interact_repl<R: crate::application::CollaborationRuntime>(
    service: &mut CollaborationService<R>,
    workspace: &WorkspaceRuntime<AcpTransport>,
) -> Result<(), CliError> {
    let mut live = None;
    let interaction = interact_repl_loop(service, workspace, &mut live).await;
    let shutdown = close_repl_context(&mut live).await;
    match (interaction, shutdown) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(operation), Ok(())) => Err(operation),
        (Ok(()), Err(shutdown)) => Err(shutdown),
        (Err(operation), Err(shutdown)) => Err(CliError::OperationAndContextShutdown {
            operation: Box::new(operation),
            shutdown: shutdown.to_string(),
        }),
    }
}

async fn interact_repl_loop<R: crate::application::CollaborationRuntime>(
    service: &mut CollaborationService<R>,
    workspace: &WorkspaceRuntime<AcpTransport>,
    live: &mut Option<ReplChat>,
) -> Result<(), CliError> {
    let mut input = repl_input();
    let mut contexts = vec![ReplContext::Root];
    let mut registered = HashSet::new();
    let mut publish = PublishService::new(workspace.storage());
    loop {
        repl_stdout(format_args!("> "))?;
        let line = tokio::select! {
            line = input.recv() => match line {
                Some(line) => line?,
                None => {
                    close_repl_context(live).await?;
                    return Ok(());
                }
            },
            result = tokio::signal::ctrl_c() => {
                result?;
                close_repl_context(live).await?;
                return Ok(());
            }
        };
        let Some(line) = line else {
            close_repl_context(live).await?;
            return Ok(());
        };
        let scope = contexts.last().unwrap().scope();
        let Some((spec, arguments)) = registry::resolve(&line) else {
            // Not a registered command: chat sends it, other contexts reject it.
            if line.trim().is_empty() {
                continue;
            }
            if matches!(
                contexts.last(),
                Some(ReplContext::Dm { .. } | ReplContext::Thread { .. })
            ) {
                let chat = live.as_mut().expect("active context has a live service");
                if let Err(error) = chat.send(line, timestamp()).await {
                    repl_stderr(format_args!("{error}\n"))?;
                    continue;
                }
                drain_repl_turn(chat, &mut input).await?;
            } else {
                repl_stderr(format_args!("{}\n", CliError::InvalidCommand))?;
            }
            continue;
        };
        if !spec.available_in(scope) {
            repl_stderr(format_args!("{}\n", spec.scope_error(scope)))?;
            continue;
        }
        let arguments = arguments.to_owned();
        let arguments = arguments.as_str();
        match spec.name {
            "/exit" if arguments.is_empty() => {
                close_repl_context(live).await?;
                return Ok(());
            }
            "/help" if arguments.is_empty() => {
                repl_stdout(format_args!("{}", registry::help(scope)))?
            }
            "/help" => match registry::find(arguments) {
                Some(spec) => repl_stdout(format_args!("{}", registry::help_command(spec)))?,
                None => repl_stderr(format_args!("unknown command: {arguments}\n"))?,
            },
            "/status" if arguments.is_empty() => {
                print_repl_status(service, workspace, contexts.last().unwrap()).await?
            }
            "/rooms" if arguments.is_empty() => match service.list_rooms().await {
                Ok(rooms) => {
                    for room in rooms {
                        repl_stdout(format_args!(
                            "{}\t{}\t{}\n",
                            room.id, room.name, room.status
                        ))?;
                    }
                }
                Err(error) => repl_stderr(format_args!("{error}\n"))?,
            },
            "/agents" if arguments.is_empty() => match service.list_agents().await {
                Ok(agents) if agents.is_empty() => repl_stderr(format_args!("{NO_AGENTS}\n"))?,
                Ok(agents) => {
                    for agent in agents {
                        repl_stdout(format_args!(
                            "{}\t{}\t{}\t{}\t{}\n",
                            agent.id,
                            agent.name,
                            agent.project_root,
                            agent.transport_type,
                            agent.status
                        ))?;
                    }
                }
                Err(error) => repl_stderr(format_args!("{error}\n"))?,
            },
            "/work" if arguments.is_empty() => {
                let conversation_id = contexts
                    .last()
                    .unwrap()
                    .conversation_id()
                    .expect("thread scope owns a conversation");
                match service.list_work_items(conversation_id).await {
                    Ok(work_items) => {
                        for work in work_items {
                            repl_stdout(format_args!(
                                "{}\t{}\t{}\t{}\n",
                                work.id,
                                work.status,
                                work.title,
                                work.owner_agent_id
                                    .map(|agent| agent.to_string())
                                    .unwrap_or_default(),
                            ))?;
                        }
                    }
                    Err(error) => repl_stderr(format_args!("{error}\n"))?,
                }
            }
            "/results" if arguments.is_empty() => {
                let conversation_id = contexts
                    .last()
                    .unwrap()
                    .conversation_id()
                    .expect("thread scope owns a conversation");
                match service.list_work_results(conversation_id).await {
                    Ok(results) => {
                        for result in results {
                            repl_stdout(format_args!(
                                "{}\t{}\t{}\t{}\n",
                                result.id, result.work_id, result.status, result.summary,
                            ))?;
                        }
                    }
                    Err(error) => repl_stderr(format_args!("{error}\n"))?,
                }
            }
            "/restart" if arguments.is_empty() => {
                if let Err(error) = close_repl_context(live).await {
                    repl_stderr(format_args!("{error}\n"))?;
                    continue;
                }
                if let Err(error) = restore_repl_context(service, workspace, &contexts, live).await
                {
                    repl_stderr(format_args!("{error}\n"))?;
                    continue;
                }
                print_repl_status(service, workspace, contexts.last().unwrap()).await?
            }
            "/thread new" if !arguments.is_empty() => {
                let room_id = contexts
                    .last()
                    .unwrap()
                    .room_id()
                    .expect("room and thread scopes own a room");
                match parse_thread_new(arguments) {
                    Ok((title, goal)) => {
                        match service
                            .create_thread(CreateThread {
                                thread_id: ConversationId::new(),
                                primary_work_id: WorkItemId::new(),
                                room: RoomRef::Id(room_id),
                                title,
                                goal,
                                user_id: LOCAL_USER_ID.into(),
                                initial_agents: Vec::new(),
                                created_at: timestamp(),
                            })
                            .await
                        {
                            Ok(thread) => repl_stdout(format_args!(
                                "thread\t{}\t{}\n",
                                thread.thread_id, thread.primary_work_id
                            ))?,
                            Err(error) => repl_stderr(format_args!("{error}\n"))?,
                        }
                    }
                    Err(error) => repl_stderr(format_args!("{error}\n"))?,
                }
            }
            "/back" if contexts.len() == 1 => repl_stderr(format_args!("already at root\n"))?,
            "/back" if arguments.is_empty() => {
                let previous = contexts.last().unwrap().clone();
                if let Err(error) = close_repl_context(live).await {
                    repl_stderr(format_args!("{error}\n"))?;
                    continue;
                }
                contexts.pop();
                if let Err(error) = restore_repl_context(service, workspace, &contexts, live).await
                {
                    contexts.push(previous);
                    if let Err(restore) =
                        restore_repl_context(service, workspace, &contexts, live).await
                    {
                        return Err(CliError::OperationAndRestore {
                            operation: Box::new(error),
                            restore: restore.to_string(),
                        });
                    }
                    repl_stderr(format_args!("{error}\n"))?;
                    continue;
                }
                print_repl_status(service, workspace, contexts.last().unwrap()).await?;
            }
            "/members" if arguments.is_empty() => match contexts.last().unwrap() {
                ReplContext::Room(room_id) => {
                    match service.list_room_members(RoomRef::Id(*room_id)).await {
                        Ok(members) => {
                            let output = render_room_members(
                                members
                                    .into_iter()
                                    .filter(|member| member.left_at.is_none())
                                    .collect(),
                                false,
                            );
                            if !output.is_empty() {
                                repl_stdout(format_args!("{output}\n"))?;
                            }
                        }
                        Err(error) => repl_stderr(format_args!("{error}\n"))?,
                    }
                }
                ReplContext::Thread {
                    conversation_id, ..
                } => match service.list_thread_members(*conversation_id).await {
                    Ok(members) => {
                        let output = render_thread_members(
                            members
                                .into_iter()
                                .filter(|member| member.left_at.is_none())
                                .collect(),
                            false,
                        );
                        if !output.is_empty() {
                            repl_stdout(format_args!("{output}\n"))?;
                        }
                    }
                    Err(error) => repl_stderr(format_args!("{error}\n"))?,
                },
                ReplContext::Root | ReplContext::Dm { .. } => {
                    unreachable!("registry scopes /members to room and thread")
                }
            },
            "/room" if !arguments.is_empty() => match room_ref(arguments) {
                Ok(reference) => match service.resolve_room(reference).await {
                    Ok(room) => {
                        if let Err(error) = close_repl_context(live).await {
                            repl_stderr(format_args!("{error}\n"))?;
                            continue;
                        }
                        repl_stdout(format_args!("room\t{}\t{}\n", room.id, room.name))?;
                        contexts.push(ReplContext::Room(room.id));
                    }
                    Err(error) => repl_stderr(format_args!("{error}\n"))?,
                },
                Err(error) => repl_stderr(format_args!("{error}\n"))?,
            },
            "/publish" if !arguments.is_empty() => {
                let source_conversation_id = contexts
                    .last()
                    .unwrap()
                    .conversation_id()
                    .expect("thread scope owns a conversation");
                let fields: Vec<_> = arguments.split_whitespace().collect();
                let (result, target) = match fields.as_slice() {
                    [result] => (*result, None),
                    [result, flag, target] if *flag == "--to" => (*result, Some(*target)),
                    _ => {
                        repl_stderr(format_args!("{}\n", CliError::InvalidCommand))?;
                        continue;
                    }
                };
                let result_id = match result_id(result) {
                    Ok(result_id) => result_id,
                    Err(error) => {
                        repl_stderr(format_args!("{error}\n"))?;
                        continue;
                    }
                };
                // Deterministic only: an explicit `--to`, or the single
                // downstream conversation linked by a work dependency.
                let target_conversation_id = match target {
                    Some(target) => match thread_id(target) {
                        Ok(target) => target,
                        Err(error) => {
                            repl_stderr(format_args!("{error}\n"))?;
                            continue;
                        }
                    },
                    None => match publish.resolve_target(source_conversation_id).await {
                        Ok(target) => target,
                        Err(error) => {
                            repl_stderr(format_args!("{error}\n"))?;
                            continue;
                        }
                    },
                };
                match publish
                    .publish(PublishResult {
                        publish_id: PublishId::new(),
                        result_id,
                        target_conversation_id,
                        published_at: timestamp(),
                    })
                    .await
                {
                    Ok(published) => repl_stdout(format_args!(
                        "{}\t{}\t{}\t{}\t{}\n",
                        published.publish_id,
                        published.result.id,
                        published.source_conversation_id,
                        published.target_conversation_id,
                        published.published_at,
                    ))?,
                    Err(error) => repl_stderr(format_args!("{error}\n"))?,
                }
            }
            "/thread" if !arguments.is_empty() => {
                let fields: Vec<_> = arguments.split_whitespace().collect();
                let (thread, agent) = match fields.as_slice() {
                    [thread] => (*thread, None),
                    [thread, flag, agent] if *flag == "--agent" => (*thread, Some(*agent)),
                    _ => {
                        repl_stderr(format_args!("{}\n", CliError::InvalidCommand))?;
                        continue;
                    }
                };
                let thread_id = match thread_id(thread) {
                    Ok(thread_id) => thread_id,
                    Err(error) => {
                        repl_stderr(format_args!("{error}\n"))?;
                        continue;
                    }
                };
                let agent = match resolve_thread_agent(service, thread_id, agent).await {
                    Ok(agent) => agent,
                    Err(error) => {
                        repl_stderr(format_args!("{error}\n"))?;
                        continue;
                    }
                };
                // A Thread is always entered from its Room.
                let room_id = contexts
                    .last()
                    .unwrap()
                    .room_id()
                    .expect("room and thread scopes own a room");
                if let Err(error) = require_thread_in_room(service, room_id, thread_id).await {
                    repl_stderr(format_args!("{error}\n"))?;
                    continue;
                }
                if !registered.contains(&agent.id) {
                    if let Err(error) = register_acp_agent(workspace, &agent).await {
                        repl_stderr(format_args!("{error}\n"))?;
                        continue;
                    }
                    registered.insert(agent.id);
                }
                if let Err(error) = close_repl_context(live).await {
                    repl_stderr(format_args!("{error}\n"))?;
                    continue;
                }
                match open_repl_thread(workspace, &agent, thread_id, room_id).await {
                    Ok((context, chat)) => {
                        contexts.push(context);
                        *live = Some(chat);
                        repl_stdout(format_args!("thread\t{thread_id}\t{}\n", agent.name))?;
                    }
                    Err(error) => {
                        if let Err(restore) =
                            restore_repl_context(service, workspace, &contexts, live).await
                        {
                            return Err(CliError::OperationAndRestore {
                                operation: Box::new(error),
                                restore: restore.to_string(),
                            });
                        }
                        repl_stderr(format_args!("{error}\n"))?;
                    }
                }
            }
            "/dm" if !arguments.is_empty() => {
                let agent = match agent_ref(arguments) {
                    Ok(reference) => match service.resolve_agent(reference).await {
                        Ok(agent) => agent,
                        Err(error) => {
                            repl_stderr(format_args!("{error}\n"))?;
                            continue;
                        }
                    },
                    Err(error) => {
                        repl_stderr(format_args!("{error}\n"))?;
                        continue;
                    }
                };
                if !registered.contains(&agent.id) {
                    if let Err(error) = register_acp_agent(workspace, &agent).await {
                        repl_stderr(format_args!("{error}\n"))?;
                        continue;
                    }
                    registered.insert(agent.id);
                }
                if let Err(error) = close_repl_context(live).await {
                    repl_stderr(format_args!("{error}\n"))?;
                    continue;
                }
                match open_repl_dm(workspace, &agent).await {
                    Ok((context, dm)) => {
                        contexts.push(context);
                        *live = Some(dm);
                        repl_stdout(format_args!("dm\t{}\t{}\n", agent.id, agent.name))?;
                    }
                    Err(error) => {
                        if let Err(restore) =
                            restore_repl_context(service, workspace, &contexts, live).await
                        {
                            return Err(CliError::OperationAndRestore {
                                operation: Box::new(error),
                                restore: restore.to_string(),
                            });
                        }
                        repl_stderr(format_args!("{error}\n"))?;
                    }
                }
            }
            // A registered command with arguments it does not accept.
            _ => repl_stderr(format_args!("{}\n", CliError::InvalidCommand))?,
        }
    }
}

async fn require_thread_in_room<R: crate::application::CollaborationRuntime>(
    service: &mut CollaborationService<R>,
    room_id: RoomId,
    thread_id: ConversationId,
) -> Result<(), CliError> {
    let threads = service.list_threads(RoomRef::Id(room_id)).await?;
    threads
        .iter()
        .any(|thread| thread.id == thread_id)
        .then_some(())
        .ok_or_else(|| {
            CollaborationError::InvalidCommand(format!(
                "thread {thread_id} does not belong to room {room_id}"
            ))
            .into()
        })
}

async fn open_repl_dm(
    workspace: &WorkspaceRuntime<AcpTransport>,
    agent: &crate::domain::Agent,
) -> Result<(ReplContext, ReplChat), CliError> {
    let runtime = workspace
        .direct_message_for_agent(agent.id)
        .map_err(|error| CliError::Runtime(error.to_string()))?;
    let mut dm = DirectMessageService::new(runtime);
    let opened = dm
        .open(LOCAL_USER_ID.into(), agent.name.clone(), timestamp())
        .await?;
    Ok((
        ReplContext::Dm {
            conversation_id: opened.conversation_id,
            agent_id: opened.agent_id,
            agent_name: opened.agent_name,
        },
        ReplChat::Dm(Box::new(dm)),
    ))
}

async fn open_repl_thread(
    workspace: &WorkspaceRuntime<AcpTransport>,
    agent: &crate::domain::Agent,
    thread_id: ConversationId,
    room_id: RoomId,
) -> Result<(ReplContext, ReplChat), CliError> {
    let mut chat = ThreadChatService::new(open_thread_runtime(workspace, agent.id)?);
    let opened = chat
        .open(
            LOCAL_USER_ID.into(),
            OpenThreadForAgent {
                thread_id,
                agent_id: agent.id,
                opened_at: timestamp(),
            },
        )
        .await?;
    Ok((
        ReplContext::Thread {
            conversation_id: opened.thread_id,
            room_id,
            agent_id: opened.agent_id,
            agent_name: agent.name.clone(),
        },
        ReplChat::Thread(Box::new(chat)),
    ))
}

fn open_thread_runtime(
    workspace: &WorkspaceRuntime<AcpTransport>,
    agent_id: AgentId,
) -> Result<AgentThreadRuntime<AcpTransport>, CliError> {
    workspace
        .thread(agent_id)
        .map_err(|error| CliError::Runtime(error.to_string()))
}

async fn close_repl_context(live: &mut Option<ReplChat>) -> Result<(), CliError> {
    if let Some(chat) = live.as_mut() {
        chat.stop(timestamp()).await?;
    }
    *live = None;
    Ok(())
}

/// Split REPL arguments on whitespace, honouring double-quoted spans so a
/// thread title may contain spaces.
fn repl_arguments(input: &str) -> Result<Vec<String>, CliError> {
    let mut arguments = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut started = false;
    for character in input.chars() {
        match character {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            _ if character.is_whitespace() && !quoted => {
                if started {
                    arguments.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            _ => {
                current.push(character);
                started = true;
            }
        }
    }
    if quoted {
        return Err(CliError::InvalidCommand);
    }
    if started {
        arguments.push(current);
    }
    Ok(arguments)
}

/// `/thread new <title> [--goal <goal>]`. Deliberately not option-heavy;
/// advanced creation stays in `july thread create`.
fn parse_thread_new(arguments: &str) -> Result<(String, Option<String>), CliError> {
    let arguments = repl_arguments(arguments)?;
    let (title, rest) = arguments.split_first().ok_or(CliError::InvalidCommand)?;
    if title.trim().is_empty() || title.starts_with("--") {
        return Err(CliError::InvalidCommand);
    }
    let goal = match rest {
        [] => None,
        [flag, goal] if flag == "--goal" && !goal.trim().is_empty() => Some(goal.clone()),
        _ => return Err(CliError::InvalidCommand),
    };
    Ok((title.clone(), goal))
}

/// Resolve the agent a Thread turn is bound to. An explicit `--agent` wins;
/// otherwise the Thread's single active agent member is used. Zero or several
/// candidates are reported instead of guessed.
async fn resolve_thread_agent<R: crate::application::CollaborationRuntime>(
    service: &mut CollaborationService<R>,
    thread_id: ConversationId,
    agent: Option<&str>,
) -> Result<crate::domain::Agent, CliError> {
    if let Some(agent) = agent {
        return Ok(service.resolve_agent(agent_ref(agent)?).await?);
    }
    let members: Vec<_> = service
        .list_thread_members(thread_id)
        .await?
        .into_iter()
        .filter(|member| member.left_at.is_none() && member.member_type == MemberType::Agent)
        .collect();
    let [member] = members.as_slice() else {
        return Err(CollaborationError::InvalidCommand(format!(
            "thread {thread_id} has {} active agent members; use --agent <agent>",
            members.len()
        ))
        .into());
    };
    let agent_id = AgentId::from_str(&member.member_id).map_err(|_| CliError::InvalidCommand)?;
    Ok(service.resolve_agent(AgentRef::Id(agent_id)).await?)
}

async fn restore_repl_context<R: crate::application::CollaborationRuntime>(
    service: &mut CollaborationService<R>,
    workspace: &WorkspaceRuntime<AcpTransport>,
    contexts: &[ReplContext],
    live: &mut Option<ReplChat>,
) -> Result<(), CliError> {
    let restored = match contexts.last() {
        Some(ReplContext::Dm { agent_id, .. }) => {
            let agent = service.resolve_agent(AgentRef::Id(*agent_id)).await?;
            open_repl_dm(workspace, &agent).await?
        }
        Some(ReplContext::Thread {
            conversation_id,
            room_id,
            agent_id,
            ..
        }) => {
            let agent = service.resolve_agent(AgentRef::Id(*agent_id)).await?;
            open_repl_thread(workspace, &agent, *conversation_id, *room_id).await?
        }
        _ => return Ok(()),
    };
    *live = Some(restored.1);
    Ok(())
}

async fn drain_repl_turn<C: ChatContext>(
    service: &mut C,
    input: &mut mpsc::UnboundedReceiver<io::Result<Option<String>>>,
) -> Result<(), CliError> {
    let mut cancelled = false;
    loop {
        tokio::select! {
            event = service.next(timestamp()) => {
                let Some(event) = event? else { return Err(CliError::EventStreamClosed); };
                match event {
                    ChatEvent::TextDelta(text) => repl_stdout(format_args!("{text}"))?,
                    ChatEvent::MessageCompleted(_) => repl_stdout(format_args!("\n"))?,
                    ChatEvent::PermissionRequested { request_id, options } => {
                        for (index, option) in options.iter().enumerate() {
                            repl_stdout(format_args!("{}. {}\n", index + 1, option.label))?;
                        }
                        repl_stdout(format_args!("permission> "))?;
                        let (selected, interrupted) = tokio::select! {
                            line = input.recv() => (
                                line.transpose()?.flatten()
                                    .and_then(|line| line.parse::<usize>().ok())
                                    .and_then(|index| index.checked_sub(1))
                                    .and_then(|index| options.get(index))
                                    .map(|option| PermissionOutcome::Selected(option.id.clone()))
                                    .unwrap_or(PermissionOutcome::Cancelled),
                                false,
                            ),
                            signal = tokio::signal::ctrl_c(), if !cancelled => {
                                signal?;
                                (PermissionOutcome::Cancelled, true)
                            }
                        };
                        service.permit(request_id, selected, timestamp()).await?;
                        if interrupted {
                            service.cancel(timestamp()).await?;
                            cancelled = true;
                        }
                    }
                    ChatEvent::TurnCompleted => return Ok(()),
                    ChatEvent::TurnFailed(failure) => return Err(turn_failed(failure)),
                    ChatEvent::Disconnected(reason) => return Err(CliError::Disconnected(reason)),
                }
            }
            signal = tokio::signal::ctrl_c(), if !cancelled => {
                signal?;
                service.cancel(timestamp()).await?;
                cancelled = true;
            }
        }
    }
}

fn repl_input() -> mpsc::UnboundedReceiver<io::Result<Option<String>>> {
    let (sender, receiver) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        let stdin = io::stdin();
        let mut stdin = stdin.lock();
        loop {
            let mut line = String::new();
            let input = match stdin.read_line(&mut line) {
                Ok(0) => Ok(None),
                Ok(_) => {
                    line = line.trim_end_matches(['\n', '\r']).into();
                    Ok(Some(line))
                }
                Err(error) => Err(error),
            };
            let done = matches!(&input, Ok(None) | Err(_));
            if sender.send(input).is_err() || done {
                return;
            }
        }
    });
    receiver
}

async fn print_repl_status<R: crate::application::CollaborationRuntime>(
    service: &mut CollaborationService<R>,
    workspace: &WorkspaceRuntime<AcpTransport>,
    context: &ReplContext,
) -> Result<(), CliError> {
    match context {
        ReplContext::Root => repl_stdout(format_args!("root\n"))?,
        ReplContext::Room(room_id) => match service.resolve_room(RoomRef::Id(*room_id)).await {
            Ok(room) => repl_stdout(format_args!("room\t{}\t{}\n", room.id, room.name))?,
            Err(error) => repl_stderr(format_args!("{error}\n"))?,
        },
        ReplContext::Dm {
            conversation_id,
            agent_id,
            agent_name,
        }
        | ReplContext::Thread {
            conversation_id,
            agent_id,
            agent_name,
            ..
        } => {
            let binding = workspace
                .storage()
                .get_current_session_binding(*conversation_id, *agent_id)
                .await
                .map_err(|error| CliError::Runtime(error.to_string()))?;
            let binding_id = binding
                .as_ref()
                .map(|binding| binding.id.to_string())
                .unwrap_or_default();
            let status = binding
                .map(|binding| binding.status.to_string())
                .unwrap_or_else(|| "unbound".into());
            let kind = match context {
                ReplContext::Thread { .. } => "thread",
                _ => "dm",
            };
            repl_stdout(format_args!(
                "{kind}\t{conversation_id}\t{agent_name}\t{binding_id}\t{status}\n"
            ))?;
        }
    }
    Ok(())
}

fn repl_stdout(args: fmt::Arguments<'_>) -> Result<(), CliError> {
    let mut stdout = io::stdout().lock();
    stdout.write_fmt(args)?;
    stdout.flush()?;
    Ok(())
}

fn repl_stderr(args: fmt::Arguments<'_>) -> Result<(), CliError> {
    let mut stderr = io::stderr().lock();
    stderr.write_fmt(args)?;
    stderr.flush()?;
    Ok(())
}

async fn run_dm(agent_name: String) -> Result<(), CliError> {
    let database = database_path()?;
    if let Some(parent) = database
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let (mut workspace, runtime) = open_acp_direct_message(&database, &agent_name)?;
    let mut service = DirectMessageService::new(runtime);
    let interaction = async {
        let opened = service
            .open(LOCAL_USER_ID.into(), agent_name, timestamp())
            .await?;
        for message in opened.messages {
            let sender = match message.sender_type {
                MemberType::User => LOCAL_USER_ID,
                MemberType::Agent => opened.agent_name.as_str(),
            };
            println!("[{sender}] {}", message.body);
        }
        interact(&mut service, &opened.agent_name).await
    }
    .await;
    let stopped_at = timestamp();
    let context_shutdown = service.shutdown(stopped_at.clone()).await;
    let workspace_shutdown = workspace.shutdown(stopped_at).await;
    interaction?;
    context_shutdown?;
    workspace_shutdown.map_err(DirectMessageBootstrapError::from)?;
    Ok(())
}

async fn run_thread_open(thread_id: ConversationId, agent: AgentRef) -> Result<(), CliError> {
    let database = database_path()?;
    if let Some(parent) = database
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let worker =
        StorageWorker::open(&database).map_err(|error| CliError::Runtime(error.to_string()))?;
    let mut workspace =
        WorkspaceRuntime::new(worker).map_err(|error| CliError::Runtime(error.to_string()))?;
    let mut collaboration = CollaborationService::new(workspace.storage());
    let interaction = run_thread_session(&workspace, &mut collaboration, thread_id, agent).await;
    let shutdown = workspace
        .shutdown(timestamp())
        .await
        .map_err(|error| CliError::Runtime(error.to_string()));
    match (interaction, shutdown) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(operation), Ok(())) => Err(operation),
        (Ok(()), Err(shutdown)) => Err(shutdown),
        (Err(operation), Err(shutdown)) => Err(CliError::OperationAndShutdown {
            operation: Box::new(operation),
            shutdown: shutdown.to_string(),
        }),
    }
}

async fn run_thread_session<R: crate::application::CollaborationRuntime>(
    workspace: &WorkspaceRuntime<AcpTransport>,
    collaboration: &mut CollaborationService<R>,
    thread_id: ConversationId,
    agent: AgentRef,
) -> Result<(), CliError> {
    let agent = collaboration.resolve_agent(agent).await?;
    register_acp_agent(workspace, &agent).await?;
    let mut chat = ThreadChatService::new(open_thread_runtime(workspace, agent.id)?);
    chat.open(
        LOCAL_USER_ID.into(),
        OpenThreadForAgent {
            thread_id,
            agent_id: agent.id,
            opened_at: timestamp(),
        },
    )
    .await?;
    let interaction = interact(&mut chat, &agent.name).await;
    let shutdown = chat.stop(timestamp()).await;
    match (interaction, shutdown) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(operation), Ok(())) => Err(operation),
        (Ok(()), Err(shutdown)) => Err(shutdown),
        (Err(operation), Err(shutdown)) => Err(CliError::OperationAndContextShutdown {
            operation: Box::new(operation),
            shutdown: shutdown.to_string(),
        }),
    }
}

async fn run_publish(
    result_id: ResultId,
    target_conversation_id: ConversationId,
    json_output: bool,
) -> Result<(), CliError> {
    let database = database_path()?;
    if let Some(parent) = database
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let worker =
        StorageWorker::open(&database).map_err(|error| CliError::Runtime(error.to_string()))?;
    let mut service = PublishService::new(worker);
    let output = async {
        let published = service
            .publish(PublishResult {
                publish_id: PublishId::new(),
                result_id,
                target_conversation_id,
                published_at: timestamp(),
            })
            .await?;
        Ok::<_, CliError>(if json_output {
            json!({
                "publish_id": published.publish_id.to_string(),
                "result_id": published.result.id.to_string(),
                "source_conversation_id": published.source_conversation_id.to_string(),
                "target_conversation_id": published.target_conversation_id.to_string(),
                "published_at": published.published_at,
            })
            .to_string()
        } else {
            format!(
                "{}\t{}\t{}\t{}\t{}",
                published.publish_id,
                published.result.id,
                published.source_conversation_id,
                published.target_conversation_id,
                published.published_at,
            )
        })
    }
    .await;
    let mut worker = service.into_runtime();
    let shutdown = worker
        .shutdown()
        .await
        .map_err(|error| CliError::Runtime(error.to_string()));
    match (output, shutdown) {
        (Ok(output), Ok(())) => {
            println!("{output}");
            Ok(())
        }
        (Err(operation), Ok(())) => Err(operation),
        (Ok(_), Err(shutdown)) => Err(shutdown),
        (Err(operation), Err(shutdown)) => Err(CliError::OperationAndShutdown {
            operation: Box::new(operation),
            shutdown: shutdown.to_string(),
        }),
    }
}

async fn run_agent(operation: AgentOperation, json_output: bool) -> Result<(), CliError> {
    let database = database_path()?;
    if let Some(parent) = database
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let worker =
        StorageWorker::open(&database).map_err(|error| CliError::Runtime(error.to_string()))?;
    let mut service = CollaborationService::new(worker);
    let output = async {
        let output = match operation {
            AgentOperation::Add {
                name,
                project,
                runtime,
                transport,
                config,
            } => {
                let transport_config = match config {
                    Some(path) => serde_json::from_str(&std::fs::read_to_string(path)?)
                        .map_err(|error| CliError::Runtime(error.to_string()))?,
                    None => json!({}),
                };
                // Identity only: no AgentSession is started, no Room joined.
                let agent = service
                    .add_agent(AddAgent {
                        agent_id: AgentId::new(),
                        name,
                        project_root: project,
                        transport_type: transport,
                        transport_config,
                        runtime,
                        created_at: timestamp(),
                    })
                    .await?;
                Some(render_agent(&agent, json_output))
            }
            AgentOperation::List => {
                let agents = service.list_agents().await?;
                if json_output {
                    Some(json!(agents.iter().map(agent_json).collect::<Vec<_>>()).to_string())
                } else {
                    let output = agents
                        .iter()
                        .map(|agent| render_agent(agent, false))
                        .collect::<Vec<_>>()
                        .join("\n");
                    (!output.is_empty()).then_some(output)
                }
            }
            AgentOperation::Show(reference) => {
                let agent = service.resolve_agent(reference).await?;
                Some(render_agent(&agent, json_output))
            }
            AgentOperation::Remove(reference) => {
                let agent = service.remove_agent(reference, timestamp()).await?;
                Some(render_agent(&agent, json_output))
            }
        };
        Ok::<_, CliError>(output)
    }
    .await;
    let mut worker = service.into_runtime();
    let shutdown = worker
        .shutdown()
        .await
        .map_err(|error| CliError::Runtime(error.to_string()));
    match (output, shutdown) {
        (Ok(Some(output)), Ok(())) => {
            println!("{output}");
            Ok(())
        }
        (Ok(None), Ok(())) => Ok(()),
        (Err(operation), Ok(())) => Err(operation),
        (Ok(_), Err(shutdown)) => Err(shutdown),
        (Err(operation), Err(shutdown)) => Err(CliError::OperationAndShutdown {
            operation: Box::new(operation),
            shutdown: shutdown.to_string(),
        }),
    }
}

/// The runtime preference an agent was onboarded with, if any.
fn agent_runtime(agent: &crate::domain::Agent) -> String {
    agent
        .metadata
        .get("runtime")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn agent_json(agent: &crate::domain::Agent) -> serde_json::Value {
    json!({
        "agent_id": agent.id.to_string(),
        "name": agent.name,
        "project_root": agent.project_root,
        "transport_type": agent.transport_type,
        "runtime": agent_runtime(agent),
        "status": agent.status,
        "created_at": agent.created_at,
        "updated_at": agent.updated_at,
    })
}

fn render_agent(agent: &crate::domain::Agent, json_output: bool) -> String {
    if json_output {
        return agent_json(agent).to_string();
    }
    format!(
        "{}\t{}\t{}\t{}\t{}\t{}",
        agent.id,
        agent.name,
        agent.project_root,
        agent.transport_type,
        agent_runtime(agent),
        agent.status,
    )
}

async fn run_room(operation: RoomOperation, json_output: bool) -> Result<(), CliError> {
    let database = database_path()?;
    if let Some(parent) = database
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let worker =
        StorageWorker::open(&database).map_err(|error| CliError::Runtime(error.to_string()))?;
    let mut service = CollaborationService::new(worker);
    let output = async {
        let output = match operation {
            RoomOperation::Create { name, description } => {
                let room_id = service
                    .create_room(CreateRoom {
                        room_id: RoomId::new(),
                        name,
                        description,
                        created_at: timestamp(),
                    })
                    .await?;
                if json_output {
                    Some(json!({ "room_id": room_id.to_string() }).to_string())
                } else {
                    Some(room_id.to_string())
                }
            }
            RoomOperation::List => {
                let rooms = service.list_rooms().await?;
                if json_output {
                    let rooms: Vec<_> = rooms
                        .into_iter()
                        .map(|room| {
                            json!({
                                "room_id": room.id.to_string(), "name": room.name,
                                "description": room.description, "status": room.status,
                                "created_at": room.created_at, "updated_at": room.updated_at,
                            })
                        })
                        .collect();
                    Some(json!(rooms).to_string())
                } else {
                    let output = rooms
                        .into_iter()
                        .map(|room| {
                            format!(
                                "{}\t{}\t{}\t{}",
                                room.id,
                                room.name,
                                room.description.unwrap_or_default(),
                                room.status
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    (!output.is_empty()).then_some(output)
                }
            }
            RoomOperation::Members(room) => {
                let members = service.list_room_members(room).await?;
                let output = render_room_members(members, json_output);
                (!output.is_empty()).then_some(output)
            }
            RoomOperation::AddMember { room, agent } => {
                let change = service
                    .add_room_member(AddRoomMember {
                        room,
                        agent,
                        role: None,
                        changed_at: timestamp(),
                    })
                    .await?;
                Some(render_membership_change(change, json_output))
            }
            RoomOperation::RemoveMember { room, agent } => {
                let change = service
                    .remove_room_member(RemoveRoomMember {
                        room,
                        agent,
                        changed_at: timestamp(),
                    })
                    .await?;
                Some(render_membership_change(change, json_output))
            }
        };
        Ok::<_, CliError>(output)
    }
    .await;
    let mut worker = service.into_runtime();
    let shutdown = worker
        .shutdown()
        .await
        .map_err(|error| CliError::Runtime(error.to_string()));
    match (output, shutdown) {
        (Ok(Some(output)), Ok(())) => {
            println!("{output}");
            Ok(())
        }
        (Ok(None), Ok(())) => Ok(()),
        (Err(operation), Ok(())) => Err(operation),
        (Ok(_), Err(shutdown)) => Err(shutdown),
        (Err(operation), Err(shutdown)) => Err(CliError::OperationAndShutdown {
            operation: Box::new(operation),
            shutdown: shutdown.to_string(),
        }),
    }
}

async fn run_thread(operation: ThreadOperation, json_output: bool) -> Result<(), CliError> {
    let database = database_path()?;
    if let Some(parent) = database
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let worker =
        StorageWorker::open(&database).map_err(|error| CliError::Runtime(error.to_string()))?;
    let mut service = CollaborationService::new(worker);
    let output = async {
        let output = match operation {
            ThreadOperation::Create {
                title,
                room,
                goal,
                members,
            } => {
                let thread = service
                    .create_thread(CreateThread {
                        thread_id: ConversationId::new(),
                        primary_work_id: WorkItemId::new(),
                        room,
                        title,
                        goal,
                        user_id: LOCAL_USER_ID.into(),
                        initial_agents: members,
                        created_at: timestamp(),
                    })
                    .await?;
                if json_output {
                    Some(
                        json!({
                            "thread_id": thread.thread_id.to_string(),
                            "primary_work_id": thread.primary_work_id.to_string(),
                        })
                        .to_string(),
                    )
                } else {
                    Some(format!("{}\t{}", thread.thread_id, thread.primary_work_id))
                }
            }
            ThreadOperation::Members(thread_id) => {
                let members = service.list_thread_members(thread_id).await?;
                Some(render_thread_members(members, json_output))
            }
            ThreadOperation::AddMember { thread_id, agent } => Some(render_membership_change(
                service
                    .add_thread_member(AddThreadMember {
                        thread_id,
                        agent,
                        changed_at: timestamp(),
                    })
                    .await?,
                json_output,
            )),
            ThreadOperation::RemoveMember { thread_id, agent } => Some(render_membership_change(
                service
                    .remove_thread_member(RemoveThreadMember {
                        thread_id,
                        agent,
                        changed_at: timestamp(),
                    })
                    .await?,
                json_output,
            )),
            ThreadOperation::List(room) => {
                let threads = service.list_threads(room).await?;
                if json_output {
                    Some(
                        json!(
                            threads
                                .into_iter()
                                .map(|thread| json!({
                                    "thread_id": thread.id.to_string(),
                                    "room_id": thread.room_id.map(|id| id.to_string()),
                                    "title": thread.title,
                                    "goal": thread.goal,
                                    "status": thread.status,
                                    "created_at": thread.created_at,
                                    "updated_at": thread.updated_at,
                                }))
                                .collect::<Vec<_>>()
                        )
                        .to_string(),
                    )
                } else {
                    let output = threads
                        .into_iter()
                        .map(|thread| {
                            format!(
                                "{}\t{}\t{}\t{}\t{}",
                                thread.id,
                                thread.room_id.map(|id| id.to_string()).unwrap_or_default(),
                                thread.title.unwrap_or_default(),
                                thread.goal.unwrap_or_default(),
                                thread.status,
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    (!output.is_empty()).then_some(output)
                }
            }
        };
        Ok::<_, CliError>(output)
    }
    .await;
    let mut worker = service.into_runtime();
    let shutdown = worker
        .shutdown()
        .await
        .map_err(|error| CliError::Runtime(error.to_string()));
    match (output, shutdown) {
        (Ok(Some(output)), Ok(())) => {
            println!("{output}");
            Ok(())
        }
        (Ok(None), Ok(())) => Ok(()),
        (Err(operation), Ok(())) => Err(operation),
        (Ok(_), Err(shutdown)) => Err(shutdown),
        (Err(operation), Err(shutdown)) => Err(CliError::OperationAndShutdown {
            operation: Box::new(operation),
            shutdown: shutdown.to_string(),
        }),
    }
}

fn render_room_members(members: Vec<crate::domain::RoomMember>, json_output: bool) -> String {
    if json_output {
        json!(
            members
                .into_iter()
                .map(|member| json!({
                    "room_id": member.room_id.to_string(), "agent_id": member.agent_id.to_string(),
                    "role": member.role, "generation": member.generation,
                    "joined_at": member.joined_at, "left_at": member.left_at,
                    "state": membership_state(member.left_at.is_none()),
                }))
                .collect::<Vec<_>>()
        )
        .to_string()
    } else {
        members
            .into_iter()
            .map(|member| {
                let state = membership_state(member.left_at.is_none());
                format!(
                    "{}\t{}\t{}\t{}\t{}\t{}\t{state}",
                    member.room_id,
                    member.agent_id,
                    member.role.unwrap_or_default(),
                    member.generation,
                    member.joined_at,
                    member.left_at.unwrap_or_default(),
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn render_thread_members(
    members: Vec<crate::domain::ConversationMember>,
    json_output: bool,
) -> String {
    if json_output {
        json!(
            members
                .into_iter()
                .map(|member| json!({
                    "thread_id": member.conversation_id.to_string(),
                    "member_type": match member.member_type {
                        MemberType::User => "user",
                        MemberType::Agent => "agent",
                    },
                    "member_id": member.member_id,
                    "generation": member.generation,
                    "joined_at": member.joined_at,
                    "left_at": member.left_at,
                    "state": membership_state(member.left_at.is_none()),
                }))
                .collect::<Vec<_>>()
        )
        .to_string()
    } else {
        members
            .into_iter()
            .map(|member| {
                let state = membership_state(member.left_at.is_none());
                let member_type = match member.member_type {
                    MemberType::User => "user",
                    MemberType::Agent => "agent",
                };
                format!(
                    "{}\t{member_type}\t{}\t{}\t{}\t{}\t{state}",
                    member.conversation_id,
                    member.member_id,
                    member.generation,
                    member.joined_at,
                    member.left_at.unwrap_or_default(),
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn render_membership_change(change: MembershipChange, json_output: bool) -> String {
    let state = match change.state {
        MembershipState::Active => "active",
        MembershipState::Left => "left",
    };
    if json_output {
        json!({ "state": state, "changed": change.changed }).to_string()
    } else {
        format!("{state}\t{}", change.changed)
    }
}

fn membership_state(active: bool) -> &'static str {
    if active { "active" } else { "left" }
}

fn run_version(json: bool) -> Result<(), CliError> {
    let version = env!("CARGO_PKG_VERSION");
    if json {
        println!("{}", json!({ "name": "july", "version": version }));
    } else {
        println!("july {version}");
    }
    Ok(())
}

fn database_path() -> Result<PathBuf, CliError> {
    if let Some(path) = std::env::var_os("JULY_WORKSPACE_DB") {
        return Ok(path.into());
    }
    let home = std::env::var_os("HOME").ok_or(CliError::MissingHome)?;
    let directory = PathBuf::from(home).join(".july");
    std::fs::create_dir_all(&directory)?;
    Ok(directory.join("workspace.db"))
}

async fn interact<C: ChatContext>(service: &mut C, agent_name: &str) -> Result<(), CliError> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    loop {
        print!("[{agent_name}] > ");
        io::stdout().flush()?;
        let Some(line) = next_input_or_event(service, &mut lines).await? else {
            return Ok(());
        };
        // The single-conversation stream shares the REPL's exit vocabulary.
        if line == "/exit" || line == "/quit" {
            return Ok(());
        }
        if line.trim().is_empty() {
            continue;
        }
        service.send(line, timestamp()).await?;
        drain_turn(service, &mut lines).await?;
    }
}

async fn next_input_or_event<C: ChatContext>(
    service: &mut C,
    lines: &mut Lines<BufReader<Stdin>>,
) -> Result<Option<String>, CliError> {
    loop {
        tokio::select! {
            line = lines.next_line() => return Ok(line?),
            event = service.next(timestamp()) => {
                let Some(event) = event? else { return Err(CliError::EventStreamClosed); };
                if handle_idle_event(service, lines, event).await? {
                    return Ok(None);
                }
            }
            signal = tokio::signal::ctrl_c() => {
                signal?;
                return Ok(None);
            }
        }
    }
}

async fn drain_turn<C: ChatContext>(
    service: &mut C,
    lines: &mut Lines<BufReader<Stdin>>,
) -> Result<(), CliError> {
    let mut cancelled = false;
    loop {
        tokio::select! {
            event = service.next(timestamp()) => {
                let Some(event) = event? else { return Err(CliError::EventStreamClosed); };
                match event {
                    ChatEvent::TextDelta(text) => {
                        print!("{text}");
                        io::stdout().flush()?;
                    }
                    ChatEvent::MessageCompleted(_) => println!(),
                    ChatEvent::PermissionRequested { request_id, options } => {
                        if permission(service, lines, request_id, &options).await? && !cancelled {
                            service.cancel(timestamp()).await?;
                            cancelled = true;
                        }
                    }
                    ChatEvent::TurnCompleted => return Ok(()),
                    ChatEvent::TurnFailed(failure) => return Err(turn_failed(failure)),
                    ChatEvent::Disconnected(reason) => return Err(CliError::Disconnected(reason)),
                }
            }
            signal = tokio::signal::ctrl_c(), if !cancelled => {
                signal?;
                service.cancel(timestamp()).await?;
                cancelled = true;
            }
        }
    }
}

async fn handle_idle_event<C: ChatContext>(
    service: &mut C,
    lines: &mut Lines<BufReader<Stdin>>,
    event: ChatEvent,
) -> Result<bool, CliError> {
    match event {
        ChatEvent::TextDelta(text) => {
            print!("{text}");
            io::stdout().flush()?;
        }
        ChatEvent::MessageCompleted(_) => println!(),
        ChatEvent::PermissionRequested {
            request_id,
            options,
        } => {
            return permission(service, lines, request_id, &options).await;
        }
        ChatEvent::TurnCompleted => {}
        ChatEvent::TurnFailed(failure) => return Err(turn_failed(failure)),
        ChatEvent::Disconnected(reason) => return Err(CliError::Disconnected(reason)),
    }
    Ok(false)
}

async fn permission<C: ChatContext>(
    service: &mut C,
    lines: &mut Lines<BufReader<Stdin>>,
    request_id: ChatPermissionRequestId,
    options: &[PermissionOption],
) -> Result<bool, CliError> {
    for (index, option) in options.iter().enumerate() {
        println!("{}. {}", index + 1, option.label);
    }
    print!("permission> ");
    io::stdout().flush()?;
    let (outcome, interrupted) = tokio::select! {
        line = lines.next_line() => {
            let selected = line?.and_then(|line| line.parse::<usize>().ok())
                .and_then(|index| index.checked_sub(1))
                .and_then(|index| options.get(index))
                .map(|option| PermissionOutcome::Selected(option.id.clone()))
                .unwrap_or(PermissionOutcome::Cancelled);
            (selected, false)
        }
        signal = tokio::signal::ctrl_c() => {
            signal?;
            (PermissionOutcome::Cancelled, true)
        }
    };
    service.permit(request_id, outcome, timestamp()).await?;
    Ok(interrupted)
}

fn turn_failed(failure: ChatFailureKind) -> CliError {
    CliError::TurnFailed(match failure {
        ChatFailureKind::AuthenticationRequired => "authentication required",
        ChatFailureKind::Protocol => "protocol error",
    })
}

fn timestamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::CliError;

    #[test]
    fn operation_and_restore_preserves_the_operation_error_code() {
        let error = CliError::OperationAndRestore {
            operation: Box::new(CliError::InvalidCommand),
            restore: "restore failed".into(),
        };

        assert_eq!(error.error_code(), "invalid_command");
        assert_eq!(
            error.to_string(),
            "invalid command; context restore failed: restore failed"
        );
    }

    #[test]
    fn operation_and_context_shutdown_preserves_the_operation_error_code() {
        let error = CliError::OperationAndContextShutdown {
            operation: Box::new(CliError::InvalidCommand),
            shutdown: "detach failed".into(),
        };

        assert_eq!(error.error_code(), "invalid_command");
        assert_eq!(
            error.to_string(),
            "invalid command; context shutdown failed: detach failed"
        );
    }
}
