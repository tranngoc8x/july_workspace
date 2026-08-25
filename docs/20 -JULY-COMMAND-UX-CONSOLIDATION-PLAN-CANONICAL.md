# July — Command UX Consolidation Plan

## 1. Objective

Refactor July's command surface so that:

- slash commands are primarily workspace/navigation UI;
- application APIs remain the complete domain interface;
- the REPL does not become an orchestration layer;
- agents communicate through the collaboration protocol rather than user-facing commands;
- the design remains compatible with future ACP/A2A transports;
- existing Phase 1–9 + Phase 6.5 behavior is preserved.

Core principle:

```text
REPL / Slash Commands
        ↓
Presentation / Navigation
        ↓
Application Services
        ↓
Domain
        ↓
SQLite / Agent Runtime
```

Do not turn July into a slash-command agent router.

---


## 1.1 Locked decisions for this revision

The following decisions are canonical for this consolidation:

1. Explicit interactive scopes are `Root`, `Room`, `Dm`, and `Thread`; do not use a generic `Conversation` scope.
2. `/dm` and `/room` are navigation commands available from every interactive context.
3. `/thread` is available from Room and Thread contexts.
4. Thread creation uses `/thread new <title>` rather than a top-level `/thread new`.
5. `/help` supports both `/help` and `/help <command>`.
6. `/quit` is the canonical interactive exit command; `/exit` may be an alias.
7. `/back` means navigation-history pop only.
8. `/publish` target resolution is deterministic; use `--to` when the destination is ambiguous.
9. `/activity` is optional for this refactor.
10. The command registry is the only source of executable/help/completion command metadata.

---

## 2. Command taxonomy

### Navigation — core

Keep:

```text
/dm <agent>
/room <room>
/thread <thread>
/back
```

These switch conversation/workspace context. They must preserve conversation identity, not merge LLM histories, and not mutate membership merely by navigation.

### Inspection

Provide a small read-only surface:

```text
/rooms
/agents
/members
/work
/results
/activity
```

Priority:

**P0**
```text
/help
/rooms
/agents
```

**P1**
```text
/members
/work
/results
```

**Optional / follow-up**
```text
/activity
```

`/activity` is not required for this consolidation unless a clean application-level activity query already exists. Do not expand this task into a new activity-feed subsystem.

### Control

Keep a small set:

```text
/thread new <title>
/publish <result>
/restart
```

These are convenience controls, not the primary domain protocol.

---

## 3. `/help`

Add:

```text
/help
```

Help must be context-aware.

Root example:

```text
/help

Navigation
  /dm <agent>
  /room <room>
  /back

Workspace
  /rooms
  /agents

General
  /help
  /quit
```

Room example:

```text
/help

Navigation
  /dm <agent>
  /thread <thread>
  /back

Room
  /members
  /thread new <title>
```

Thread example:

```text
/help

Navigation
  /dm <agent>
  /room <room>
  /back

Thread
  /members
  /work
  /results
  /publish
```

Use one command registry as the source of truth for parser, help, completion, validation, and tests.

Also support:

```text
/help <command>
```

Examples:

```text
/help thread
/help publish
/help restart
```

Detailed help must be generated from the same registry metadata and should include:

```text
name
summary
usage
valid contexts
aliases
examples
```

Do not maintain separate hard-coded detailed help text outside the registry.

---

## 4. `/thread new`

Add:

```text
/thread new <title>
```

Example:

```text
[vna] > /thread new "Refund flow"
```

The thread belongs to the current room.

Optional goal support is acceptable:

```text
/thread new "Refund flow" --goal "Implement refund API"
```

Do not make this an option-heavy command. Advanced creation remains available through CLI/application APIs.

---

## 5. `/publish`

Keep:

```text
/publish <result>
```

as a manual convenience/override when the destination is already deterministic from the current Thread's configured link/dependency.

Also support:

```text
/publish <result> --to <target>
```

Rules:

- if exactly one deterministic downstream target exists, `/publish <result>` may use it;
- if no target exists, return a clear error;
- if multiple valid targets exist, require `--to`;
- never ask an LLM to infer the destination;
- never infer the destination from transcript content.

Normal result publication should still be possible through the collaboration/runtime layer without requiring the user to issue a command.

Do not add commands such as:

```text
/publish <agent> ...
/ask pay ...
/delegate pay ...
/send pay ...
```

---

## 6. `/restart`

Expose:

```text
/restart
```

for the current conversation.

Low-level session operations may remain in the administrative CLI for debugging, recovery, and automation.

Do not expose runtime/session identifiers in normal REPL UX.

---

## 7. Work commands

Current low-level operations may include:

```text
july work show <id>
july work block <id>
july work ready <id>
```

Target REPL behavior:

```text
/work
```

is primarily inspection.

Do not make:

```text
/work block
/work ready
```

the normal user workflow.

Work state should normally be changed by collaboration/runtime behavior:

```text
Agent
  ↓
Collaboration protocol
  ↓
Work state
```

Low-level CLI/API operations may remain for debugging, recovery, migration, automation, and tests.

---

## 8. Room and Thread CRUD

Do not remove useful administrative CLI commands such as:

```text
july room create ...
july room list
july room members ...
july room member add ...
july room member remove ...

july thread create ...
july thread list ...
july thread members ...
july thread member add ...
july thread member remove ...
july thread open ...
```

Classify these primarily as administrative/scripting APIs.

The REPL should expose common interactive operations only:

```text
/room
/thread
/thread new
/members
```

Membership mutation can remain in CLI/application APIs.

Agent onboarding/configuration follows the same separation:

```text
july agent add ...
july agent list
july agent show ...
july agent remove ...
```

while `/agents` remains an interactive inspection command.

---

## 9. Agent interaction — do not add slash commands

Do not implement:

```text
/ask pay ...
/delegate pay ...
/send pay ...
/handoff pay ...
/challenge pay ...
/approve ...
/reject ...
```

These belong to the collaboration protocol.

Example:

```text
Cashpoint
    ↓
Handoff
    ↓
Pay
    ↓
Proposal
    ↓
Challenge
    ↓
Decision
```

The user should observe or intervene only when necessary.

---


## 9.5 Agent onboarding

July needs a clear path for adding a new project-owned agent.

Do not add a REPL mutation command such as:

```text
/agent add ...
/agents add ...
```

Keep `/agents` inspection-only in the interactive REPL.

Agent lifecycle/configuration belongs to the administrative CLI and application API:

```text
july agent add <name> --project <path> [--runtime <runtime>]
july agent list
july agent show <name>
july agent remove <name>
```

`july agent update <name> ...` may be added if the current application layer already supports a clean update operation. It is not required solely for this consolidation.

### Canonical onboarding semantics

Adding an agent creates a persistent logical Agent identity and binds it to a project.

Example:

```bash
july agent add cashpoint \
  --project ~/work/cashpoint \
  --runtime codex
```

Conceptually:

```text
Project
  id: cashpoint
  root: ~/work/cashpoint
       │
       owns
       ▼
Agent
  id: cashpoint
  runtime_preference: codex
```

Important invariant:

> `july agent add` creates Agent identity/configuration. It does not create or start an AgentSession.

Runtime/session creation remains lazy.

Example:

```text
july agent add cashpoint
        ↓
persistent Agent exists
        ↓
no active model session required
        ↓
user opens /dm cashpoint
        ↓
July creates or resumes conversation/session
        ↓
Agent runtime starts/resumes as needed
```

### `/agents`

`/agents` remains read-only.

Example:

```text
> /agents

AGENT        PROJECT                    RUNTIME
cashpoint    ~/work/cashpoint           codex
pay          ~/work/pay                 claude
infra        ~/work/infra               codex
```

If no agents exist, provide actionable guidance:

```text
No agents configured.

Add a project agent with:

  july agent add <name> --project <path> --runtime <runtime>
```

### `/dm <agent>` lifecycle

Opening a DM resolves the logical agent first:

```text
/dm cashpoint
    ↓
resolve Agent identity
    ↓
find/create DM conversation
    ↓
find/resume/create AgentSession
    ↓
Agent Gateway / runtime adapter
```

The command layer must not know or require ACP session IDs, process IDs, provider-specific session IDs, or terminal identifiers.

### Agent and runtime configuration separation

Do not model the logical agent like:

```text
Agent {
    codex_session_id
    claude_session_id
    acp_process_id
}
```

Prefer separation:

```text
Agent
  id
  project_id
  name
  enabled

Project
  id
  name
  root

AgentRuntimeConfig
  agent_id
  adapter
  profile
  runtime_preference
```

Temporary runtime/session state belongs outside the persistent logical Agent identity.

This preserves:

```text
Agent != AgentSession
```

Changing runtime implementation must not create a new logical project agent.

### Room membership is separate

Adding an agent must not implicitly add it to any Room.

These are distinct operations:

```text
Agent onboarding
        !=
Room membership
```

Example:

```bash
july agent add cashpoint \
  --project ~/repos/cashpoint \
  --runtime codex

july room member add vna cashpoint
```

A single project-owned agent may belong to multiple Rooms without duplicating its identity:

```text
cashpoint
├── VNA
├── GrabGift
└── Loyalty
```

### Runtime abstraction

Normal onboarding should accept user-facing runtime preference only when needed:

```text
--runtime codex
--runtime claude
```

Do not require normal users to provide:

```text
ACP session ID
process ID
provider-specific message ID
terminal pane/tab ID
```

Runtime-specific connection details belong below the Agent Gateway / adapter boundary.

### Future A2A onboarding

Do not implement A2A onboarding as part of this consolidation.

Future external agents may eventually use an administrative form such as:

```text
july agent add external-pay --adapter a2a ...
```

but the exact external-agent syntax belongs to the A2A interoperability plan.

The current command plan only needs to preserve the abstraction so `/agents`, `/dm`, Room membership, and Thread collaboration do not depend on whether an agent is internal ACP/native or future external A2A.

---

## 10. A2A compatibility

Do not add A2A-specific slash commands.

The command layer must not depend on ACP, A2A, Claude Code, Codex, or a specific terminal tool.

The command layer operates on July concepts:

```text
Conversation
Room
Thread
Agent
Work
Result
```

Transport belongs below the collaboration layer:

```text
Slash Command
      ↓
Workspace API
      ↓
Collaboration
      ↓
Agent Gateway
      ├── ACP
      ├── A2A
      └── future transport
```

---

## 11. Command registry

Create one canonical command registry.

Conceptual model:

```rust
enum CommandScope {
    Root,
    Room,
    Dm,
    Thread,
}

enum CommandKind {
    Navigation,
    Inspection,
    Control,
}
```

Example:

```text
/dm
    Navigation
    Root | Room | Dm | Thread

/room
    Navigation
    Root | Room | Dm | Thread

/thread
    Navigation
    Room | Thread

/thread new
    Control
    Room | Thread

/work
    Inspection
    Thread

/publish
    Control
    Thread

/restart
    Control
    Dm | Thread

/help
    Inspection
    All

/quit
    Control
    All
```

The registry is the source of truth for parser, help, completion, scope validation, and tests.

---

## 12. Context-aware validation

Commands must declare valid contexts.

### Root

```text
/dm
/room
/rooms
/agents
/help
/quit
```

### Room

```text
/dm
/thread
/thread new
/members
/help
/back
```

### Thread

```text
/dm
/room
/thread
/thread new
/members
/work
/results
/publish
/help
/back
/quit
```

### DM

```text
/dm
/room
/restart
/help
/back
/quit
```

`/thread` requires a Room/Thread parent context. From a DM, use `/room <name>` first.

Invalid commands must return a clear error rather than being silently reinterpreted.

Example:

```text
> /thread new

Command unavailable in DM context.

Use /room <name> first.
```

---

## 12.5 `/quit`

`/quit` is the canonical command for leaving the interactive July REPL.

```text
/quit
```

Optional compatibility alias:

```text
/exit
```

may resolve to `/quit`, but `/quit` remains canonical.

Scope:

```text
Root | Room | Dm | Thread
```

`/quit` exits the REPL only. It must not delete workspace state, remove memberships, mark work complete, or invent a separate shutdown path. The handler should delegate to the existing runtime/application shutdown flow.

---

## 12.6 `/back` semantics

Lock `/back` to one meaning:

> `/back` pops the interactive navigation history and restores the previous workspace context.

Example:

```text
Root
  ↓ /room vna
Room: VNA
  ↓ /thread payment
Thread: payment
  ↓ /dm pay
DM: pay
  ↓ /back
Thread: payment
```

`/back` changes only active UI/navigation state.

It must not:

- leave a Room;
- remove Thread membership;
- close a conversation;
- terminate an agent session;
- merge or copy model context;
- mutate Work/Result/Dependency state.

If no previous navigation entry exists, return a clear no-op message.

Navigation history is UI state, not collaboration state.

---

## 13. CLI vs REPL separation

The target distinction is:

```text
july room create ...
```

= CLI/scripting interface

and:

```text
/room
/thread new
/dm
```

= interactive workspace interface

They do not need identical command sets.

The application layer remains the authoritative domain API.

---

## 14. Command classification

Audit every existing command and classify:

```text
KEEP
REDUCE
DEPRECATE
REMOVE
```

Initial target:

| Command | Action |
|---|---|
| `/dm` | KEEP |
| `/room` | KEEP |
| `/thread` | KEEP |
| `/back` | KEEP |
| `/help` | ADD |
| `/help <command>` | ADD |
| `/thread new` | ADD |
| `/publish` | KEEP |
| `/restart` | ADD |
| `/quit` | KEEP/ADD as canonical REPL exit |
| `/exit` | OPTIONAL alias to `/quit` |
| `/work` | KEEP, read-only |
| `/work block` | REDUCE |
| `/work ready` | REDUCE |
| `/ask` | REMOVE if present |
| `/delegate` | REMOVE if present |
| `/handoff` | REMOVE if present |
| `/activity` | OPTIONAL; only if application API already exists |
| `/agents` | KEEP, read-only |
| `july agent add/list/show/remove` | KEEP/ADD as administrative CLI/API |
| REPL `/agent add` or `/agents add` | DO NOT ADD |
| A2A commands | DO NOT ADD |

Do not delete a command solely because it is not exposed in REPL; first determine whether it is still useful as an administrative/application API.

---

## 15. Implementation sequence

### Step 1 — Audit

Inventory:

- slash commands;
- CLI commands;
- aliases;
- parser branches;
- help text;
- completion logic;
- documentation;
- tests.

Do not modify behavior yet.

### Step 2 — Classify

Record for every command:

```text
name
interface
scope
kind
handler
domain operation
keep/reduce/deprecate/remove
```

### Step 3 — Create command registry

Introduce one canonical registry.

### Step 4 — Refactor parser

Resolve commands through the registry.

### Step 5 — Implement context-aware help

Generate both:

```text
/help
/help <command>
```

from the registry.

### Step 6 — Normalize navigation

Ensure:

```text
/dm
/room
/thread
/back
```

have consistent navigation semantics.

Rules:

```text
/dm <agent>       available everywhere
/room <room>      available everywhere
/thread <thread>  available in Room or Thread
/back             pop navigation history
```

### Step 7 — Normalize interactive thread creation

Implement:

```text
/thread new
```

for the current room.

### Step 8 — Normalize inspection

Implement/standardize:

```text
/rooms
/agents
/members
/work
/results
/activity
```

according to priority.

### Step 8.5 — Normalize agent onboarding

Ensure:

```text
/agents
```

is inspection-only.

Audit or implement the administrative lifecycle:

```text
july agent add
july agent list
july agent show
july agent remove
```

Verify that:

```text
agent add
→ persist logical Agent + Project binding
→ does NOT start AgentSession
```

and:

```text
/dm <agent>
→ lazy create/resume Conversation + AgentSession
```

Do not couple agent onboarding to Room membership.

### Step 9 — Reduce domain mutations in REPL

Move normal state mutation toward:

```text
Agent
→ Collaboration
→ Application service
→ Domain state
```

while retaining low-level CLI/API operations.

### Step 10 — Regression tests

Run command, application, collaboration, and runtime suites.

### Step 11 — Documentation

Update:

```text
docs/08-RUNTIME-AND-CLI.md
```

---

## 16. Tests

### Parser tests

Cover:

```text
/dm cashpoint
/room vna
/thread abc
/back
/help
/help thread
/thread new "refund"
/work
/publish result-1
/publish result-1 --to payment-42
/restart
/quit
```

### Context tests

For each command, test:

```text
Root
Room
Thread
DM
```

and verify availability.

### Regression

Verify:

- room creation;
- thread creation;
- DM navigation;
- publish;
- session recovery;
- work state;
- collaboration;
- agent runtime.

### Registry invariants

Verify:

```text
every executable REPL command is registered
every registered command has summary + usage
every help entry resolves to a registered command
every completion entry comes from the registry
no duplicate canonical command names
no duplicate aliases
no parser-only command branches
no help-only commands
no completion-only commands
```

### Navigation history

Verify that `/back` restores the previous UI context without mutating collaboration/session state.

### Publish target resolution

Verify:

```text
1 deterministic target  → /publish <result> succeeds
0 targets               → clear error
multiple targets        → require --to
explicit --to           → deterministic target selection
```

No LLM-based target inference is allowed.

### Agent onboarding

Verify:

```text
july agent add cashpoint --project <path> --runtime codex
```

creates a persistent logical agent and project binding.

Verify:

- adding an agent does not start an AgentSession;
- `/agents` shows the new logical agent;
- `/dm cashpoint` lazily creates/resumes its runtime session;
- runtime/session restart does not replace logical agent identity;
- runtime preference can change without changing agent identity;
- adding an agent does not add Room membership;
- runtime-specific IDs do not leak into the Agent domain object;
- removing an agent follows the existing safety/reference policy and does not silently destroy unrelated Room/Thread history.

### Collaboration isolation

Verify that slash commands do not unexpectedly generate agent-to-agent messages.

---

## 17. Documentation

Update:

```text
docs/08-RUNTIME-AND-CLI.md
```

with:

```text
1. CLI
2. REPL
3. Slash Commands
4. Context Model
5. Command Registry
6. Navigation
7. Inspection
8. Control Commands
9. Administrative CLI
10. Agent Onboarding
11. Future A2A / Agent Transport
```

Explicitly document:

> Slash commands are a presentation-layer interface. They are not the collaboration protocol.

---

## 18. Acceptance criteria

- [ ] One canonical command registry exists.
- [ ] `/help` is context-aware.
- [ ] `/dm`, `/room`, `/thread`, `/back` work consistently.
- [ ] `/thread new` works inside a Room.
- [ ] `/work` is primarily inspection.
- [ ] `/publish` remains a manual convenience with deterministic target resolution.
- [ ] `/publish <result> --to <target>` handles ambiguous/multi-target cases.
- [ ] `/restart` is DM/Thread conversation-local.
- [ ] `/quit` is canonical REPL exit and is registered/tested.
- [ ] Domain mutation is not unnecessarily exposed as REPL workflow.
- [ ] No A2A-specific slash commands exist.
- [ ] No agent-routing slash commands are introduced.
- [ ] Administrative CLI commands remain available where useful.
- [ ] Parser, help, completion, scope validation, and tests share the same registry.
- [ ] No executable/help/completion-only command exists outside the registry.
- [ ] `/activity` is optional and does not expand scope into an activity subsystem.
- [ ] `/agents` is inspection-only in the REPL.
- [ ] `july agent add/list/show/remove` is available through the administrative CLI/application layer as appropriate.
- [ ] `july agent add` creates persistent Agent identity + Project binding.
- [ ] Adding an agent does not start an AgentSession.
- [ ] `/dm <agent>` lazily creates/resumes the agent conversation/runtime session.
- [ ] Agent identity survives runtime/session restart or runtime preference changes.
- [ ] Adding an agent does not implicitly mutate Room membership.
- [ ] Runtime/provider-specific session IDs do not leak into the core Agent model.

- [ ] All regression tests pass.
- [ ] Documentation is updated.
- [ ] Existing collaboration/domain protocols remain unchanged.

---

## 19. Constraints

Do not:

1. rewrite the workspace architecture;
2. change Room/Thread/DM semantics;
3. redesign the collaboration protocol;
4. implement A2A as part of this task;
5. add another agent orchestration layer;
6. delete useful administrative CLI APIs merely because they are not exposed in REPL;
7. make slash commands responsible for agent-to-agent communication;
8. add `/agent add` or `/agents add` as normal REPL mutation commands;
9. start an AgentSession merely because an Agent identity is created;
10. implicitly add a newly created Agent to a Room.

This is a command-surface consolidation, not another architecture rewrite.

---

## 20. Target UX

The interactive experience should converge toward:

```text
$ july

July
┌─ VNA
│  ├─ #payment
│  └─ #cashpoint
│
├─ DMs
│  ├─ cashpoint
│  └─ pay
│
└─ Activity

[vna] >
```

Administrative onboarding example:

```bash
july agent add cashpoint --project ~/work/cashpoint --runtime codex
july agent add pay --project ~/work/pay --runtime claude
```

Interactive REPL remains simple:

```text
/agents
/dm cashpoint
```

Typical interaction:

```text
/room vna
/thread new "Refund flow"
/thread payment-42
/dm pay
/back
/help
/help thread
```

Agents communicate independently:

```text
Cashpoint
    ↕
collaboration protocol
    ↕
Pay
```

Future transport can be:

```text
ACP / A2A / other
```

without changing the user-facing command model.

The intended result is:

> **Few slash commands, complete application APIs, autonomous agent collaboration, and a workspace UX that remains simple as July grows.**

---

## 21. Deliverables

Expected changes:

```text
src/
  command registry / parser changes
  context validation
  REPL command handlers

tests/
  command parser tests
  context/scope tests
  help tests
  agent onboarding/lifecycle tests
  regression tests

docs/
  08-RUNTIME-AND-CLI.md
  command documentation as needed
```

Do not create a new roadmap phase solely for this work. Treat it as a post-Phase-6.5 UX/architecture consolidation.
