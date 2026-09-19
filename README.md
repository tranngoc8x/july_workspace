# July Workspace

> **Status:** Actively developed and usable today. The core multi-project agent workflow is implemented and ready for hands-on use, while advanced collaboration capabilities continue to evolve.

July Workspace is a local-first workspace for coordinating persistent coding agents across multiple software projects.

It is built for developers who use Codex, Claude Code, ACP-compatible runtimes, or other coding agents across multiple repositories and want those agents to behave more like a persistent engineering team than isolated chat sessions.

July gives each project a durable agent identity, keeps working contexts isolated, and lets agents collaborate through explicit Results, Artifacts, Decisions, Work, and Dependencies instead of sharing entire transcripts.

**The problem July focuses on is coordination, not coding intelligence.**

Instead of putting another supervisor LLM in front of Claude, Codex, or other coding agents, July gives each project its own persistent agent and provides the workspace around them:

- direct conversations with project agents;
- Rooms and isolated Threads;
- cross-agent collaboration;
- Work, Results, Artifacts, Decisions, and Dependencies;
- session recovery;
- durable local state;
- runtime-independent agent identity;
- Room-scoped communication between July-managed agents through A2A.

July is intended for developers who work across multiple repositories and want coding agents to behave more like a persistent engineering team rather than isolated chat sessions.

Hướng dẫn tiếng Việt: [docs/21-USER-GUIDE-VI.md](docs/21-USER-GUIDE-VI.md)

---

## Installation

July is currently installed from source.

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

With both standard streams attached to a terminal, this opens the full-screen
TUI. Piped or redirected use keeps the line REPL for script compatibility.

---

## Who is July for?

July is useful when you:

- work across multiple repositories with coding agents;
- want each project to keep a persistent logical agent identity;
- need isolated conversations instead of one growing shared context;
- want agents to exchange explicit results and artifacts without leaking full working transcripts;
- want coordination state to survive runtime restarts or session replacement.

Typical examples include coordinating backend, frontend, infrastructure, and integration work across separate repositories, or keeping long-running coding-agent workflows organized without introducing another supervisor LLM.

---

## Quick start

### 1. Add a project agent

Each project has a persistent logical agent.

Example:

```bash
july agent add agent_order \
  --project ~/work/agent_order \
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
/dm agent_order
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
/dm agent_order
```

DMs preserve durable conversation/workspace state while runtime sessions may be resumed or recreated as needed.

---

### Rooms

Rooms organize agents around a product, business area, or workstream.

Example:

```text
Room: VNA

members:
- agent_order
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
agent_order integration
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
  └── native / SDK adapters (future)
```

The core workspace should not depend on provider-specific session IDs or runtime details.

ACP executes agent runtimes. A2A carries agent-to-agent interactions inside a Room through July’s collaboration bridge. RoomMessage and July Work remain canonical.

---

## Slash commands

July keeps the interactive command surface intentionally small.

### Navigation

```text
/dm <agent>
/room <room>
/thread <thread>
/thread new <title>
/new @agent  # Room: start a new session for one agent
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
├── adapters/        ACP / runtime integrations
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

July is under active development, but the core workflow is usable today.

The main architecture, July Next work, and the Phase 10 interactive TUI are implemented. The TUI provides application-owned scrollback, progressive Markdown, multiline input, context navigation, permission handling, and cancel-once active-turn behavior while preserving the existing CLI contracts.

```text
july                         both streams TTY → TUI
july                         either stream non-TTY → line REPL
july dm ...                  standalone stream
july thread open ...         standalone stream
finite command / --json      unchanged CLI output
```

The TUI deliberately has no mouse workflow, theme/plugin system, syntax
highlighting, daemon, new command grammar or transcript replay.

Room collaboration follows [Room agent communication via A2A](<docs/24-JULY WORKSPACE — ROOM AGENT COMMUNICATION VIA A2A.md>).

```text
RoomMessage → July Room bridge → A2A → target logical Agent
                                           ↓ ACP
                                     coding-agent runtime
```

Leading user mentions select Room members; later unmentioned messages activate
the same selected recipients. A new mention replaces that selection. Entering
a Room starts unselected: messages are saved without waking an agent. `/back`
clears a selection first, then returns to the previous context. Agent A2A
mentions never change the user selection.

Each agent keeps its current session in that Room across recipient switches.
`/new @pay` starts a durable new session generation for pay and selects it;
it accepts exactly one Room member and rejects invalid or busy targets. The
runtime session is created lazily on the next prompt. Room history, cursors,
and other agents’ sessions remain intact; shared context stays bounded and
incremental.

Agents publish shared replies
through the Room messaging tool; private runtime transcripts stay isolated.
Plain conversation requires no Thread or Work. Structured requests can attach
canonical Work and Results, with A2A Task IDs stored as bindings.

After restart, a new explicit message resumes or replaces the ACP session with
bounded shared context. Interrupted messages are not automatically resent.
External agents, cross-Room routing and agent-agent DM remain deferred.
See the [implementation roadmap](docs/11-IMPLEMENTATION-ROADMAP.md) for validation status.

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
