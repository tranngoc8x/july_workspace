# July Workspace — Proposed Rust Repository Structure

## Repository layout

```text
july/
├── Cargo.toml
├── Cargo.lock
├── rust-toolchain.toml          # optional; use if we intentionally pin toolchain
├── README.md
├── docs/
│
├── src/
│   ├── main.rs
│   ├── lib.rs
│   │
│   ├── cli/
│   │   ├── mod.rs
│   │   ├── repl.rs
│   │   └── commands/
│   │
│   ├── domain/
│   │   ├── mod.rs
│   │   ├── agent.rs
│   │   ├── room.rs
│   │   ├── conversation.rs
│   │   ├── message.rs
│   │   ├── work.rs
│   │   ├── result.rs
│   │   └── dependency.rs
│   │
│   ├── application/
│   │   ├── mod.rs
│   │   ├── workspace.rs
│   │   ├── conversations.rs
│   │   ├── messaging.rs
│   │   ├── work.rs
│   │   ├── publish.rs
│   │   ├── memory.rs
│   │   └── recovery.rs
│   │
│   ├── runtime/
│   │   ├── mod.rs
│   │   ├── agent_runtime.rs
│   │   ├── session_manager.rs
│   │   ├── cancellation.rs
│   │   └── events.rs
│   │
│   ├── transport/
│   │   ├── mod.rs
│   │   └── acp.rs
│   │
│   ├── storage/
│   │   ├── mod.rs
│   │   ├── sqlite.rs
│   │   ├── worker.rs
│   │   ├── repositories/
│   │   └── migrations/
│   │
│   ├── memory/
│   │   ├── mod.rs
│   │   ├── checkpoint.rs
│   │   └── assembler.rs
│   │
│   ├── config/
│   │   ├── mod.rs
│   │   ├── loader.rs
│   │   └── models.rs
│   │
│   └── error.rs
│
├── tests/
│   ├── integration/
│   └── e2e/
│
└── .july/
    ├── agents/
    │   ├── cashpoint.md
    │   └── pay.md
    └── rooms/
        └── vna.md
```

## Layer rules

### `domain/`

Pure July concepts and invariants.

Allowed dependencies should be minimal (`serde` only if serialization on domain DTOs genuinely helps).

Must not depend on:
- ACP SDK;
- rusqlite;
- Tokio process APIs;
- Herdr/Zellij/tmux.

Strongly model state using enums/newtypes rather than free-form strings.

Example:

```rust
pub enum WorkStatus {
    Open,
    Working,
    Blocked,
    Ready,
    Done,
    Failed,
    Cancelled,
}
```

### `application/`

Deterministic use cases coordinating domain + ports/repositories.

Examples:
- send message;
- create thread;
- add member;
- publish result;
- update dependency;
- recover lost session.

### `runtime/`

Tokio-based lifecycle management.

Owns:
- long-lived tasks;
- session lifecycle;
- cancellation;
- normalized runtime events.

### `transport/`

External agent protocol boundary.

`acp.rs` is the only initial implementation.
Raw ACP SDK types must not escape this module into the domain.

### `storage/`

SQLite implementation using `rusqlite`.

Prefer explicit repository functions/direct SQL over a large ORM.
Do not hold database transactions across `.await`.

### `.july/agents/*.md`

Human-maintained non-obvious project knowledge.

### `.july/rooms/*.md`

Human-maintained shared business/workstream knowledge.

Runtime transcripts/work state belong in SQLite, not Markdown.

## Dependency direction

```text
CLI
 ↓
Application
 ↓
Domain

Application → repository/transport ports
Infrastructure → implements those ports
Runtime → calls Application and AgentTransport
```

Avoid:
- domain importing ACP;
- domain importing rusqlite;
- CLI writing SQLite directly;
- ACP transport mutating work state directly;
- global mutable application state behind one giant `Arc<Mutex<_>>`.

## Package and target strategy

Start with one Cargo package, `july-workspace`, containing the
`july_workspace` library target/crate and the `july` binary target/crate.

Do not create a multi-package Cargo workspace with internal packages until
compile boundaries or reuse provide a concrete benefit. Modules are sufficient
for the initial architecture.
