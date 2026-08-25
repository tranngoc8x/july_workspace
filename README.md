# July Workspace

**July Workspace** is a local-first workspace for working with multiple persistent coding agents across multiple projects.

July is built around a simple idea:

> Coding agents are already strong. The next bottleneck is coordination, not coding intelligence.

Instead of putting another supervisor LLM in front of Claude, Codex, or other coding agents, July gives each project its own persistent agent and provides the workspace around them:

- direct conversations with project agents;
- Rooms and isolated Threads;
- cross-agent collaboration;
- Work, Results, Artifacts, Decisions, and Dependencies;
- session recovery;
- durable local state;
- runtime-independent agent identity;
- future interoperability with external agents through A2A.

July is intended for developers who work across multiple repositories and want coding agents to behave more like a persistent engineering team rather than isolated chat sessions.

---

## Installation

> The exact installation command depends on how the repository is currently packaged. If the project already provides a release installer or package command, use that instead of the development commands below.

### Build from source

Requirements:

- Rust toolchain
- Cargo
- SQLite
- a supported coding-agent runtime / ACP-compatible setup

Clone the repository:

```bash
git clone https://github.com/tranngoc8x/july_workspace.git
cd july_workspace
```

Build:

```bash
cargo build --release
```

Run directly:

```bash
cargo run
```

Or install the binary locally:

```bash
cargo install --path .
```

Then start July:

```bash
july
```

---

## Quick start

### 1. Add a project agent

Each project has a persistent logical agent.

Example:

```bash
july agent add cashpoint \
  --project ~/work/cashpoint \
  --runtime codex
```

Adding an agent registers the project and agent identity in July.

It does **not** immediately start a model session.

### 2. Start July

```bash
july
```

### 3. Open a DM with an agent

```text
/dm cashpoint
```

July resolves the logical agent and lazily creates or resumes the runtime session when necessary.

### 4. Create or enter a Room

```text
/room vna
```

### 5. Create a Thread

```text
/thread new "Refund flow"
```

Then work inside the isolated Thread context.

### 6. Get help

```text
/help
```

or:

```text
/help thread
```

---

## Main features

### Persistent project agents

A project owns a persistent logical agent.

The agent identity survives runtime restarts and session replacement.

```text
Project
  └── Agent
        ├── DM session
        ├── Thread session
        └── replacement runtime session
```

This means:

```text
Agent != AgentSession
```

A Codex, Claude, ACP, or future runtime session is an execution detail, not the agent's identity.

---

### Direct messages

Use a DM when you want to work directly with one project agent.

```text
/dm cashpoint
```

DMs preserve durable conversation/workspace state while runtime sessions may be resumed or recreated as needed.

---

### Rooms

Rooms organize agents around a product, business area, or workstream.

Example:

```text
Room: VNA

members:
- cashpoint
- pay
- infra
```

A Room is a collaboration namespace.

It is **not** one shared LLM context.

---

### Isolated Threads

Threads are concrete collaboration contexts inside a Room.

Example:

```text
VNA
├── payment-callback
├── refund
└── voucher-pending
```

Each Thread is an independent context boundary.

This avoids feeding every project agent the full history of every discussion in the Room.

---

### Cross-agent collaboration

Project agents can collaborate through explicit July domain state rather than hidden transcript sharing.

The collaboration flow can include:

```text
Request
  ↓
Accept / Reject / Counter / Clarify
  ↓
Work
  ↓
Result
  ↓
Decision / Completion
```

Agents do not automatically have authority over other project agents.

They can reject, clarify, counter, or challenge cross-project work.

---

### Work and dependencies

July keeps durable coordination state for engineering work.

This includes concepts such as:

- Work / Task
- Result
- Artifact
- Decision
- Dependency

Example:

```text
auth-api
   ↓ READY
cashpoint integration
   ↓ READY
E2E
```

The workspace can track dependencies without requiring a supervisor LLM to reason about every transition.

---

### Results instead of transcript sharing

A central July rule is:

> **Results cross boundaries; transcripts don't.**

Example:

```text
Thread A
  ├── messages
  ├── experiments
  └── Result #42
        │
        ▼
Thread B references Result #42
```

Thread B receives the explicit result or artifact, not Thread A's private working transcript.

This helps reduce context growth and keeps collaboration traceable.

---

### Session recovery

July owns durable workspace state while agents own their active model context.

If a runtime session disappears, July can preserve the logical agent and conversation and recreate the runtime session from durable state such as:

```text
Agent identity
+ project
+ current work
+ checkpoint
+ bounded recent messages
+ Results / Decisions / Artifacts
```

A full historical transcript replay should not be required.

---

### Local-first storage

July uses SQLite as its canonical local persistence layer.

Durable state can include:

- projects;
- agents;
- conversations;
- Rooms;
- Threads;
- Work;
- Results;
- Artifacts;
- Decisions;
- Dependencies;
- runtime bindings;
- recovery metadata.

The workspace is designed to remain lightweight and local-first.

---

### Runtime abstraction

July separates logical agents from their execution transport.

Conceptually:

```text
July
  ↓
Agent Gateway
  ├── ACP
  ├── native / SDK adapters
  └── A2A
```

The core workspace should not depend on provider-specific session IDs or runtime details.

A2A support is intended to live at this adapter boundary rather than becoming July's internal task model.

---

## Slash commands

July keeps the interactive command surface intentionally small.

### Navigation

```text
/dm <agent>
/room <room>
/thread <thread>
/thread new <title>
/back
```

### Inspection

```text
/agents
/rooms
/members
/work
/results
```

### Control

```text
/publish <result>
/restart
```

### General

```text
/help
/help <command>
/quit
```

Slash commands are primarily a workspace/navigation interface.

They are not the agent-to-agent collaboration protocol.

---

## Administrative CLI

Administrative and scripting operations live outside the interactive REPL.

Examples:

```bash
july agent add ...
july agent list
july agent show ...
july agent remove ...

july room create ...
july room list
july room member add ...
july room member remove ...

july thread create ...
july thread list ...
```

The REPL and CLI do not need identical command sets.

Both should operate through the same application/domain layer.

---

## Project structure

The exact directory names may evolve, but the project is organized around a few main responsibilities:

```text
src/
├── domain/          core July entities and state rules
├── application/     use cases and application services
├── storage/         SQLite persistence
├── runtime/         agent sessions and lifecycle
├── adapters/        ACP / future A2A / runtime integrations
├── collaboration/   Work, Result, Decision, Dependency flows
├── commands/        CLI / REPL command handling
└── main.rs          application entry point

tests/
└── integration and architecture-level tests

docs/
└── design and architecture documentation
```

The important boundary is:

```text
REPL / CLI
    ↓
Application Services
    ↓
Domain
    ↓
Storage / Runtime adapters
```

The command layer should not become an orchestration layer.

---

## Architecture in brief

July follows several core rules:

1. A project owns a persistent logical agent.
2. Agent identity is independent from runtime sessions.
3. Threads are context-isolation boundaries.
4. Rooms do not imply shared model context.
5. Transcripts do not implicitly cross Thread or project boundaries.
6. Results and explicit artifacts may cross boundaries.
7. Cross-agent work uses explicit collaboration semantics.
8. No project agent has implicit authority over another project agent.
9. Runtime/provider-specific concepts stay outside the core domain.
10. Structured references are preferred over copied context.

The responsibility split is:

```text
Agents
  own reasoning and active model context

July
  owns durable workspace state,
  coordination,
  routing,
  lifecycle,
  isolation,
  and references
```

---

## Technology

July is implemented primarily in Rust.

Core technologies include:

```text
Rust
├── Tokio
├── SQLite
├── rusqlite
├── serde / serde_json
├── clap
├── tracing
└── ACP integration
```

---

## Current status

The main July architecture and the July Next work have been implemented.

Current work is focused on simplifying and consolidating the interactive command UX:

```text
Command UX consolidation
├── canonical command registry
├── /help
├── /help <command>
├── normalized navigation
├── /thread new
├── /back semantics
├── /quit
├── agent onboarding UX
└── command/scope regression tests
```

The next major interoperability area is **A2A**.

A2A is intended to work as an external agent adapter:

```text
July Task
   ↓
Agent Gateway
   ↓
A2A Adapter
   ↓
External A2A Agent
```

July's own task and collaboration state remains canonical.

---

## What July is not

July is not:

- a supervisor LLM that re-reasons every user request;
- a shared transcript for every agent;
- a terminal multiplexer;
- a generic agent chat application;
- an ACP-specific shell;
- an A2A-specific shell;
- a replacement for coding-agent intelligence.

July exists to make strong coding agents easier to coordinate across real engineering work.

---

## Guiding principle

> **July routes conversations; agents own reasoning; results cross boundaries, transcripts don't.**
