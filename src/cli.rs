use crate::application::{
    DirectMessageError, DirectMessageEvent, DirectMessageFailureKind, DirectMessageRuntime,
    DirectMessageService,
};
use crate::domain::{MemberType, PermissionOption, PermissionOutcome};
use crate::runtime::{DirectMessageBootstrapError, open_acp_direct_message};
use chrono::{SecondsFormat, Utc};
use std::ffi::OsString;
use std::io::{self, Write};
use std::path::PathBuf;
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
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Bootstrap(#[from] DirectMessageBootstrapError),
    #[error(transparent)]
    DirectMessage(#[from] DirectMessageError),
    #[error("agent turn failed: {0}")]
    TurnFailed(&'static str),
    #[error("agent transport disconnected: {0}")]
    Disconnected(String),
    #[error("agent event stream closed")]
    EventStreamClosed,
}

pub async fn run(args: impl IntoIterator<Item = OsString>) -> Result<(), CliError> {
    let agent_name = parse_agent(args)?;
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

fn parse_agent(args: impl IntoIterator<Item = OsString>) -> Result<String, CliError> {
    let mut args = args.into_iter();
    let _program = args.next();
    match (args.next(), args.next(), args.next()) {
        (Some(command), Some(agent), None) if command == "dm" => {
            agent.into_string().map_err(|_| CliError::InvalidAgentName)
        }
        _ => Err(CliError::Usage),
    }
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
