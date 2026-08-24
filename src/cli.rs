use crate::application::{
    AddRoomMember, AddThreadMember, AgentRef, CollaborationError, CollaborationService, CreateRoom,
    CreateThread, DirectMessageError, DirectMessageEvent, DirectMessageFailureKind,
    DirectMessageRuntime, DirectMessageService, MembershipChange, MembershipState, PublishError,
    PublishResult, PublishService, RemoveRoomMember, RemoveThreadMember, RoomRef,
};
use crate::domain::{
    AgentId, ConversationId, MemberType, PermissionOption, PermissionOutcome, PublishId, ResultId,
    RoomId, WorkItemId,
};
use crate::runtime::{DirectMessageBootstrapError, StorageWorker, open_acp_direct_message};
use chrono::{SecondsFormat, Utc};
use serde_json::json;
use std::ffi::OsString;
use std::io::{self, Write};
use std::path::PathBuf;
use std::str::FromStr;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, BufReader, Lines, Stdin};

const LOCAL_USER_ID: &str = "local-user";
const USAGE: &str = "usage: july dm <agent>";

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
        Command::Dm(agent_name) => run_dm(agent_name).await,
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
                PublishError::Runtime(_) => "runtime_error",
            },
            Self::MissingHome
            | Self::TurnFailed(_)
            | Self::Disconnected(_)
            | Self::EventStreamClosed => "runtime_error",
            Self::OperationAndShutdown { operation, .. } => operation.error_code(),
            Self::Json(_) => unreachable!(),
        }
    }
}

enum Command {
    Repl,
    Dm(String),
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

impl Command {
    fn json(&self) -> bool {
        matches!(
            self,
            Self::Room { json: true, .. }
                | Self::Thread { json: true, .. }
                | Self::Publish { json: true, .. }
        )
    }
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
        Some("room") => parse_room(args, json),
        Some("thread") => parse_thread(args, json),
        Some("publish") => parse_publish(args, json),
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
    let mut service = CollaborationService::new(worker);
    let interaction = interact_repl(&mut service).await;
    let mut worker = service.into_runtime();
    let shutdown = worker
        .shutdown()
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
    _service: &mut CollaborationService<R>,
) -> Result<(), CliError> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let interrupted = tokio::signal::ctrl_c();
    tokio::pin!(interrupted);
    loop {
        print!("> ");
        io::stdout().flush()?;
        let line = tokio::select! {
            line = lines.next_line() => line?,
            result = &mut interrupted => {
                result?;
                return Ok(());
            }
        };
        let Some(line) = line else {
            return Ok(());
        };
        match line.as_str() {
            "/quit" => return Ok(()),
            "/status" => println!("root"),
            "/back" => eprintln!("already at root"),
            "/members" => eprintln!("members unavailable at root"),
            _ if line.trim().is_empty() => {}
            _ => eprintln!("{}", CliError::InvalidCommand),
        }
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
            if json_output {
                let members: Vec<_> = members
                    .into_iter()
                    .map(|member| json!({
                        "room_id": member.room_id.to_string(), "agent_id": member.agent_id.to_string(),
                        "role": member.role, "generation": member.generation,
                        "joined_at": member.joined_at, "left_at": member.left_at,
                        "state": membership_state(member.left_at.is_none()),
                    }))
                    .collect();
                Some(json!(members).to_string())
            } else {
                let output = members
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
                    .join("\n");
                (!output.is_empty()).then_some(output)
            }
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

fn database_path() -> Result<PathBuf, CliError> {
    if let Some(path) = std::env::var_os("JULY_WORKSPACE_DB") {
        return Ok(path.into());
    }
    let home = std::env::var_os("HOME").ok_or(CliError::MissingHome)?;
    Ok(PathBuf::from(home).join(".july/workspace.db"))
}

async fn interact<R: DirectMessageRuntime>(
    service: &mut DirectMessageService<R>,
    agent_name: &str,
) -> Result<(), CliError> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    loop {
        print!("[{agent_name}] > ");
        io::stdout().flush()?;
        let Some(line) = next_input_or_event(service, &mut lines).await? else {
            return Ok(());
        };
        if line == "/quit" {
            return Ok(());
        }
        if line.trim().is_empty() {
            continue;
        }
        service.send_message(line, timestamp()).await?;
        drain_turn(service, &mut lines).await?;
    }
}

async fn next_input_or_event<R: DirectMessageRuntime>(
    service: &mut DirectMessageService<R>,
    lines: &mut Lines<BufReader<Stdin>>,
) -> Result<Option<String>, CliError> {
    loop {
        tokio::select! {
            line = lines.next_line() => return Ok(line?),
            event = service.next_event(timestamp()) => {
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

async fn drain_turn<R: DirectMessageRuntime>(
    service: &mut DirectMessageService<R>,
    lines: &mut Lines<BufReader<Stdin>>,
) -> Result<(), CliError> {
    let mut cancelled = false;
    loop {
        tokio::select! {
            event = service.next_event(timestamp()) => {
                let Some(event) = event? else { return Err(CliError::EventStreamClosed); };
                match event {
                    DirectMessageEvent::TextDelta(text) => {
                        print!("{text}");
                        io::stdout().flush()?;
                    }
                    DirectMessageEvent::MessageCompleted(_) => println!(),
                    DirectMessageEvent::PermissionRequested { request_id, options } => {
                        if permission(service, lines, request_id, &options).await? && !cancelled {
                            service.cancel_turn(timestamp()).await?;
                            cancelled = true;
                        }
                    }
                    DirectMessageEvent::TurnCompleted => return Ok(()),
                    DirectMessageEvent::TurnFailed(failure) => return Err(turn_failed(failure)),
                    DirectMessageEvent::Disconnected(reason) => return Err(CliError::Disconnected(reason)),
                }
            }
            signal = tokio::signal::ctrl_c(), if !cancelled => {
                signal?;
                service.cancel_turn(timestamp()).await?;
                cancelled = true;
            }
        }
    }
}

async fn handle_idle_event<R: DirectMessageRuntime>(
    service: &mut DirectMessageService<R>,
    lines: &mut Lines<BufReader<Stdin>>,
    event: DirectMessageEvent,
) -> Result<bool, CliError> {
    match event {
        DirectMessageEvent::TextDelta(text) => {
            print!("{text}");
            io::stdout().flush()?;
        }
        DirectMessageEvent::MessageCompleted(_) => println!(),
        DirectMessageEvent::PermissionRequested {
            request_id,
            options,
        } => {
            return permission(service, lines, request_id, &options).await;
        }
        DirectMessageEvent::TurnCompleted => {}
        DirectMessageEvent::TurnFailed(failure) => return Err(turn_failed(failure)),
        DirectMessageEvent::Disconnected(reason) => return Err(CliError::Disconnected(reason)),
    }
    Ok(false)
}

async fn permission<R: DirectMessageRuntime>(
    service: &mut DirectMessageService<R>,
    lines: &mut Lines<BufReader<Stdin>>,
    request_id: crate::application::DirectMessagePermissionRequestId,
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
    service
        .respond_permission(request_id, outcome, timestamp())
        .await?;
    Ok(interrupted)
}

fn turn_failed(failure: DirectMessageFailureKind) -> CliError {
    CliError::TurnFailed(match failure {
        DirectMessageFailureKind::AuthenticationRequired => "authentication required",
        DirectMessageFailureKind::Protocol => "protocol error",
    })
}

fn timestamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}
