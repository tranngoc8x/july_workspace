# Architecture Hardening — Persistent Project Agents & Structured Collaboration

## Goal

Formalize the architectural principles that distinguish July from a generic multi-agent runner or shared-chat workspace.

July is a workspace for persistent project-owned agents.

Each project has a long-lived logical agent identity. Runtime sessions are temporary execution environments. Agents collaborate across project and thread boundaries through explicit structured artifacts rather than by implicitly sharing conversation history.

This phase should harden the existing architecture rather than replace it.

---

## Core Architecture Principles

### 1. Project-Owned Agent

Each project owns one persistent logical agent identity.

```text
Project
  └── Agent
       ├── Runtime Session A
       ├── Runtime Session B
       └── Runtime Session C
```

An agent is not a Claude, Codex, ACP, or other runtime process.

Runtime sessions may start, stop, crash, resume, or be replaced without changing the logical identity of the project agent.

Invariant:

```text
Agent != Runtime Session
```

---

### 2. Thread Is the Context Boundary

Threads are the primary context-isolation boundary in July.

A Room organizes collaboration.

A DM identifies a direct project-agent interaction.

Neither Room nor DM implies a shared LLM context.

```text
Room
 ├── Thread A → Context A
 ├── Thread B → Context B
 └── Thread C → Context C
```

Invariant:

```text
Room != Shared Context
Thread = Context Boundary
```

---

### 3. Results Cross Boundaries, Transcripts Don't

Conversation transcripts must not implicitly cross thread or project boundaries.

When work from Thread A is needed by Thread B, July should transfer an explicit artifact representing the useful outcome.

Allowed cross-boundary objects may include:

- Result
- Request
- Decision
- Dependency
- Work reference
- Artifact reference

Example:

```text
Thread A

messages
experiments
reasoning
implementation
    ↓
Result #42
    │
    └──────────────► Thread B
```

Thread B receives Result #42, not Thread A's complete transcript.

Invariant:

```text
Transcripts stay within their context boundary.

Explicit results and artifacts may cross boundaries.
```

This principle should apply to:

- thread linking
- cross-project work
- agent-to-agent communication
- Room coordination
- future memory/context features

---

### 4. Collaboration Is Explicit

Agents should not interact through arbitrary hidden context sharing.

Cross-agent collaboration should be represented by explicit domain objects.

Initial collaboration lifecycle:

```text
REQUEST
   ↓
ACCEPT
REJECT
COUNTER
CLARIFY
   ↓
WORK
   ↓
RESULT
   ↓
DECISION / COMPLETION
```

A project agent does not automatically have authority over another project agent.

Example:

```text
cashpoint → pay

REQUEST:
Expose payment status for cashpoint.

pay → cashpoint

COUNTER:
Use the existing GET /payments/{id} endpoint instead of
introducing a second status-specific endpoint.
```

The requester may accept, reject, clarify, or challenge the counter-proposal.

Invariant:

```text
Cross-project work requires an explicit collaboration contract.
```

---

## Domain Model Direction

Do not redesign the current persistence model solely for this phase.

Introduce these concepts incrementally when required:

```text
Project
Agent
AgentSession

Room
DM
Thread
Message

Work
Request
Result
Decision
Dependency
```

`Message` remains part of the system, but it should not become the primary coordination primitive.

July's higher-level domain should increasingly operate around:

```text
Work
Result
Decision
Dependency
```

rather than:

```text
Conversation
Message
```

---

## Thread Linking Hardening

Thread linking should reference explicit results or artifacts.

Preferred:

```text
Thread A
   ↓
Result #123
   ↓
Thread B references Result #123
```

Avoid:

```text
Thread A transcript
   ↓
copy messages
   ↓
Thread B
```

Existing thread-linking behavior should be audited against this rule.

No migration is required if the existing implementation already transfers summarized or explicit results instead of raw history.

---

## Agent Identity Hardening

Ensure agent identity is stored independently from runtime state.

Conceptually:

```text
Agent
  id
  project_id
  identity
  capabilities
  configuration

AgentSession
  id
  agent_id
  runtime
  external_session_id
  status
  started_at
  ended_at
```

Changing:

```text
Claude → Codex
```

must not create a new logical project agent.

---

## Collaboration Protocol

Introduce a minimal protocol first.

### Request

Contains:

```text
requester
target
work
context/results references
expected outcome
acceptance criteria
```

### Response

Initial response types:

```text
ACCEPT
REJECT
COUNTER
CLARIFY
```

Avoid adding complex negotiation engines in the first implementation.

The goal is explicit semantics, not autonomous debate complexity.

---

## Architectural Guardrails

The following should become hard architecture invariants:

1. A project owns a persistent logical agent identity.
2. Agent identity is independent from runtime sessions.
3. Threads are context-isolation boundaries.
4. Rooms do not imply shared LLM context.
5. Transcripts never implicitly cross thread or project boundaries.
6. Results and explicit artifacts may cross boundaries.
7. Cross-agent work uses explicit collaboration semantics.
8. No project agent has implicit authority over another project agent.
9. Runtime/provider-specific concepts must not leak into the core domain model.
10. New features should prefer references to structured artifacts over copied context.

---

## Verification

Add architecture-level tests where practical.

Examples:

### Context isolation

Verify that creating a linked thread does not automatically expose the source thread transcript.

### Result transfer

Verify that a result can be referenced from another thread without transferring unrelated messages.

### Agent persistence

Verify that changing or restarting the runtime session preserves the same logical Agent ID.

### Collaboration ownership

Verify that one agent cannot silently assign work to another project without creating an explicit collaboration request.

### Runtime independence

Verify that core domain logic does not require Claude-, Codex-, or ACP-specific identifiers.

---

## Non-Goals

This phase does NOT introduce:

- global shared memory between agents
- automatic transcript sharing
- autonomous organization hierarchy
- manager-agent hierarchy
- unrestricted agent-to-agent messaging
- complex voting systems
- multi-agent consensus algorithms
- LLM-based global supervisor
- new external memory infrastructure

These may be evaluated separately later.

---

## Expected Outcome

After this phase, July should be clearly recognizable as:

> A workspace for persistent project-owned agents with isolated contexts and structured collaboration.

The core mental model becomes:

```text
Project owns Agent
        ↓
Agent operates through Sessions
        ↓
Work happens inside isolated Threads
        ↓
Threads produce Results / Decisions
        ↓
Explicit artifacts cross boundaries
        ↓
Agents collaborate through contracts
```

Rather than:

```text
Spawn agents
   ↓
Share chat/context
   ↓
Ask them to cooperate
```

This is an architecture-hardening phase, not a rewrite.