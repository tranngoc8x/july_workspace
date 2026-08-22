# July Workspace — Implementation Roadmap

## Phase 0 — Project Foundation

### Deliverables

- new Rust repository/project;
- `Cargo.toml`;
- accepted domain vocabulary;
- accepted ADRs;
- canonical tech-stack document;
- module dependency rules;
- initial CI.

### Rust foundation

- Rust edition/toolchain policy documented;
- minimal dependencies only;
- `cargo fmt --check`;
- `cargo clippy --all-targets --all-features -- -D warnings`;
- `cargo test --workspace`;
- tracing/error conventions;
- raw provider/ACP types forbidden from `domain`.

### Architecture decisions that must be frozen

- SQLite is canonical durable storage;
- ACP is the initial transport;
- no Beads;
- no terminal dependency;
- no headless fallback initially;
- Room is namespace;
- Thread is working context;
- DM is first-class;
- Agent identity != session;
- Results cross boundaries, transcripts do not.

### DoD

A developer can clone the new repository and understand the project without reading any previous July codebase.

---

## Phase 1 — SQLite + Core Domain

Implement:

- schema migrations;
- Agent;
- Room/member;
- Conversation/member;
- Message;
- WorkItem;
- Result;
- Dependency;
- SessionBinding;
- Checkpoint/Memory records.

DoD:

- persistence survives restart;
- transaction tests pass;
- domain has no ACP/provider imports;
- schema begins at the new project baseline; no historical migrations.

---

## Phase 2 — AgentTransport + ACP

Status: complete. Deterministic acceptance uses a test-only ACP JSON-RPC
subprocess; provider-authenticated prompt smoke remains opt-in.

Implement:

- `AgentTransport` trait;
- `ACPTransport`;
- `SessionManager`.

Prove:

- connect;
- create session;
- send;
- streaming events;
- resume;
- cancel;
- close;
- permission request.

Rust-specific checks:

- ACP tasks run under Tokio with explicit cancellation;
- raw ACP SDK types remain inside transport/infrastructure boundaries;
- streams have a backpressure strategy;
- errors map into typed July errors.

DoD:

- no stdout scraping;
- no terminal-control dependency;
- session binding persists in SQLite.

---

## Phase 3 — DM MVP

Status: complete. The deterministic suite drives the real `july` binary through
the official ACP SDK against a test-only ACP subprocess; authenticated provider
prompts remain opt-in.

Implement:

```text
july dm <agent>
```

Features:

- create/open DM;
- create/resume remote session;
- persist messages;
- direct explicit routing;
- project-root scoping;
- restart continuity.

DoD:

- no July LLM call for explicit target;
- session resumes after restart;
- remote-session loss has a defined error/recovery path;
- token overhead baseline measured against direct agent usage.

Baseline: for an explicit target, July sends the user's exact message body with
zero July-injected bytes. Therefore the model-visible prompt-content token delta
against sending the identical text directly is zero; SQLite metadata, routing
IDs and lifecycle bookkeeping are not injected into the prompt.

---

## Phase 4 — Room + Thread MVP

Implemented and verified. Phase 4 adds migration `0003`, the explicit
application command surface, guarded SQLite membership/aggregate operations,
and targeted isolated Thread session startup. Room/Thread shell commands remain
deferred to Phase 8.

Implement:

- explicit application commands for Room/Thread create, list and membership;
- generational Room and Thread membership with idempotent add/remove;
- targeted `OpenThreadForAgent(thread_id, agent_id)`;
- atomic Thread + members + primary Work creation;
- separate agent session per conversation.

The primary Work starts `Open`, mirrors the Thread title/goal and has no owner
until Phase 6. Session/ACP startup occurs lazily after the durable create
transaction commits. The locked future CLI uses explicit `--room` and
`--agent`; `room use`, REPL context and `--json` remain Phase 8.

Do not implement in Phase 4:

- mentions or dynamic join;
- Agent-originated membership mutation;
- Work ownership/lifecycle behavior;
- A2A, deliberation or structured decision artifacts.

DoD:

- same agent can join multiple threads without context leakage;
- room history is not injected wholesale;
- non-member agents do not receive thread context;
- Thread creation cannot leave partial members or primary Work;
- rejoin preserves prior membership history;
- Room removal cannot silently cascade active Thread memberships.

Coverage lives in `tests/phase4_storage.rs`, `tests/phase4_application.rs` and
`tests/thread_runtime.rs`. The runtime tests also prove that admission happens
before transport, transport startup failure leaves the durable aggregate
intact, and no Room or other-Thread transcript is injected.

---

## Phase 5 — Agent-to-Agent Messaging

### Phase 5.1 — Explicit Agent-to-Agent DM

Status: implemented and verified. The current slice proves exact active
unordered Agent pair reuse, deterministic typed target routing through an
existing shared target owner, exact-body source delivery, and durable
source/target attribution. It has no semantic router.

### Phase 5.2 — Thread mention and dynamic member join

Status: implemented and verified. A typed mention names one source and one
target Agent. One SQLite transaction validates both scopes, joins or rejoins
the target, and persists the attributed source message. The target's existing
shared owner opens or resumes an isolated Thread session; a new join receives
the caller-provided capsule before the exact message body. Exact message
replays are durable no-ops and transport failures do not roll back persistence.

### Phase 5.3 — Offline persistence and delivery

Status: implemented and verified. Each message has one explicit target
`message_deliveries` row. The message and `PENDING` row are persisted before
transport; transport acceptance transitions it to terminal `DELIVERED`.
Ordinary owner/open/send failures transition it to `FAILED` and return a typed
structured failure. Explicit retry claims only `FAILED`, reuses the stored
exact target/body, and preserves target-only routing. Thread retry revalidates
active Agent, Room, and Thread membership without implicit rejoin; a persisted
join/rejoin capsule has separate delivery progress and is not resent after
success.

Delivery is at-least-once: a crash after transport acceptance and before the
`DELIVERED` write can result in duplicate delivery on explicit retry. Exactly
once is not promised. Phase 5.3 adds no daemon, automatic backoff, CLI, semantic
routing, or new public syntax.

### Phase 5.4 — Restart, isolation, and delivery regression tests

Status: planned.

Remaining implementation:

- Phase 5.4 restart and cancellation-related `PENDING` reconciliation,
  isolation, and delivery regression tests.

DoD:

- explicit routing is deterministic;
- no semantic coordinator for explicit recipient;
- target receives only relevant context/capsule.

---

## Phase 6 — Work / Result / Publish / Dependency

Implement:

- WorkItem lifecycle;
- structured Result;
- Result version/supersede semantics;
- Publish;
- dependency graph;
- READY propagation.

DoD:

- Thread A can unblock Thread B;
- publish transfers structured Result, not transcript;
- dependency transitions are transactional/idempotent.

---

## Phase 6.5 — Agent Deliberation & Decision Protocol

Detailed plan:

```text
16-AGENT-DELIBERATION-UPGRADE-PLAN.md
```

Prerequisites:

- Phase 4 — Room + Thread;
- Phase 5 — Agent-to-Agent Messaging;
- Phase 6 — Work / Result / Publish / Dependency.

Implement:

- Handoff / ownership negotiation;
- ACCEPT / REJECT / PARTIAL / DISPUTED;
- evidence-backed ownership responses;
- bounded dispute rounds;
- Proposal + SUPPORT / CHALLENGE / AMEND / REJECT;
- durable Decision;
- optional `decision_owner`;
- Decision → WorkItem conversion;
- escalation to `NEEDS_DECISION`.

Do not:

- create unrestricted agent debate loops;
- introduce a generic Meeting framework;
- require an LLM facilitator;
- use voting as the default decision mechanism.

DoD:

- two agents can dispute ownership without infinite ping-pong;
- evidence and provenance survive the discussion;
- unresolved disputes escalate deterministically;
- a proposal can become a durable Decision;
- Decision can generate WorkItems/dependencies;
- all state survives restart;
- normal DM/Thread workflows remain unaffected.

---

## Phase 7 — Memory + Session Recovery

Implement:

- checkpoint creation;
- memory promotion;
- recovery capsule;
- replacement session generation;
- bounded recent-message replay.

DoD:

- remote session can be intentionally deleted;
- replacement session continues work from durable state;
- full transcript replay is unnecessary;
- unverified hypotheses are not promoted automatically.

---

## Phase 8 — CLI / REPL

Implement:

```text
/dm
/room
/thread
/back
/members
/status
/publish
```

Also support machine-readable `--json` where operationally useful.

DoD:

- common workflow is faster than manually juggling multiple coding-agent terminals;
- switching UI context does not merge LLM contexts.

---

## Phase 9 — Packaging / Release

Implement:

- release build;
- macOS target(s) used for development;
- version/checksum output;
- safe install/uninstall behavior;
- optional Homebrew packaging when stable.

DoD:

- July runs as one release binary;
- no Python runtime required;
- SQLite migrations run/check automatically on startup;
- `july --version` works;
- uninstall does not remove workspace data accidentally.

---

## Cross-phase metrics

### DM overhead

Compare July DM token/context overhead with direct agent use.

### Context isolation

Measure irrelevant-context leakage.

### Recovery efficiency

Compare recovery capsule size with full transcript size.

### Collaboration efficiency

Count manual copy/paste/context-switch operations avoided.

### Reliability

Track failed delivery, lost session, duplicate publish and invalid state transition rates.
