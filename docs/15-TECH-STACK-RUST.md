# July Workspace — Canonical Rust Tech Stack

## 1. Decision

July Workspace core is implemented in Rust.

This is an architectural choice for a long-running, stateful local developer runtime — not a CPU-performance optimization.

## 2. Initial stack

| Concern | Choice | Purpose |
|---|---|---|
| Language | Rust | core runtime/domain/CLI |
| Async runtime | Tokio | ACP sessions, child processes, streaming, cancellation |
| Agent protocol | `agent-client-protocol = "=2.0.0"` | stable ACP v1 client transport |
| Codex ACP adapter | `@agentclientprotocol/codex-acp = 1.1.13` | pinned external ACP agent process |
| Claude ACP adapter | `@agentclientprotocol/claude-agent-acp = 0.66.0` | pinned external ACP agent process |
| Database | SQLite | canonical durable workspace state |
| SQLite binding | rusqlite | explicit low-level SQLite access |
| Serialization | serde + serde_json | config, protocol/domain metadata JSON |
| CLI | stdlib parser in Phase 3; clap later | one current DM command; broader Phase 8 CLI |
| Observability | tracing + tracing-subscriber | structured runtime logs/spans |
| Domain errors | thiserror | typed library/domain errors |
| Application boundary errors | anyhow (sparingly) | CLI/bootstrap context-rich errors |
| IDs | ULID | stable, locally sortable identifiers; add a crate when Phase 1 implements IDs |

Do not add dependencies just because they appear in this table; add them in the phase where they become necessary.

Phase 3 uses `chrono` only for UTC RFC 3339 millisecond timestamps and enables
Tokio's standard-I/O and signal features for `july dm <agent>`. It does not add
`clap`, `anyhow` or `tracing-subscriber`; those remain unnecessary for the
single-command surface.

## 3. Why Rust fits July Workspace

July Workspace is primarily:
- async I/O;
- process/session lifecycle;
- event routing;
- durable state transitions;
- SQLite persistence;
- CLI tooling.

It is not primarily:
- model inference;
- data science;
- Python AI framework integration.

Rust therefore gives July more useful advantages in:
- typed state machines;
- ownership/lifecycle clarity;
- long-running reliability;
- low idle footprint;
- single-binary packaging.

## 4. Tokio rules

Use Tokio for:
- one owned ACP connection task per Agent, hosting multiple sessions;
- child-process stdio;
- event streams;
- cancellation/select loops;
- timers/retry backoff;
- runtime channels.

Avoid:
- blocking SQLite calls directly on latency-sensitive async executor threads;
- detached tasks with no ownership/cancellation path;
- a giant shared mutable runtime object;
- a global mutex around `SqliteStore`.

Every spawned long-lived task should have:
- an owner;
- a shutdown path;
- an error reporting path.

## 5. ACP SDK policy

Use the official ACP Rust SDK pinned to
`agent-client-protocol = { version = "=2.0.0", default-features = false }`.
The initial adapter uses stable ACP protocol v1 only. Do not enable unstable
features or HTTP, RMCP, conductor or proxy transports.

ACP is an implementation detail under `AgentTransport`.

- launch only an executable described by explicit `AcpAgentConfig`;
- reject moving command specifications such as `npx ... @latest`;
- require an absolute, pre-provisioned adapter executable and verify its
  handshake name/version; never install an adapter on the runtime path;
- keep raw ACP requests, responses, events and errors inside `transport::acp`;
- expose July-owned transport DTOs, events and typed errors;
- expose event subscription and permission response through July-owned types;
  no SDK responder crosses the adapter boundary;
- use `SessionManager<T: AgentTransport>` with static dispatch initially;
- do not add `async-trait`, a transport factory or dynamic dispatch until a
  second implementation requires it.

Command/event channels are bounded and preserve per-session ordering. Text
deltas may be coalesced under pressure; permission, terminal-state and
session-loss events must not be dropped.

Initial capacities are 32 commands, 256 events and 64 storage commands. One
coalesced text event is capped at 64 KiB. Exactly one turn may be active per
remote session.

`TransportDisconnected` is Agent-scoped; `SessionLost` is reserved for a
remote session not found during resume. Permission events carry a July-owned
correlation ID and advertised July-owned options.

Cancellation sends the stable ACP `session/cancel` notification and drains
events to a terminal state; it does not use generic JSON-RPC
`$/cancel_request`. A fixed 10-second grace period expires before the Agent
connection is closed.

Permission handling is fail-closed and always produces an explicit response.
Cancellation, shutdown and unknown options map to `Cancelled`; there is no
implicit approval.

Because ACP evolves, dependency upgrades must be intentional:
1. pin a known-good SDK version;
2. run protocol/transport tests;
3. verify Claude/Codex adapters;
4. only then upgrade the lockfile.

Do not expose raw ACP request/event structs through workspace/domain APIs.

## 6. SQLite/rusqlite policy

Use direct SQL with rusqlite.

Initial recommendation:
- SQLite bundled into the binary build where practical;
- WAL mode;
- foreign keys enabled;
- busy timeout configured;
- one controlled write path;
- one bounded storage worker that owns the synchronous store when called from
  Tokio;
- explicit immutable migrations.

Do not use an ORM initially.

Do not keep SQLite transactions alive across `.await`.
Do not wrap the store in a global runtime mutex.

Phase 2 migration `0002` adds typed session-binding states
`Active | Disconnected | Lost | Closed`, current-binding uniqueness per
Conversation/Agent pair, and append-only permission decision audit records.
`Active` and `Disconnected` are current; `Lost` and `Closed` are historical.
Resume is capability-gated and limited to the current generation. A missing
remote session becomes `Lost`; transcript replay and replacement generations
remain Phase 7 work.

## 7. Error strategy

### Domain/infrastructure errors
Use `thiserror` enums where callers need to distinguish cases.

Examples:
- invalid work transition;
- conversation missing;
- session resume unavailable;
- permission denied;
- storage conflict.

### CLI/bootstrap errors
Use `anyhow` only at outer application boundaries for context-rich reporting.

Avoid converting every internal error into `anyhow::Error`, which would erase useful types.

## 8. Logging/observability

Use `tracing` fields rather than ad-hoc print statements.

Useful fields:
- `conversation_id`;
- `thread_id`;
- `agent_id`;
- `session_id`;
- `work_id`;
- `message_id`;
- `transport`.

Never log secrets or hidden model reasoning.

The CLI may render friendly human output while structured logs remain available for diagnostics.

## 9. ID decision

ULID is the single project ID family. Its lexicographic ordering is useful for
locally sortable identifiers, which keeps related records easy to inspect and
order without introducing a second ID family. No ULID crate is added in Phase
0; add one in Phase 1 when identifiers are implemented.

## 10. Config format

Use a human-editable config format supported by Serde. TOML is the preferred default for July application configuration because it maps naturally to Rust tooling.

Example:

```toml
[agents.cashpoint]
project_root = "/repos/cashpoint"

[agents.cashpoint.transport]
type = "acp"
executable = "/opt/july/adapters/claude-agent-acp-0.66.0"
expected_agent = "@agentclientprotocol/claude-agent-acp"
expected_version = "0.66.0"
```

The path is illustrative but must be absolute. July does not invoke a shell.
Codex starts with `NO_BROWSER=1` and without a `CODEX_PATH` override. Claude
starts without a `CLAUDE_CODE_EXECUTABLE` override and must be switched from
adapter-default `auto` mode to verified manual `default` mode after every
create/resume and before a prompt. Connect preflight checks the executable,
project root, writable adapter state directory and provider authentication.

Long-lived knowledge remains Markdown; runtime state remains SQLite.

## 11. Build and quality gates

Baseline local/CI commands:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
cargo build --release
```

Optional tooling can be added later only when justified:
- nextest;
- cargo-deny;
- cargo-audit;
- coverage tooling.

Do not make Phase 0 depend on a large Rust tooling stack.

## 12. Packaging

Primary goal:

```text
one `july` executable
+
~/.july/workspace.db
+
human config/knowledge files
```

No Python runtime or virtualenv required.

Start with the developer's current macOS target, then add other targets when release/distribution becomes a real requirement.

## 13. Explicitly rejected initially

Do not add initially:
- Axum/web server;
- Tauri/desktop UI;
- Ratatui/full TUI;
- actor framework;
- SQL ORM;
- Redis/message broker;
- vector database;
- Mem0/Letta/LangGraph;
- Beads;
- terminal multiplexer dependency;
- headless CLI adapter fallback.

## 14. Guiding rule

> Prefer Rust types and a small amount of deterministic runtime code over another framework layer.

July Workspace should remain a small control plane around powerful coding agents, not become an agent framework ecosystem of its own.
