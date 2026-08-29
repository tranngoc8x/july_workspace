use crate::adapter::AdapterStore;
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
    WorkspaceRuntime, open_acp_direct_message, parse_acp_config, register_acp_agent,
};
use crate::transport::AcpTransport;
use chrono::{SecondsFormat, Utc};
use serde_json::json;
use std::collections::{HashSet, VecDeque};
use std::ffi::OsString;
use std::fmt;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, BufReader, Lines, Stdin};
use tokio::sync::{mpsc, oneshot};

mod init;
mod keys;
mod mention;
pub mod registry;

use registry::CommandScope;

const LOCAL_USER_ID: &str = "local-user";
const USAGE: &str = "usage: july dm <agent>";
const INIT_USAGE: &str = "usage: july init [--adapters <ids>]      cài ACP adapter";
const AGENT_USAGE: &str = "usage: july agent add <name> --project <path> --adapter <id> [--runtime <runtime>]\n\
                          usage: july agent add <name> --project <path> --transport <type> --config <file> [--runtime <runtime>]\n\
                          usage: july agent update <agent> --adapter <id>\n\
                          usage: july agent update <agent> --config <file>";
const NO_AGENTS: &str = "no agents configured; add one with: \
                         july agent add <name> --project <path> --adapter <id>";

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
    Adapter(#[from] crate::adapter::AdapterError),
    #[error(
        "agent dùng transport acp cần --adapter <id> hoặc --config <file>; chạy july init để xem adapter đã cài"
    )]
    MissingAdapter,
    #[error("chưa chọn adapter nào; chọn ít nhất một để july có thể chạy agent")]
    NoAdapterSelected,
    #[error("{INIT_USAGE}")]
    InitUsage,
    #[error("{AGENT_USAGE}")]
    AgentUsage,
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
        Command::Init { adapters } => init::run_init(adapters).await,
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
            Self::Usage
            | Self::InitUsage
            | Self::AgentUsage
            | Self::InvalidAgentName
            | Self::InvalidUtf8 => "usage",
            Self::InvalidCommand => "invalid_command",
            Self::Adapter(_) => "adapter",
            Self::MissingAdapter => "missing_adapter",
            Self::NoAdapterSelected => "no_adapter_selected",
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
    Init {
        adapters: Option<Vec<String>>,
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
        adapter: Option<String>,
        config: Option<PathBuf>,
    },
    List,
    Show(AgentRef),
    Remove(AgentRef),
    Update {
        agent: AgentRef,
        adapter: Option<String>,
        config: Option<PathBuf>,
    },
}

struct AgentAddArgs {
    project: String,
    runtime: Option<String>,
    transport: String,
    adapter: Option<String>,
    config: Option<PathBuf>,
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
        Some("init") => parse_init(args, json),
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

fn parse_init(args: Vec<String>, json: bool) -> Result<Command, CliError> {
    if json {
        return Err(CliError::InitUsage);
    }
    match args.as_slice() {
        [_] => Ok(Command::Init { adapters: None }),
        [_, flag, list] if flag == "--adapters" => Ok(Command::Init {
            adapters: Some(list.split(',').map(str::to_owned).collect()),
        }),
        _ => Err(CliError::InitUsage),
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
        [_, command, agent] if command == "show" => {
            AgentOperation::Show(agent_ref(agent).map_err(|_| CliError::AgentUsage)?)
        }
        [_, command, agent] if command == "remove" => {
            AgentOperation::Remove(agent_ref(agent).map_err(|_| CliError::AgentUsage)?)
        }
        [_, command, agent, rest @ ..] if command == "update" => {
            let (adapter, config) = parse_agent_update(rest)?;
            AgentOperation::Update {
                agent: agent_ref(agent).map_err(|_| CliError::AgentUsage)?,
                adapter,
                config,
            }
        }
        [_, command, name, rest @ ..] if command == "add" => {
            let AgentAddArgs {
                project,
                runtime,
                transport,
                adapter,
                config,
            } = parse_agent_add(rest)?;
            AgentOperation::Add {
                name: positional(name).map_err(|_| CliError::AgentUsage)?,
                project,
                runtime,
                transport,
                adapter,
                config,
            }
        }
        _ if matches!(
            args.get(1).map(String::as_str),
            Some("add" | "list" | "show" | "remove" | "update")
        ) =>
        {
            return Err(CliError::AgentUsage);
        }
        _ => return Err(CliError::InvalidCommand),
    };
    Ok(Command::Agent { operation, json })
}

/// `--project` is required; `--runtime` is a preference. `--adapter` generates
/// an ACP config from the catalog, while `--transport` plus `--config` is the
/// custom transport escape hatch.
fn parse_agent_add(args: &[String]) -> Result<AgentAddArgs, CliError> {
    let mut project = None;
    let mut runtime = None;
    let mut transport = None;
    let mut adapter = None;
    let mut config = None;
    let mut index = 0;
    while index < args.len() {
        let Some(value) = args.get(index + 1).filter(|value| !value.starts_with("--")) else {
            return Err(CliError::AgentUsage);
        };
        match args[index].as_str() {
            "--project" if project.is_none() => project = Some(value.clone()),
            "--runtime" if runtime.is_none() => runtime = Some(value.clone()),
            "--transport" if transport.is_none() => transport = Some(value.clone()),
            "--adapter" if adapter.is_none() => adapter = Some(value.clone()),
            "--config" if config.is_none() => config = Some(PathBuf::from(value)),
            _ => return Err(CliError::AgentUsage),
        }
        index += 2;
    }
    if adapter.is_some() && (transport.is_some() || config.is_some()) {
        return Err(CliError::AgentUsage);
    }
    let project = project.ok_or(CliError::AgentUsage)?;
    Ok(AgentAddArgs {
        project,
        runtime,
        transport: transport.unwrap_or_else(|| "acp".into()),
        adapter,
        config,
    })
}

fn parse_agent_update(args: &[String]) -> Result<(Option<String>, Option<PathBuf>), CliError> {
    let mut adapter = None;
    let mut config = None;
    let mut index = 0;
    while index < args.len() {
        let Some(value) = args.get(index + 1).filter(|value| !value.starts_with("--")) else {
            return Err(CliError::AgentUsage);
        };
        match args[index].as_str() {
            "--adapter" if adapter.is_none() => adapter = Some(value.clone()),
            "--config" if config.is_none() => config = Some(PathBuf::from(value)),
            _ => return Err(CliError::AgentUsage),
        }
        index += 2;
    }
    match (&adapter, &config) {
        (None, None) | (Some(_), Some(_)) => Err(CliError::AgentUsage),
        _ => Ok((adapter, config)),
    }
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
    if use_tui(io::stdin().is_terminal(), io::stdout().is_terminal()) {
        return run_tui_repl(database).await;
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

const fn use_tui(stdin_is_terminal: bool, stdout_is_terminal: bool) -> bool {
    stdin_is_terminal && stdout_is_terminal
}

async fn run_tui_repl(database: PathBuf) -> Result<(), CliError> {
    let bridge = std::cell::RefCell::new(InactiveTuiBridge::open(database).await?);
    let interaction = crate::tui::run_app(
        |command| bridge.borrow().dispatch(command).map_err(io::Error::other),
        || {
            bridge
                .borrow_mut()
                .try_next_event()
                .map_err(io::Error::other)
        },
    )
    .await
    .map_err(|error| CliError::Runtime(error.to_string()));
    let shutdown = bridge.into_inner().shutdown().await;
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

struct ReplLine {
    line: Option<String>,
    origin: Option<crate::tui::app::ContextId>,
}

enum ReplInput {
    Line(io::Result<ReplLine>),
    Permission {
        request_id: ChatPermissionRequestId,
        outcome: PermissionOutcome,
    },
    Cancel,
}

struct ReplSessionState {
    contexts: Vec<ReplContext>,
    registered: HashSet<AgentId>,
    live: Option<ReplChat>,
}

impl ReplSessionState {
    fn new() -> Self {
        Self {
            contexts: vec![ReplContext::Root],
            registered: HashSet::new(),
            live: None,
        }
    }
}

struct ReplCaptureWriter<W> {
    inner: W,
    captured: Vec<u8>,
}

impl<W> ReplCaptureWriter<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            captured: Vec::new(),
        }
    }

    fn clear(&mut self) {
        self.captured.clear();
    }

    fn take(&mut self) -> Option<String> {
        if self.captured.is_empty() {
            return None;
        }
        Some(
            String::from_utf8_lossy(&std::mem::take(&mut self.captured))
                .trim_end()
                .to_owned(),
        )
    }
}

impl<W: Write> Write for ReplCaptureWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(bytes)?;
        self.captured.extend_from_slice(&bytes[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

async fn interact_repl<R: crate::application::CollaborationRuntime>(
    service: &mut CollaborationService<R>,
    workspace: &WorkspaceRuntime<AcpTransport>,
) -> Result<(), CliError> {
    let mut input = repl_input();
    let stdout = io::stdout();
    let stderr = io::stderr();
    let mut stdout = ReplCaptureWriter::new(stdout.lock());
    let mut stderr = ReplCaptureWriter::new(stderr.lock());
    let mut state = ReplSessionState::new();
    let interaction = interact_repl_loop(
        service,
        workspace,
        &mut input,
        &mut stdout,
        &mut stderr,
        &mut state,
        None,
    )
    .await;
    let shutdown = close_repl_context(&mut state.live).await;
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
    input: &mut mpsc::UnboundedReceiver<ReplInput>,
    stdout: &mut ReplCaptureWriter<impl Write>,
    stderr: &mut ReplCaptureWriter<impl Write>,
    state: &mut ReplSessionState,
    tui_events: Option<&mpsc::UnboundedSender<crate::tui::app::AppEvent>>,
) -> Result<(), CliError> {
    let ReplSessionState {
        contexts,
        registered,
        live,
    } = state;
    let mut publish = PublishService::new(workspace.storage());
    let mut pending_origin = None;
    // Navigation echoes duplicate the TUI breadcrumb, so they never become a status.
    let mut navigated = false;
    loop {
        if let Some(origin) = pending_origin.take() {
            let context = project_repl_context(service, contexts).await?;
            let result = match (stderr.take(), stdout.take()) {
                (Some(error), _) => crate::tui::app::CommandResult::Failed(error),
                (None, Some(output)) if !navigated => {
                    crate::tui::app::CommandResult::Output { context, output }
                }
                (None, _) => crate::tui::app::CommandResult::Context(context),
            };
            navigated = false;
            if let Some(events) = tui_events {
                let _ = events.send(crate::tui::app::AppEvent::CommandFinished {
                    context: origin,
                    result,
                });
            }
        } else {
            stderr.clear();
            stdout.clear();
        }
        repl_write(stdout, format_args!("> "))?;
        stdout.clear();
        let request = tokio::select! {
            input = input.recv() => match input {
                Some(ReplInput::Line(line)) => line?,
                Some(ReplInput::Permission { .. }) => {
                    if let Some(events) = tui_events {
                        let _ = events.send(crate::tui::app::AppEvent::PermissionFinished(
                            Err("no active permission request".into()),
                        ));
                    }
                    continue;
                }
                Some(ReplInput::Cancel) => {
                    if let Some(events) = tui_events {
                        let _ = events.send(crate::tui::app::AppEvent::CancelFinished(
                            Err("no active turn".into()),
                        ));
                    }
                    continue;
                }
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
        pending_origin = request.origin;
        let Some(line) = request.line else {
            close_repl_context(live).await?;
            return Ok(());
        };
        let scope = contexts.last().unwrap().scope();
        let Some((spec, arguments)) = registry::resolve(&line) else {
            // Not a registered command: chat sends it, other contexts reject it.
            if line.trim().is_empty() {
                continue;
            }
            // Rules B and C: `@agent ...` targets work regardless of context.
            if let Some(parsed) = mention::parse(&line) {
                let names: Vec<String> = parsed.agents.iter().map(|&n| n.to_owned()).collect();
                let prompt = parsed.prompt.to_owned();
                // A new context replaces the breadcrumb, so its echo is not a status.
                navigated = true;
                if let Some(error) = route_mentions(
                    service, workspace, contexts, registered, live, &names, &prompt, stdout,
                )
                .await?
                {
                    repl_write(stderr, format_args!("{error}\n"))?;
                    continue;
                }
                if prompt.is_empty() {
                    continue;
                }
                let chat = live.as_mut().expect("entered context has a live service");
                if let Err(error) = chat.send(prompt, timestamp()).await {
                    repl_write(stderr, format_args!("{error}\n"))?;
                    continue;
                }
                if let (Some(events), Some(origin)) = (tui_events, pending_origin.take()) {
                    let _ = events.send(crate::tui::app::AppEvent::CommandFinished {
                        context: origin,
                        result: crate::tui::app::CommandResult::Submitted,
                    });
                }
                drain_repl_turn(chat, input, stdout, tui_events).await?;
                continue;
            }
            if matches!(
                contexts.last(),
                Some(ReplContext::Dm { .. } | ReplContext::Thread { .. })
            ) {
                let chat = live.as_mut().expect("active context has a live service");
                if let Err(error) = chat.send(line, timestamp()).await {
                    repl_write(stderr, format_args!("{error}\n"))?;
                    continue;
                }
                if let (Some(events), Some(origin)) = (tui_events, pending_origin.take()) {
                    let _ = events.send(crate::tui::app::AppEvent::CommandFinished {
                        context: origin,
                        result: crate::tui::app::CommandResult::Submitted,
                    });
                }
                drain_repl_turn(chat, input, stdout, tui_events).await?;
            } else {
                repl_write(stderr, format_args!("{}\n", CliError::InvalidCommand))?;
            }
            continue;
        };
        if !spec.available_in(scope) {
            repl_write(stderr, format_args!("{}\n", spec.scope_error(scope)))?;
            continue;
        }
        let arguments = arguments.to_owned();
        let arguments = arguments.as_str();
        match spec.name {
            "/exit" if arguments.is_empty() => {
                if let Some(events) = tui_events {
                    let _ = events.send(crate::tui::app::AppEvent::Exit);
                }
                close_repl_context(live).await?;
                return Ok(());
            }
            "/help" if arguments.is_empty() => {
                repl_write(stdout, format_args!("{}", registry::help(scope)))?
            }
            "/help" => match registry::find(arguments) {
                Some(spec) => repl_write(stdout, format_args!("{}", registry::help_command(spec)))?,
                None => repl_write(stderr, format_args!("unknown command: {arguments}\n"))?,
            },
            "/status" if arguments.is_empty() => {
                print_repl_status(service, workspace, contexts.last().unwrap(), stdout, stderr)
                    .await?
            }
            "/rooms" if arguments.is_empty() => match service.list_rooms().await {
                Ok(rooms) => {
                    for room in rooms {
                        repl_write(
                            stdout,
                            format_args!("{}\t{}\t{}\n", room.id, room.name, room.status),
                        )?;
                    }
                }
                Err(error) => repl_write(stderr, format_args!("{error}\n"))?,
            },
            "/agents" if arguments.is_empty() => match service.list_agents().await {
                Ok(agents) if agents.is_empty() => {
                    repl_write(stderr, format_args!("{NO_AGENTS}\n"))?
                }
                Ok(agents) => {
                    for agent in agents {
                        repl_write(
                            stdout,
                            format_args!(
                                "{}\t{}\t{}\t{}\t{}\n",
                                agent.id,
                                agent.name,
                                agent.project_root,
                                agent.transport_type,
                                agent.status
                            ),
                        )?;
                    }
                }
                Err(error) => repl_write(stderr, format_args!("{error}\n"))?,
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
                            repl_write(
                                stdout,
                                format_args!(
                                    "{}\t{}\t{}\t{}\n",
                                    work.id,
                                    work.status,
                                    work.title,
                                    work.owner_agent_id
                                        .map(|agent| agent.to_string())
                                        .unwrap_or_default(),
                                ),
                            )?;
                        }
                    }
                    Err(error) => repl_write(stderr, format_args!("{error}\n"))?,
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
                            repl_write(
                                stdout,
                                format_args!(
                                    "{}\t{}\t{}\t{}\n",
                                    result.id, result.work_id, result.status, result.summary,
                                ),
                            )?;
                        }
                    }
                    Err(error) => repl_write(stderr, format_args!("{error}\n"))?,
                }
            }
            "/restart" if arguments.is_empty() => {
                if let Err(error) = close_repl_context(live).await {
                    repl_write(stderr, format_args!("{error}\n"))?;
                    continue;
                }
                if let Err(error) = restore_repl_context(service, workspace, contexts, live).await {
                    repl_write(stderr, format_args!("{error}\n"))?;
                    continue;
                }
                print_repl_status(service, workspace, contexts.last().unwrap(), stdout, stderr)
                    .await?
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
                            Ok(thread) => repl_write(
                                stdout,
                                format_args!(
                                    "thread\t{}\t{}\n",
                                    thread.thread_id, thread.primary_work_id
                                ),
                            )?,
                            Err(error) => repl_write(stderr, format_args!("{error}\n"))?,
                        }
                    }
                    Err(error) => repl_write(stderr, format_args!("{error}\n"))?,
                }
            }
            "/back" if contexts.len() == 1 => {
                repl_write(stderr, format_args!("already at root\n"))?
            }
            "/back" if arguments.is_empty() => {
                let previous = contexts.last().unwrap().clone();
                if let Err(error) = close_repl_context(live).await {
                    repl_write(stderr, format_args!("{error}\n"))?;
                    continue;
                }
                contexts.pop();
                if let Err(error) = restore_repl_context(service, workspace, contexts, live).await {
                    contexts.push(previous);
                    if let Err(restore) =
                        restore_repl_context(service, workspace, contexts, live).await
                    {
                        return Err(CliError::OperationAndRestore {
                            operation: Box::new(error),
                            restore: restore.to_string(),
                        });
                    }
                    repl_write(stderr, format_args!("{error}\n"))?;
                    continue;
                }
                print_repl_status(service, workspace, contexts.last().unwrap(), stdout, stderr)
                    .await?;
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
                                repl_write(stdout, format_args!("{output}\n"))?;
                            }
                        }
                        Err(error) => repl_write(stderr, format_args!("{error}\n"))?,
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
                            repl_write(stdout, format_args!("{output}\n"))?;
                        }
                    }
                    Err(error) => repl_write(stderr, format_args!("{error}\n"))?,
                },
                ReplContext::Root | ReplContext::Dm { .. } => {
                    unreachable!("registry scopes /members to room and thread")
                }
            },
            "/room" if !arguments.is_empty() => match room_ref(arguments) {
                Ok(reference) => match service.resolve_room(reference).await {
                    Ok(room) => {
                        if let Err(error) = close_repl_context(live).await {
                            repl_write(stderr, format_args!("{error}\n"))?;
                            continue;
                        }
                        repl_write(stdout, format_args!("room\t{}\t{}\n", room.id, room.name))?;
                        contexts.push(ReplContext::Room(room.id));
                    }
                    Err(error) => repl_write(stderr, format_args!("{error}\n"))?,
                },
                Err(error) => repl_write(stderr, format_args!("{error}\n"))?,
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
                        repl_write(stderr, format_args!("{}\n", CliError::InvalidCommand))?;
                        continue;
                    }
                };
                let result_id = match result_id(result) {
                    Ok(result_id) => result_id,
                    Err(error) => {
                        repl_write(stderr, format_args!("{error}\n"))?;
                        continue;
                    }
                };
                // Deterministic only: an explicit `--to`, or the single
                // downstream conversation linked by a work dependency.
                let target_conversation_id = match target {
                    Some(target) => match thread_id(target) {
                        Ok(target) => target,
                        Err(error) => {
                            repl_write(stderr, format_args!("{error}\n"))?;
                            continue;
                        }
                    },
                    None => match publish.resolve_target(source_conversation_id).await {
                        Ok(target) => target,
                        Err(error) => {
                            repl_write(stderr, format_args!("{error}\n"))?;
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
                    Ok(published) => repl_write(
                        stdout,
                        format_args!(
                            "{}\t{}\t{}\t{}\t{}\n",
                            published.publish_id,
                            published.result.id,
                            published.source_conversation_id,
                            published.target_conversation_id,
                            published.published_at,
                        ),
                    )?,
                    Err(error) => repl_write(stderr, format_args!("{error}\n"))?,
                }
            }
            "/thread" if !arguments.is_empty() => {
                let fields: Vec<_> = arguments.split_whitespace().collect();
                let (thread, agent) = match fields.as_slice() {
                    [thread] => (*thread, None),
                    [thread, flag, agent] if *flag == "--agent" => (*thread, Some(*agent)),
                    _ => {
                        repl_write(stderr, format_args!("{}\n", CliError::InvalidCommand))?;
                        continue;
                    }
                };
                let thread_id = match thread_id(thread) {
                    Ok(thread_id) => thread_id,
                    Err(error) => {
                        repl_write(stderr, format_args!("{error}\n"))?;
                        continue;
                    }
                };
                let agent = match resolve_thread_agent(service, thread_id, agent).await {
                    Ok(agent) => agent,
                    Err(error) => {
                        repl_write(stderr, format_args!("{error}\n"))?;
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
                    repl_write(stderr, format_args!("{error}\n"))?;
                    continue;
                }
                if !registered.contains(&agent.id) {
                    if let Err(error) = register_acp_agent(workspace, &agent).await {
                        repl_write(stderr, format_args!("{error}\n"))?;
                        continue;
                    }
                    registered.insert(agent.id);
                }
                if let Err(error) = close_repl_context(live).await {
                    repl_write(stderr, format_args!("{error}\n"))?;
                    continue;
                }
                match open_repl_thread(workspace, &agent, thread_id, room_id).await {
                    Ok((context, chat)) => {
                        contexts.push(context);
                        *live = Some(chat);
                        repl_write(
                            stdout,
                            format_args!("thread\t{thread_id}\t{}\n", agent.name),
                        )?;
                    }
                    Err(error) => {
                        if let Err(restore) =
                            restore_repl_context(service, workspace, contexts, live).await
                        {
                            return Err(CliError::OperationAndRestore {
                                operation: Box::new(error),
                                restore: restore.to_string(),
                            });
                        }
                        repl_write(stderr, format_args!("{error}\n"))?;
                    }
                }
            }
            "/dm" if !arguments.is_empty() => {
                let agent = match agent_ref(arguments) {
                    Ok(reference) => match service.resolve_agent(reference).await {
                        Ok(agent) => agent,
                        Err(error) => {
                            repl_write(stderr, format_args!("{error}\n"))?;
                            continue;
                        }
                    },
                    Err(error) => {
                        repl_write(stderr, format_args!("{error}\n"))?;
                        continue;
                    }
                };
                if !registered.contains(&agent.id) {
                    if let Err(error) = register_acp_agent(workspace, &agent).await {
                        repl_write(stderr, format_args!("{error}\n"))?;
                        continue;
                    }
                    registered.insert(agent.id);
                }
                if let Err(error) = close_repl_context(live).await {
                    repl_write(stderr, format_args!("{error}\n"))?;
                    continue;
                }
                match open_repl_dm(workspace, &agent).await {
                    Ok((context, dm)) => {
                        contexts.push(context);
                        *live = Some(dm);
                        repl_write(stdout, format_args!("dm\t{}\t{}\n", agent.id, agent.name))?;
                    }
                    Err(error) => {
                        if let Err(restore) =
                            restore_repl_context(service, workspace, contexts, live).await
                        {
                            return Err(CliError::OperationAndRestore {
                                operation: Box::new(error),
                                restore: restore.to_string(),
                            });
                        }
                        repl_write(stderr, format_args!("{error}\n"))?;
                    }
                }
            }
            // A registered command with arguments it does not accept.
            _ => repl_write(stderr, format_args!("{}\n", CliError::InvalidCommand))?,
        }
        navigated = matches!(spec.name, "/room" | "/thread" | "/dm" | "/back");
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

/// Rule B/C: turn `@` targets into an active work context and enter it.
/// `Ok(None)` entered; `Ok(Some(error))` is recoverable and belongs on stderr.
// The REPL threads its whole mutable context through these; a wrapper struct
// would only rename the same fields.
#[allow(clippy::too_many_arguments)]
async fn route_mentions<R: crate::application::CollaborationRuntime>(
    service: &mut CollaborationService<R>,
    workspace: &WorkspaceRuntime<AcpTransport>,
    contexts: &mut Vec<ReplContext>,
    registered: &mut HashSet<AgentId>,
    live: &mut Option<ReplChat>,
    names: &[String],
    prompt: &str,
    stdout: &mut impl Write,
) -> Result<Option<CliError>, CliError> {
    let mut agents = Vec::with_capacity(names.len());
    for name in names {
        let reference = match agent_ref(name) {
            Ok(reference) => reference,
            Err(error) => return Ok(Some(error)),
        };
        match service.resolve_agent(reference).await {
            Ok(agent) => agents.push(agent),
            Err(error) => return Ok(Some(error.into())),
        }
    }
    match agents.as_slice() {
        [agent] => enter_repl_dm(service, workspace, contexts, registered, live, agent).await,
        agents => {
            enter_repl_work(
                service, workspace, contexts, registered, live, agents, prompt, stdout,
            )
            .await
        }
    }
}

/// Rule B: one agent is direct work, which is a DM underneath.
async fn enter_repl_dm<R: crate::application::CollaborationRuntime>(
    service: &mut CollaborationService<R>,
    workspace: &WorkspaceRuntime<AcpTransport>,
    contexts: &mut Vec<ReplContext>,
    registered: &mut HashSet<AgentId>,
    live: &mut Option<ReplChat>,
    agent: &crate::domain::Agent,
) -> Result<Option<CliError>, CliError> {
    if !registered.contains(&agent.id) {
        if let Err(error) = register_acp_agent(workspace, agent).await {
            return Ok(Some(error.into()));
        }
        registered.insert(agent.id);
    }
    if let Err(error) = close_repl_context(live).await {
        return Ok(Some(error));
    }
    match open_repl_dm(workspace, agent).await {
        Ok((context, dm)) => {
            contexts.push(context);
            *live = Some(dm);
            Ok(None)
        }
        Err(error) => Ok(Some(
            recover_repl_context(service, workspace, contexts, live, error).await?,
        )),
    }
}

/// Rule C: several agents share a Work, which is a Thread underneath. The
/// first mention owns the turn; the others join as members.
// The REPL threads its whole mutable context through these; a wrapper struct
// would only rename the same fields.
#[allow(clippy::too_many_arguments)]
async fn enter_repl_work<R: crate::application::CollaborationRuntime>(
    service: &mut CollaborationService<R>,
    workspace: &WorkspaceRuntime<AcpTransport>,
    contexts: &mut Vec<ReplContext>,
    registered: &mut HashSet<AgentId>,
    live: &mut Option<ReplChat>,
    agents: &[crate::domain::Agent],
    prompt: &str,
    stdout: &mut impl Write,
) -> Result<Option<CliError>, CliError> {
    let Some(room_id) = contexts.last().unwrap().room_id() else {
        return Ok(Some(
            CollaborationError::InvalidCommand(
                "work with several agents needs a room; enter one with /room <room>".into(),
            )
            .into(),
        ));
    };
    // Membership is a precondition of the Thread, so an explicit mention adds
    // the agent and says so rather than failing or mutating silently.
    let members = match service.list_room_members(RoomRef::Id(room_id)).await {
        Ok(members) => members,
        Err(error) => return Ok(Some(error.into())),
    };
    for agent in agents {
        let joined = members
            .iter()
            .any(|member| member.left_at.is_none() && member.agent_id == agent.id);
        if joined {
            continue;
        }
        match service
            .add_room_member(AddRoomMember {
                room: RoomRef::Id(room_id),
                agent: AgentRef::Id(agent.id),
                role: None,
                changed_at: timestamp(),
            })
            .await
        {
            Ok(_) => repl_write(
                stdout,
                format_args!("member\t{}\t{}\n", agent.name, room_id),
            )?,
            Err(error) => return Ok(Some(error.into())),
        }
    }
    let thread_id = ConversationId::new();
    let title = work_title(prompt);
    if let Err(error) = service
        .create_thread(CreateThread {
            thread_id,
            primary_work_id: WorkItemId::new(),
            room: RoomRef::Id(room_id),
            title: title.clone(),
            goal: None,
            user_id: LOCAL_USER_ID.into(),
            initial_agents: agents.iter().map(|agent| AgentRef::Id(agent.id)).collect(),
            created_at: timestamp(),
        })
        .await
    {
        return Ok(Some(error.into()));
    }
    repl_write(stdout, format_args!("work\t{thread_id}\t{title}\n"))?;
    enter_repl_thread(
        service, workspace, contexts, registered, live, &agents[0], thread_id, room_id,
    )
    .await
}

/// Open a Thread for `agent` and make it the active context.
// The REPL threads its whole mutable context through these; a wrapper struct
// would only rename the same fields.
#[allow(clippy::too_many_arguments)]
async fn enter_repl_thread<R: crate::application::CollaborationRuntime>(
    service: &mut CollaborationService<R>,
    workspace: &WorkspaceRuntime<AcpTransport>,
    contexts: &mut Vec<ReplContext>,
    registered: &mut HashSet<AgentId>,
    live: &mut Option<ReplChat>,
    agent: &crate::domain::Agent,
    thread_id: ConversationId,
    room_id: RoomId,
) -> Result<Option<CliError>, CliError> {
    if !registered.contains(&agent.id) {
        if let Err(error) = register_acp_agent(workspace, agent).await {
            return Ok(Some(error.into()));
        }
        registered.insert(agent.id);
    }
    if let Err(error) = close_repl_context(live).await {
        return Ok(Some(error));
    }
    match open_repl_thread(workspace, agent, thread_id, room_id).await {
        Ok((context, chat)) => {
            contexts.push(context);
            *live = Some(chat);
            Ok(None)
        }
        Err(error) => Ok(Some(
            recover_repl_context(service, workspace, contexts, live, error).await?,
        )),
    }
}

/// A failed switch must leave the previous context live; if even that fails the
/// session cannot continue.
async fn recover_repl_context<R: crate::application::CollaborationRuntime>(
    service: &mut CollaborationService<R>,
    workspace: &WorkspaceRuntime<AcpTransport>,
    contexts: &[ReplContext],
    live: &mut Option<ReplChat>,
    error: CliError,
) -> Result<CliError, CliError> {
    match restore_repl_context(service, workspace, contexts, live).await {
        Ok(()) => Ok(error),
        Err(restore) => Err(CliError::OperationAndRestore {
            operation: Box::new(error),
            restore: restore.to_string(),
        }),
    }
}

/// A Work title from the opening prompt: the first line, trimmed to something
/// a breadcrumb can hold.
fn work_title(prompt: &str) -> String {
    let line = prompt.lines().next().unwrap_or("").trim();
    if line.is_empty() {
        return "untitled work".into();
    }
    match line.char_indices().nth(60) {
        Some((end, _)) => format!("{}…", line[..end].trim_end()),
        None => line.to_owned(),
    }
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
    input: &mut mpsc::UnboundedReceiver<ReplInput>,
    stdout: &mut impl Write,
    tui_events: Option<&mpsc::UnboundedSender<crate::tui::app::AppEvent>>,
) -> Result<(), CliError> {
    let mut cancelled = false;
    let mut permission = None;
    loop {
        tokio::select! {
            event = service.next(timestamp()) => {
                let Some(event) = event? else { return Err(CliError::EventStreamClosed); };
                if let Some(events) = tui_events {
                    let _ = events.send(crate::tui::app::AppEvent::Chat(event.clone()));
                }
                match event {
                    ChatEvent::TextDelta(text) => repl_write(stdout, format_args!("{text}"))?,
                    ChatEvent::MessageCompleted(_) => repl_write(stdout, format_args!("\n"))?,
                    ChatEvent::PermissionRequested {
                        request_id,
                        prompt: _,
                        options,
                    } => {
                        if tui_events.is_none() {
                            for (index, option) in options.iter().enumerate() {
                                repl_write(stdout, format_args!("{}. {}\n", index + 1, option.label))?;
                            }
                            repl_write(stdout, format_args!("permission> "))?;
                        }
                        permission = Some((request_id, options));
                    }
                    ChatEvent::TurnCompleted => return Ok(()),
                    ChatEvent::TurnFailed(failure) => return Err(turn_failed(failure)),
                    ChatEvent::Disconnected(reason) => return Err(CliError::Disconnected(reason)),
                }
            }
            control = input.recv(), if tui_events.is_some() || permission.is_some() => {
                let Some(control) = control else { return Err(CliError::EventStreamClosed); };
                match control {
                    ReplInput::Line(line) => {
                        let Some((request_id, options)) = permission.take() else { continue; };
                        let outcome = line?.line
                            .and_then(|line| line.parse::<usize>().ok())
                            .and_then(|index| index.checked_sub(1))
                            .and_then(|index| options.get(index))
                            .map(|option| PermissionOutcome::Selected(option.id.clone()))
                            .unwrap_or(PermissionOutcome::Cancelled);
                        service.permit(request_id, outcome, timestamp()).await?;
                    }
                    ReplInput::Permission { request_id, outcome } => {
                        let Some((pending_id, options)) = permission.take() else {
                            if let Some(events) = tui_events {
                                let _ = events.send(crate::tui::app::AppEvent::PermissionFinished(
                                    Err("no active permission request".into()),
                                ));
                            }
                            continue;
                        };
                        if pending_id != request_id {
                            permission = Some((pending_id, options));
                            if let Some(events) = tui_events {
                                let _ = events.send(crate::tui::app::AppEvent::PermissionFinished(
                                    Err("stale permission response".into()),
                                ));
                            }
                            continue;
                        }
                        let result = service.permit(request_id, outcome, timestamp()).await;
                        if let Some(events) = tui_events {
                            let _ = events.send(crate::tui::app::AppEvent::PermissionFinished(
                                result.as_ref().map(|_| ()).map_err(ToString::to_string),
                            ));
                        }
                        result?;
                    }
                    ReplInput::Cancel => {
                        if cancelled {
                            if let Some(events) = tui_events {
                                let _ = events.send(crate::tui::app::AppEvent::CancelFinished(Ok(())));
                            }
                            continue;
                        }
                        let result = service.cancel(timestamp()).await;
                        if result.is_ok() {
                            cancelled = true;
                        }
                        if let Some(events) = tui_events {
                            let _ = events.send(crate::tui::app::AppEvent::CancelFinished(
                                result.as_ref().map(|_| ()).map_err(ToString::to_string),
                            ));
                        }
                    }
                }
            }
            signal = tokio::signal::ctrl_c(), if !cancelled => {
                signal?;
                if let Some((request_id, _)) = permission.take() {
                    service
                        .permit(request_id, PermissionOutcome::Cancelled, timestamp())
                        .await?;
                }
                service.cancel(timestamp()).await?;
                cancelled = true;
            }
        }
    }
}

fn repl_input() -> mpsc::UnboundedReceiver<ReplInput> {
    let (sender, receiver) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        let stdin = io::stdin();
        let mut stdin = stdin.lock();
        loop {
            let mut line = String::new();
            let input = match stdin.read_line(&mut line) {
                Ok(0) => Ok(ReplLine {
                    line: None,
                    origin: None,
                }),
                Ok(_) => {
                    line = line.trim_end_matches(['\n', '\r']).into();
                    Ok(ReplLine {
                        line: Some(line),
                        origin: None,
                    })
                }
                Err(error) => Err(error),
            };
            let done = matches!(&input, Ok(ReplLine { line: None, .. }) | Err(_));
            if sender.send(ReplInput::Line(input)).is_err() || done {
                return;
            }
        }
    });
    receiver
}

async fn project_repl_context<R: crate::application::CollaborationRuntime>(
    service: &mut CollaborationService<R>,
    contexts: &[ReplContext],
) -> Result<crate::tui::app::Context, CliError> {
    use crate::tui::app::{Context, ContextId};

    let Some(current) = contexts.last() else {
        return Ok(Context::root());
    };
    let mut segments: Vec<String> = Vec::new();
    for context in contexts {
        match context {
            // ponytail: root only shows when it is the whole breadcrumb.
            ReplContext::Root => {}
            ReplContext::Room(room_id) => {
                let room = service.resolve_room(RoomRef::Id(*room_id)).await?;
                segments.push(format!("room::{}", room.name));
            }
            ReplContext::Dm { agent_name, .. } => segments.push(format!("dm::{agent_name}")),
            ReplContext::Thread {
                conversation_id,
                room_id,
                agent_name,
                ..
            } => {
                segments.push(format!(
                    "thread::{}",
                    thread_label(service, *room_id, *conversation_id).await?
                ));
                segments.push(agent_name.clone());
            }
        }
    }
    if segments.is_empty() {
        return Ok(Context::root());
    }
    let id = match current {
        ReplContext::Root => ContextId::root(),
        ReplContext::Room(room_id) => ContextId::new(format!("room:{room_id}")),
        ReplContext::Dm {
            conversation_id,
            agent_id,
            ..
        } => ContextId::new(format!("dm:{conversation_id}:{agent_id}")),
        ReplContext::Thread {
            conversation_id,
            agent_id,
            ..
        } => ContextId::new(format!("thread:{conversation_id}:{agent_id}")),
    };
    Ok(Context::new(id, segments.join(" > ")))
}

/// Thread title for the breadcrumb, falling back to `untitled` when the Thread
/// carries no title.
async fn thread_label<R: crate::application::CollaborationRuntime>(
    service: &mut CollaborationService<R>,
    room_id: RoomId,
    thread_id: ConversationId,
) -> Result<String, CliError> {
    Ok(service
        .list_threads(RoomRef::Id(room_id))
        .await?
        .into_iter()
        .find(|thread| thread.id == thread_id)
        .and_then(|thread| thread.title)
        .unwrap_or_else(|| "untitled".into()))
}

async fn print_repl_status<R: crate::application::CollaborationRuntime>(
    service: &mut CollaborationService<R>,
    workspace: &WorkspaceRuntime<AcpTransport>,
    context: &ReplContext,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> Result<(), CliError> {
    match context {
        ReplContext::Root => repl_write(stdout, format_args!("root\n"))?,
        ReplContext::Room(room_id) => match service.resolve_room(RoomRef::Id(*room_id)).await {
            Ok(room) => repl_write(stdout, format_args!("room\t{}\t{}\n", room.id, room.name))?,
            Err(error) => repl_write(stderr, format_args!("{error}\n"))?,
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
            repl_write(
                stdout,
                format_args!("{kind}\t{conversation_id}\t{agent_name}\t{binding_id}\t{status}\n"),
            )?;
        }
    }
    Ok(())
}

fn repl_write(output: &mut impl Write, args: fmt::Arguments<'_>) -> Result<(), CliError> {
    output.write_fmt(args)?;
    output.flush()?;
    Ok(())
}

/// Inactive typed adapter over the authoritative Phase 8 REPL state machine.
pub struct InactiveTuiBridge {
    input: mpsc::UnboundedSender<ReplInput>,
    events: mpsc::UnboundedReceiver<crate::tui::app::AppEvent>,
    buffered: VecDeque<crate::tui::app::AppEvent>,
    shutdown: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<Result<(), CliError>>,
}

impl InactiveTuiBridge {
    pub async fn open(database: impl AsRef<Path>) -> Result<Self, CliError> {
        let worker =
            StorageWorker::open(database).map_err(|error| CliError::Runtime(error.to_string()))?;
        let mut workspace =
            WorkspaceRuntime::new(worker).map_err(|error| CliError::Runtime(error.to_string()))?;
        let mut service = CollaborationService::new(workspace.storage());
        let (input_sender, mut input) = mpsc::unbounded_channel();
        let (event_sender, events) = mpsc::unbounded_channel();
        let (shutdown, mut shutdown_receiver) = oneshot::channel();
        let task = tokio::spawn(async move {
            let mut stdout = ReplCaptureWriter::new(io::sink());
            let mut stderr = ReplCaptureWriter::new(io::sink());
            let mut state = ReplSessionState::new();
            let interaction = tokio::select! {
                interaction = interact_repl_loop(
                    &mut service,
                    &workspace,
                    &mut input,
                    &mut stdout,
                    &mut stderr,
                    &mut state,
                    Some(&event_sender),
                ) => interaction,
                _ = &mut shutdown_receiver => Ok(()),
            };
            let context_shutdown = close_repl_context(&mut state.live).await;
            let interaction = match (interaction, context_shutdown) {
                (Ok(()), Ok(())) => Ok(()),
                (Err(operation), Ok(())) => Err(operation),
                (Ok(()), Err(shutdown)) => Err(shutdown),
                (Err(operation), Err(shutdown)) => Err(CliError::OperationAndContextShutdown {
                    operation: Box::new(operation),
                    shutdown: shutdown.to_string(),
                }),
            };
            let workspace_shutdown = workspace
                .shutdown(timestamp())
                .await
                .map_err(|error| CliError::Runtime(error.to_string()));
            match (interaction, workspace_shutdown) {
                (Ok(()), Ok(())) => Ok(()),
                (Err(operation), Ok(())) => Err(operation),
                (Ok(()), Err(shutdown)) => Err(shutdown),
                (Err(operation), Err(shutdown)) => Err(CliError::OperationAndShutdown {
                    operation: Box::new(operation),
                    shutdown: shutdown.to_string(),
                }),
            }
        });
        Ok(Self {
            input: input_sender,
            events,
            buffered: VecDeque::new(),
            shutdown: Some(shutdown),
            task,
        })
    }

    pub async fn execute(
        &mut self,
        command: crate::tui::app::AppCommand,
    ) -> Result<crate::tui::app::AppEvent, CliError> {
        use crate::tui::app::{AppCommand, AppEvent};

        let (origin, line) = match command {
            AppCommand::Submit { context, text } => (context, text),
            AppCommand::Execute { context, input } => (context, input),
            AppCommand::RespondPermission { .. } | AppCommand::CancelTurn => {
                return Err(CliError::InvalidCommand);
            }
        };
        self.input
            .send(ReplInput::Line(Ok(ReplLine {
                line: Some(line),
                origin: Some(origin.clone()),
            })))
            .map_err(|_| CliError::EventStreamClosed)?;
        loop {
            let event = self
                .events
                .recv()
                .await
                .ok_or(CliError::EventStreamClosed)?;
            if matches!(
                &event,
                AppEvent::CommandFinished { context, .. } if context == &origin
            ) || matches!(event, AppEvent::Exit)
            {
                return Ok(event);
            }
            self.buffered.push_back(event);
        }
    }

    pub async fn next_event(&mut self) -> Result<Option<crate::tui::app::AppEvent>, CliError> {
        Ok(match self.buffered.pop_front() {
            Some(event) => Some(event),
            None => self.events.recv().await,
        })
    }

    pub fn try_next_event(&mut self) -> Result<Option<crate::tui::app::AppEvent>, CliError> {
        Ok(match self.buffered.pop_front() {
            Some(event) => Some(event),
            None => match self.events.try_recv() {
                Ok(event) => Some(event),
                Err(mpsc::error::TryRecvError::Empty) => None,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    return Err(CliError::EventStreamClosed);
                }
            },
        })
    }

    pub fn dispatch(&self, command: crate::tui::app::AppCommand) -> Result<(), CliError> {
        use crate::tui::app::AppCommand;

        let input = match command {
            AppCommand::Submit { context, text } => ReplInput::Line(Ok(ReplLine {
                line: Some(text),
                origin: Some(context),
            })),
            AppCommand::Execute { context, input } => ReplInput::Line(Ok(ReplLine {
                line: Some(input),
                origin: Some(context),
            })),
            AppCommand::RespondPermission {
                request_id,
                outcome,
            } => ReplInput::Permission {
                request_id,
                outcome,
            },
            AppCommand::CancelTurn => ReplInput::Cancel,
        };
        self.input
            .send(input)
            .map_err(|_| CliError::EventStreamClosed)
    }

    pub async fn shutdown(mut self) -> Result<(), CliError> {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        self.task
            .await
            .map_err(|error| CliError::Runtime(error.to_string()))?
    }
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
                adapter,
                config,
            } => {
                let transport_config = match (adapter, config) {
                    (Some(id), None) => AdapterStore::open_default()?.config_for(&id, &name)?,
                    (None, Some(path)) => {
                        let value: serde_json::Value =
                            serde_json::from_str(&std::fs::read_to_string(path)?)
                                .map_err(|error| CliError::Runtime(error.to_string()))?;
                        if transport == "acp" {
                            parse_acp_config(&value)?;
                        }
                        value
                    }
                    (None, None) if transport == "acp" => return Err(CliError::MissingAdapter),
                    (None, None) => json!({}),
                    (Some(_), Some(_)) => return Err(CliError::AgentUsage),
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
            AgentOperation::Update {
                agent,
                adapter,
                config,
            } => {
                let resolved = service.resolve_agent(agent.clone()).await?;
                let name = resolved.name;
                let (transport_type, transport_config) = match (adapter, config) {
                    (Some(id), None) => (
                        "acp".to_string(),
                        AdapterStore::open_default()?.config_for(&id, &name)?,
                    ),
                    (None, Some(path)) => {
                        let value: serde_json::Value =
                            serde_json::from_str(&std::fs::read_to_string(path)?)
                                .map_err(|error| CliError::Runtime(error.to_string()))?;
                        if resolved.transport_type == "acp" {
                            parse_acp_config(&value)?;
                        }
                        (resolved.transport_type, value)
                    }
                    _ => return Err(CliError::AgentUsage),
                };
                let agent = service
                    .set_agent_transport(agent, transport_type, transport_config, timestamp())
                    .await?;
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
                    ChatEvent::PermissionRequested {
                        request_id,
                        prompt: _,
                        options,
                    } => {
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
            prompt: _,
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
    fn tui_dispatch_requires_both_standard_streams_to_be_terminals() {
        assert!(super::use_tui(true, true));
        assert!(!super::use_tui(true, false));
        assert!(!super::use_tui(false, true));
        assert!(!super::use_tui(false, false));
    }

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
