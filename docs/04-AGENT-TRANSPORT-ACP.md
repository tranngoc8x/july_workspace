# July Workspace — AgentTransport and ACP

Status: Phase 2 implementation is complete. Live authenticated model prompts
remain opt-in readiness tests and are not claimed by the deterministic suite.

## Decision

Initial implementation:

```text
AgentTransport
└── ACPTransport
```

The adapter uses the official Rust SDK pinned as:

```toml
agent-client-protocol = { version = "=2.0.0", default-features = false }
```

Only stable ACP protocol v1 is enabled. Unstable SDK features and HTTP, RMCP,
conductor or proxy transports are not part of the initial adapter. There is no
headless fallback.

Each agent has an explicit `AcpAgentConfig` containing the executable and its
fixed arguments/environment. Moving commands such as `npx ... @latest` are
rejected.

Readiness-probed adapter profiles on 2026-08-10:

| Agent | Provisioned adapter | Observed without a model prompt |
|---|---|---|
| Codex | `@agentclientprotocol/codex-acp = 1.1.13` | Version, initialize, two creates, cancel, close and clean exit; a never-prompted session was not resumable |
| Claude | `@agentclientprotocol/claude-agent-acp = 0.66.0` | Version, initialize, create, set manual mode, close and clean exit; authenticated prompt flow was not tested |

Codex launches from an absolute `codex-acp` path with no arguments and
`NO_BROWSER=1`; `CODEX_PATH` is not overridden initially. Claude launches from
an absolute provisioned binary path with no arguments;
`CLAUDE_CODE_EXECUTABLE` is not overridden initially.

`ACPTransport` must verify `agentInfo.name`, `agentInfo.version` and stable
protocol version `1` during initialization. The pinned profiles also require
advertised `session/close` and `session/resume`; absence returns a typed
`UnsupportedCapability` error before a session is admitted. Adapter
provisioning happens outside the runtime;
July never uses `npx`, a shell, or a network install on the launch path.

Provider authentication is a separate readiness check from ACP initialization.
An adapter can be protocol-ready while its provider credentials are missing.
Before connect, July checks that the executable and project root exist and that
the adapter's state directory is writable. Missing credentials map to a typed
authentication error, not a transport retry loop.

## Why keep an interface?

July domain must not become ACP-specific.

Future possibilities:

```text
AgentTransport
├── ACPTransport
└── NativeTransport (only when needed)
```

Room/DM/Thread must not change if transport changes.

## Transport boundary

The contract uses July-owned commands, events and errors. Raw SDK types are
mapped inside `transport::acp` and do not cross into application or domain
modules.

Rust-oriented sketch:

```rust
trait AgentTransport {
    async fn connect(&mut self, agent: &AgentConnection) -> Result<(), TransportError>;
    async fn create_session(&mut self, request: CreateSession) -> Result<SessionCreated, TransportError>;
    async fn resume_session(&mut self, request: ResumeSession) -> Result<SessionResumed, TransportError>;
    async fn send_message(&mut self, request: SendMessage) -> Result<(), TransportError>;
    async fn cancel_turn(&mut self, session: SessionRef) -> Result<(), TransportError>;
    async fn respond_permission(&mut self, response: PermissionResponse) -> Result<(), TransportError>;
    async fn close_session(&mut self, session: SessionRef) -> Result<(), TransportError>;
    async fn shutdown(&mut self) -> Result<(), TransportError>;
    fn subscribe(&mut self) -> Result<TransportEvents, TransportError>;
}

struct SessionManager<T: AgentTransport> {
    transport: T,
}
```

The July-owned request types carry only application identifiers, normalized
capabilities, project roots, message content and permission choices. Every
session command uses a `SessionRef` containing the July binding ID and opaque
remote session ID. Every session event carries the binding ID; an Agent-wide
disconnect carries the Agent ID instead. `subscribe` may be called once per
connection owner and transfers the single ordered event receiver.

`create_session` does not promise that a remote session is already durable.
In particular, Codex may return a session ID before the first prompt creates a
resumable rollout. A failed resume therefore maps to `SessionLost`; July does
not infer durability from a successful create response.

The initial manager uses generic static dispatch. It does not require
`async-trait`, `dyn AgentTransport` or a transport factory.

Normalized events:

- TurnStarted
- AgentTextDelta
- AgentMessageCompleted
- ToolCallStarted
- ToolCallFinished
- PermissionRequested
- TransportDisconnected
- TurnCompleted
- TurnFailed
- UsageReported
- SessionLost

`PermissionRequested` contains a July-owned correlation ID and the advertised
options mapped to July-owned values. The corresponding `PermissionResponse`
uses that ID; no ACP SDK responder escapes the adapter.

`TransportDisconnected` is Agent-scoped and reports loss of the shared ACP
connection. `SessionLost` is session-scoped and means the remote session was
not found during resume.

Command and event channels are bounded. Events remain ordered within a
session. `AgentTextDelta` may be coalesced under pressure; permission,
turn-terminal and session-loss events are never dropped.

Initial runtime limits:

| Limit | Value |
|---|---:|
| Commands per Agent connection | 32 |
| Events per Agent connection | 256 |
| Storage commands | 64 |
| One coalesced text event | 64 KiB |
| Turn cancellation grace | 10 seconds |

Command and storage producers wait when their queues are full. Consecutive
text deltas are coalesced up to 64 KiB, after which the reader waits for event
capacity. A critical event waits for capacity or shutdown and is never
discarded. Exactly one turn may be active per remote session.

## Runtime ownership

One Tokio-owned ACP connection task is created per Agent. That connection can
host multiple sessions, but a session is still bound to exactly one logical
Conversation/Agent generation. The owner retains the task handle and owns its
shutdown and error paths; tasks are not detached.

## Ownership

July owns:
- logical conversation;
- agent identity;
- binding record;
- delivery state.

Remote agent harness owns:
- current model context;
- compaction;
- reasoning;
- tool execution internals.

## Multi-agent thread

```text
Thread VNA/payment

cashpoint → ACP session C17
pay       → ACP session P42
```

Never share one agent session between codebase owners.

## Agent-to-agent message

```text
cashpoint → @pay
```

Runtime resolves target and sends through Pay's binding.

No July semantic pass if routing is explicit.

## Permissions

Permission request flow:

```text
agent
→ ACP event
→ July policy/runtime
→ allow / deny / ask user
```

Every request receives an explicit response. Until a response is selected, the
request is fail-closed. Cancellation, runtime shutdown, or an option not
advertised by the request produces `Cancelled`; no hidden auto-approval path is
allowed. Permission requests and their decisions are recorded by the Phase 2
permission audit schema.

The Claude adapter currently creates sessions in `auto` mode. July must issue
`session/set_mode` with `default` after every create/resume and observe success
before admitting a prompt. It does not expose `auto`, `acceptEdits`, `dontAsk`,
`bypassPermissions` or a persistent allow choice. Permission requests remain
owned by the Agent connection because a background tool may request permission
after its initiating turn has ended. Phase 2 also rejects the literal `/clear`
command; session replacement belongs to the recovery design, not an adapter
shortcut.

## Cancellation

Cancellation sends the stable ACP `session/cancel` notification, then
continues to drain that session's events until a terminal event arrives. It
does not use the generic JSON-RPC `$/cancel_request` mechanism. If the agent
does not terminate the turn within a fixed 10-second grace period, July closes
the owning connection, reports `TransportDisconnected` for the Agent, and
marks every current binding on that connection `Disconnected`.

## Reconnect

Transport failure:
1. change `Active` to `Disconnected`;
2. reconnect the Agent connection;
3. attempt resume only if the agent advertises resume capability and the
   binding is the current generation;
4. if the remote session is missing, change the binding to `Lost`.

Binding lifecycle states are `Active`, `Disconnected`, `Lost` and `Closed`.
`Active` and `Disconnected` are current states; `Lost` and `Closed` are
historical states.
Resume never copies or replays a transcript. Phase 2 does not create a
replacement session generation; generation `N+1` and recovery capsules remain
Phase 7 responsibilities.

Migration `0002` enforces the typed lifecycle values, at most one current
binding per Conversation/Agent pair, and append-only permission decision
audit records.

## SQLite from Tokio

`SqliteStore` remains synchronous. A single bounded storage worker owns it and
serializes runtime storage commands. No SQLite transaction crosses an `.await`,
and the store is not placed behind a global runtime mutex.

## Shutdown

The owner stops accepting commands, resolves outstanding permissions as
`Cancelled`, sends `session/cancel` for active turns and drains terminal events
for up to the same 10-second grace period. It then closes the ACP connection,
terminates the child if still running, and awaits both child and owner task.
No task or subprocess is detached. EOF or child exit before orderly shutdown
emits one Agent-scoped `TransportDisconnected` event.

## Explicit non-goals

- terminal keystroke injection;
- stdout scraping;
- Herdr pane control as protocol;
- native provider connectors before they are required;
- transcript replay during resume;
- replacement session generation before Phase 7.
