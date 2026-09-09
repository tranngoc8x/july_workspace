//! Ephemeral Room capability. The MCP process never receives database access.
use super::{StorageHandle, timestamp};
use crate::domain::{AgentId, RoomMessageId, SendRoomMessage};
use crate::transport::RoomMessagingConfig;
use serde_json::{Value, json};
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

const MAX_REQUEST: u64 = 1024 * 1024;

pub(super) struct PublicationGuard(pub Option<Arc<AtomicBool>>);
impl Drop for PublicationGuard {
    fn drop(&mut self) {
        if let Some(alive) = &self.0 {
            alive.store(false, Ordering::SeqCst);
        }
    }
}

pub(super) struct RoomMessagingScope {
    pub config: RoomMessagingConfig,
    ready: Arc<AtomicBool>,
    alive: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<()>,
    directory: PathBuf,
}

impl RoomMessagingScope {
    pub fn new(
        storage: StorageHandle,
        message: RoomMessageId,
        agent: AgentId,
        alive: Arc<AtomicBool>,
        publications: tokio::sync::mpsc::Sender<crate::domain::RoomMessage>,
    ) -> io::Result<Self> {
        // Short path also fits macOS's Unix socket path limit.
        let directory = std::env::temp_dir().join(format!("jr-{}", ulid::Ulid::generate()));
        std::fs::DirBuilder::new().mode(0o700).create(&directory)?;
        let socket = directory.join("m");
        let listener = match UnixListener::bind(&socket) {
            Ok(listener) => listener,
            Err(error) => {
                let _ = std::fs::remove_dir(&directory);
                return Err(error);
            }
        };
        let token = format!("{}{}", ulid::Ulid::generate(), ulid::Ulid::generate());
        let config = RoomMessagingConfig {
            socket,
            token: token.clone(),
        };
        let ready = Arc::new(AtomicBool::new(false));
        let task_ready = ready.clone();
        let task_alive = alive.clone();
        let task = tokio::spawn(async move {
            // One request at a time; bounded reads keep a malformed peer from allocating unbounded memory.
            while let Ok((stream, _)) = listener.accept().await {
                let request = async {
                    let (read, mut write) = stream.into_split();
                    let mut line = String::new();
                    BufReader::new(read.take(MAX_REQUEST + 1))
                        .read_line(&mut line)
                        .await?;
                    let result = if line.len() as u64 > MAX_REQUEST {
                        Err("Room request too large".to_owned())
                    } else {
                        match serde_json::from_str::<Value>(&line) {
                            Ok(value)
                                if value["token"].as_str() == Some(&token)
                                    && task_ready.load(Ordering::SeqCst)
                                    && task_alive.load(Ordering::SeqCst) =>
                            {
                                match parse_arguments(&value["arguments"]) {
                                    Ok(request) => match publications.clone().reserve_owned().await {
                                        Ok(permit) => storage.send_agent_room_message(message, agent, request, timestamp(), task_alive.clone(), Some(permit)).await
                                            .map(|saved| json!({"message_id": saved.id.to_string(), "status": "persisted"}))
                                            .map_err(|_| "Room message rejected: inactive scope, invalid members, reply, or conflicting request_id".to_owned()),
                                        Err(_) => Err("Room messaging scope is unavailable".to_owned()),
                                    },
                                    Err(error) => Err(error),
                                }
                            }
                            _ => Err("Room messaging scope is unavailable".to_owned()),
                        }
                    };
                    let response = match result {
                        Ok(value) => json!({"result":value}),
                        Err(error) => json!({"error":error}),
                    };
                    write.write_all(format!("{response}\n").as_bytes()).await
                };
                let _ = tokio::time::timeout(std::time::Duration::from_secs(30), request).await;
            }
        });
        Ok(Self {
            config,
            ready,
            alive,
            task,
            directory,
        })
    }
    pub fn enable(&self) {
        self.ready.store(true, Ordering::SeqCst);
    }
}
impl Drop for RoomMessagingScope {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::SeqCst);
        self.task.abort();
        let _ = std::fs::remove_file(&self.config.socket);
        let _ = std::fs::remove_dir(&self.directory);
    }
}

fn parse_arguments(value: &Value) -> Result<SendRoomMessage, String> {
    let object = value.as_object().ok_or("arguments must be an object")?;
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "targets" | "body" | "reply_to" | "request_id"))
    {
        return Err(
            "Unknown argument; only targets, body, reply_to and request_id are accepted".into(),
        );
    }
    let targets = object
        .get("targets")
        .and_then(Value::as_array)
        .ok_or("targets must be an array of agent names")?
        .iter()
        .map(|target| {
            target
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .map(str::to_owned)
                .ok_or("targets must be nonempty agent names")
        })
        .collect::<Result<Vec<_>, _>>()?;
    let body = object
        .get("body")
        .and_then(Value::as_str)
        .ok_or("body must be a string")?
        .to_owned();
    let optional_string = |key| -> Result<Option<&str>, String> {
        match object.get(key) {
            None => Ok(None),
            Some(Value::String(s)) if !s.trim().is_empty() => Ok(Some(s)),
            _ => Err(format!("{key} must be a nonempty string")),
        }
    };
    Ok(SendRoomMessage {
        targets,
        body,
        reply_to: optional_string("reply_to")?
            .map(str::parse)
            .transpose()
            .map_err(|_| "invalid reply_to")?,
        request_id: optional_string("request_id")?.map(str::to_owned),
    })
}

fn tool() -> Value {
    json!({"name":"send_room_message","description":"Publish an explicit shared message in the current Room. Use targets=[] to answer the Room without waking agents; name Room agents only when requesting their attention. Set reply_to to the message being answered. Private runtime output is not published. Use request_id to safely retry the same message.","inputSchema":{"type":"object","properties":{"targets":{"type":"array","minItems":0,"items":{"type":"string","minLength":1}},"body":{"type":"string","minLength":1},"reply_to":{"type":"string"},"request_id":{"type":"string","minLength":1}},"required":["targets","body"],"additionalProperties":false}})
}

async fn publish(arguments: &Value) -> Result<Value, String> {
    parse_arguments(arguments)?;
    let socket =
        std::env::var_os("JULY_ROOM_SOCKET").ok_or("Room messaging scope is unavailable")?;
    let token =
        std::env::var("JULY_ROOM_TOKEN").map_err(|_| "Room messaging scope is unavailable")?;
    let mut stream = UnixStream::connect(socket)
        .await
        .map_err(|_| "Room messaging scope is unavailable")?;
    let request = format!("{}\n", json!({"token":token,"arguments":arguments}));
    if request.len() as u64 > MAX_REQUEST {
        return Err("Room request too large".into());
    }
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|_| "Room messaging scope is unavailable")?;
    let mut response = String::new();
    BufReader::new(stream.take(MAX_REQUEST + 1))
        .read_line(&mut response)
        .await
        .map_err(|_| "Room messaging scope is unavailable")?;
    let response: Value =
        serde_json::from_str(&response).map_err(|_| "Room messaging scope is unavailable")?;
    if let Some(error) = response["error"].as_str() {
        Err(error.into())
    } else {
        Ok(response["result"].clone())
    }
}

/// Internal stdio entrypoint launched by the ACP runtime's per-Room MCP config.
#[doc(hidden)]
pub async fn run_room_mcp_stdio() -> io::Result<()> {
    let mut input = BufReader::new(tokio::io::stdin());
    let mut output = tokio::io::stdout();
    let mut initialized = false;
    let mut ready = false;
    loop {
        let mut line = String::new();
        let count = (&mut input)
            .take(MAX_REQUEST + 1)
            .read_line(&mut line)
            .await?;
        if count == 0 {
            return Ok(());
        }
        if count as u64 > MAX_REQUEST {
            return Err(io::Error::other("MCP request too large"));
        }
        let value = serde_json::from_str::<Value>(&line);
        let response = match value {
            Err(_) => {
                json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}})
            }
            Ok(value) => {
                if !value.is_object()
                    || value["jsonrpc"] != "2.0"
                    || !value["method"].is_string()
                    || value
                        .get("id")
                        .is_some_and(|id| !id.is_string() && !id.is_number() && !id.is_null())
                {
                    let response = json!({"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":"Invalid Request"}});
                    output.write_all(format!("{response}\n").as_bytes()).await?;
                    output.flush().await?;
                    continue;
                }
                let method = value["method"].as_str().unwrap_or("");
                if value.get("id").is_none() {
                    if method == "notifications/initialized" && initialized {
                        ready = true;
                    }
                    continue;
                }
                let id = value["id"].clone();
                let result = match method {
                    "initialize" if !initialized => {
                        initialized = true;
                        let version = value["params"]["protocolVersion"]
                            .as_str()
                            .filter(|version| {
                                matches!(
                                    *version,
                                    "2024-11-05" | "2025-03-26" | "2025-06-18" | "2025-11-25"
                                )
                            })
                            .unwrap_or("2025-11-25");
                        Ok(
                            json!({"protocolVersion":version,"capabilities":{"tools":{}},"serverInfo":{"name":"july-room","version":env!("CARGO_PKG_VERSION")}}),
                        )
                    }
                    "ping" => Ok(json!({})),
                    "tools/list" if ready => Ok(json!({"tools":[tool()]})),
                    "tools/call" if ready && value["params"]["name"] == "send_room_message" => {
                        let result = tokio::time::timeout(
                            std::time::Duration::from_secs(30),
                            publish(&value["params"]["arguments"]),
                        )
                        .await;
                        let (content, is_error) = match result {
                            Ok(Ok(value)) => (value.to_string(), false),
                            Ok(Err(error)) => (error, true),
                            Err(_) => (
                                "Room publication timed out; retry with the same request_id".into(),
                                true,
                            ),
                        };
                        Ok(json!({"content":[{"type":"text","text":content}],"isError":is_error}))
                    }
                    _ => Err(json!({"code":-32601,"message":"Method unavailable"})),
                };
                match result {
                    Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
                    Err(error) => json!({"jsonrpc":"2.0","id":id,"error":error}),
                }
            }
        };
        output.write_all(format!("{response}\n").as_bytes()).await?;
        output.flush().await?;
    }
}
