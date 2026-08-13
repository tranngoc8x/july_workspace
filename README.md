# July Workspace

July Workspace is a greenfield Rust project for a personal, multi-project
workspace around coding agents. Previous July implementations, schemas,
commands, task systems, terminal integrations, configuration, and session
state are reference material only; this project has no backward-compatibility
or migration requirement.

See the [documentation index](docs/README.md) for the canonical project
documents.

## Current Status

Phases 0-3 are implemented. The crate contains typed ULID domain records, a
concrete synchronous `rusqlite` store, typed session lifecycle and permission
audit persistence, an official-SDK ACP adapter, and a Tokio-owned
`SessionManager` backed by a bounded SQLite worker. `july dm <agent>` opens or
reuses one durable user-to-agent DM, resumes its isolated remote session,
streams replies, handles permissions fail-closed, and persists both message
directions.

Phase 3 does not implement Room/Thread collaboration, agent-to-agent messaging,
work/result behavior, deliberation, dependency propagation, memory promotion,
replacement-session recovery, or the full CLI/REPL. Those remain later phases.

July Workspace is not a Slack clone, project-management suite, workflow engine,
Git host, IDE, terminal multiplexer, memory SaaS, agent replacement harness,
or compatibility layer. It also does not add Beads, Herdr, Zellij, tmux, a
headless CLI fallback, or an agent framework.

## Frozen Decisions

- Rust is the implementation language and Tokio is the async runtime.
- SQLite is the only canonical durable store; future SQL uses direct
  `rusqlite`, with no ORM initially.
- ACP is the initial agent protocol behind an `AgentTransport` boundary. Raw
  provider/ACP types must not enter `domain`.
- There is no terminal dependency and no headless fallback.
- Beads and other external task systems are outside the core.
- Room is a namespace; Thread is a working context; DM is first-class.
- Agent identity is not session identity. Sessions are replaceable runtime
  state.
- Conversations stay isolated; moving between DM and Thread creates a new
  conversation or capsule rather than silently transferring full history.
- Results and explicit context capsules cross conversation boundaries;
  transcripts do not.
- The codebase is discoverable memory. Do not duplicate information an agent
  can cheaply and reliably rediscover from source.

## Package And Toolchain

- Package strategy: one Cargo package, `july-workspace`, containing the
  `july_workspace` library target/crate and `july` binary target/crate. Add
  packages only when compile boundaries or reuse justify a multi-package Cargo
  workspace.
- Rust edition: `2024`.
- Declared minimum Rust version: `1.96`.
- Pinned toolchain: `1.96.0` from `rust-toolchain.toml`.
- `Cargo.lock` is tracked.
- Phase 1 uses `ulid`, bundled `rusqlite`, `serde_json`, and `thiserror`. Phase 2
  adds the exact `agent-client-protocol` 2.0.0 SDK, Tokio, and `tracing`. Phase 3
  adds `chrono` for durable UTC timestamps and only the Tokio I/O/signal
  features needed by the DM command.
  Dependencies are introduced only in the phase that needs them.

## Current Boundaries

The current package exposes `domain`, `application`, `runtime`, `transport`,
`cli`, and `storage`. `transport` owns all raw ACP SDK types. `runtime` owns session
lifecycle and the SQLite worker; it mutates durable state only through
July-owned values. `application` owns the deterministic DM policy and exposes a
transport-neutral runtime port. The `july` binary is the presentation entry
point and currently accepts only `july dm <agent>`.

The dependency direction is:

```text
CLI/presentation -> application -> domain
application -> repository/transport ports
infrastructure -> implements those ports
runtime -> application and AgentTransport
```

`domain` must remain independent of ACP, `rusqlite`, Tokio process APIs, and
terminal integrations. The CLI must not write SQLite directly; transport must
not mutate work state directly; and avoid using `Arc<Mutex<_>>` as one giant
global mutable application state boundary.

## Errors And Tracing

Use `thiserror` typed internal/domain errors when callers need to distinguish
failure cases. Use `anyhow` sparingly at outer CLI/bootstrap boundaries for
context-rich reporting; do not erase useful internal types at every layer.

Use `tracing` structured fields, not ad-hoc prints. Useful fields include
`conversation_id`, `thread_id`, `agent_id`, `session_id`, `work_id`,
`message_id`, and `transport`. Never log secrets or hidden model reasoning.

## Tests

- `src/` unit tests cover domain invariants, migration behavior, foreign-key
  restrictions, JSON checks, indexes, and FTS synchronization.
- `tests/core_sqlite.rs` covers record round trips across database reopen,
  ordering, missing records, validation, foreign keys, and transactional batch
  rollback.
- `tests/acp_transport.rs` drives the real SDK stdio path against a deterministic
  test-only ACP subprocess.
- `tests/session_runtime.rs` covers the bounded SQLite owner and durable session
  manager effects.
- `tests/dm_storage.rs` covers atomic stable-DM reuse, concurrent creation,
  message persistence, and binding lookup.
- `tests/direct_message.rs` covers exact routing, metadata, response durability,
  restart continuity, context isolation, cancellation, and session loss.
- `tests/cli_dm.rs` drives the real `july` binary against the deterministic ACP
  subprocess and verifies the resulting SQLite state.
- `tests/integration/` is reserved for broader cross-module contracts introduced
  by later phases.
- `tests/e2e/` remains reserved for broader collaboration, publish/dependency,
  and recovery workflows introduced by later phases.

## Local Verification

Run from the repository root:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
cargo build --release
```

CI runs these same four gates on every push and pull request.
