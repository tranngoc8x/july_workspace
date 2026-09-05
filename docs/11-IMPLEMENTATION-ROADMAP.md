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

Status: complete. Phases 5.1-5.4 implement and verify the original Phase 5
scope: deterministic Agent DM routing, Thread mentions with dynamic join,
durable offline delivery, and restart reconciliation with context isolation.
Delivery remains at-least-once; exactly-once delivery, a daemon, automatic
retry/backoff, semantic routing, and messaging CLI remain out of scope.

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

Status: implemented and verified. Startup reconciliation transitions persisted
`PENDING` deliveries to `FAILED` before the storage worker becomes ready; it
does not send or automatically retry them. Explicit failed-only retry after
restart reuses the stored target/body. Thread retries preserve successful
capsule progress and do not resend that capsule. DM and Thread process-loss
tests also prove target/body and sibling-context isolation.

Delivery remains at-least-once: a crash after transport acceptance before the
`DELIVERED` write can duplicate the exact body on a later explicit retry.
Exactly-once delivery, a daemon, automatic retry/backoff, and messaging CLI
remain out of scope for this phase.

---

## Phase 6 — Work / Result / Publish / Dependency — Implemented

Status: implemented and verified.

Implemented evidence:

- WorkItem lifecycle and ownership — `tests/work_lifecycle.rs`;
- structured immutable Result and atomic first Result + `READY` —
  `tests/work_results.rs`;
- same-Work Result correction/supersede semantics — `tests/work_results.rs`;
- structured Publish with natural-key idempotency and no transcript copy —
  `tests/result_publish.rs`;
- durable dependency graph with missing-Work, self-edge, and cycle rejection —
  `tests/work_dependencies.rs`;
- transactional `READY`, `FAILED`, and Result-correction propagation to only
  matching outgoing edges — `tests/work_dependencies.rs`.

Verified DoD:

- Thread A's Result becomes a structured dependency outcome consumable by
  Thread B's Work without automatically changing that downstream Work —
  `tests/work_dependencies.rs`;
- Publish transfers the immutable structured Result and source reference, not
  transcript Messages — `tests/result_publish.rs`;
- dependency transitions are transactional and exact retries are no-ops,
  including forced SQLite rollback coverage — `tests/work_dependencies.rs`.

Phase 6 adds no CLI, external A2A protocol integration, semantic routing,
transcript transfer, background dependency reconciliation/retry, or automatic
downstream Work status change. Phase 6.5 and Phase 8 remain future
roadmap phases.

---

## Phase 6.5 — Agent Deliberation & Decision Protocol

Status: implemented and verified.

Implemented evidence:

- Handoff ownership negotiation with ACCEPT/REJECT/PARTIAL, attached evidence,
  alternate owner, one open negotiation per Work, and rejected work that is
  never silently reassigned — migration `0012`, `tests/handoff_storage.rs`;
- bounded disputes: a challenge inside the round budget reopens the proposal,
  an exhausted budget marks the Handoff `DISPUTED`, opens a `NEEDS_DECISION`
  ownership Decision and stops all further turns — migration `0013`,
  `tests/decision_storage.rs`;
- durable Decisions with user or named-agent owners, supersede chains that keep
  the outcome they once stated, and ownership that moves only because a
  Decision said so — `tests/decision_storage.rs`;
- Proposals with SUPPORT/CHALLENGE/AMEND/REJECT, disagreement that must carry
  actionable content, revision by supersede, and withdrawal — migration `0014`,
  `tests/proposal_storage.rs`;
- explicit, auditable and idempotent Decision → WorkItem/Dependency conversion —
  migration `0015`, `tests/decision_work.rs`;
- the application boundary `DeliberationService` over `StorageWorker` with
  caller-actionable typed errors — `tests/deliberation_application.rs`;
- the full section 16 scenario end to end, including loop prevention, restart
  persistence and thread context isolation — `tests/deliberation_e2e.rs`.

Phase 6.5 adds no CLI surface, no semantic facilitator, no Thread phase field,
and no voting or scoring engine; those stay deferred as the plan requires.

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

## Phase 7 — Memory + Session Recovery — Implemented

Status: implemented and verified.

Implemented evidence:

- explicit checkpoint creation, same-Conversation anchor validation, and
  deterministic latest lookup scoped by Conversation/Agent —
  `tests/checkpoint_storage.rs`;
- explicit typed/scoped memory promotion requiring a real source Conversation,
  rejecting self/cross-scope supersession, and never auto-promoting Messages or
  Results — `tests/memory_promotion.rs`;
- deterministic `JULY_RECOVERY_V1` JSON capsule containing project memory,
  relevant Room memory, latest checkpoint, compact Result/reference links, and
  at most the newest 20 post-anchor Messages in chronological order plus their
  included count/truncation metadata —
  `tests/recovery_capsule.rs`;
- atomic generation `N -> N+1` replacement with a durable pending capsule —
  `tests/session_recovery_storage.rs`;
- DM and Thread replacement through the shared per-Agent owner, normal resume
  without replay, pending-capsule retry on the same unattached `N+1` binding,
  stale-generation isolation, and terminal `Closed` handling —
  `tests/direct_message.rs` and
  `tests/thread_runtime.rs`.

Verified DoD:

- missing/provider-lost remote sessions create and continue through replacement
  generation `N+1` from durable scoped state;
- full transcript replay is not used;
- unverified hypotheses are never promoted automatically.

Recovery capsule delivery is at-least-once across ambiguous transport/storage
failure. Phase 7 adds no semantic memory/vector database, background retry
daemon, exactly-once transport claim, broader CLI, or mandatory live-provider
smoke; live smoke remains opt-in.

---

## Phase 8 — CLI / REPL — Implemented

Status: implemented and verified.

Implemented:

```text
/dm
/room
/thread
/back
/members
/status
/publish
```

Machine-readable `--json` frames the operational commands. Interactive
commands (`july dm`, `july thread open`) stream a session and never frame JSON.

Implemented evidence:

- Room operational commands with exact name/`RoomId` resolution, retained
  membership history, human and `--json` framing, and malformed grammar that
  mutates nothing — `tests/cli_room.rs`;
- non-interactive Thread commands with durable IDs, idempotent membership
  changes, and typed membership/open-state error codes —
  `tests/cli_thread.rs`;
- explicit Publish with a durable idempotent retry and typed missing
  result/target errors — `tests/cli_publish.rs`;
- REPL Root/Room descriptor stack, `/back` restoration, active-only
  `/members`, and non-fatal slash-command failures —
  `tests/cli_repl.rs`;
- multi-Agent DM switching that preserves distinct bindings and histories, and
  a failed switch that leaves the previous descriptor active —
  `tests/cli_repl.rs`;
- interactive Thread context with DM-to-Thread and Thread-to-Thread isolation,
  contextual `/publish`, `thread open` streaming and detaching, and SIGINT
  during a permission prompt cancelling the turn without dropping the context —
  `tests/cli_repl.rs`.

Verified DoD:

- one process reaches every Agent, Room, Thread and Publish target through the
  descriptor stack instead of one terminal per coding agent;
- switching UI context does not merge LLM contexts: each Conversation keeps its
  own session binding and transcript, only the top descriptor is live, and a
  cold descriptor holds no transcript or model state.

Phase 8 adds no daemon, no TUI, no `room use` command, and no implicit context
for non-interactive commands. Live-provider smoke remains opt-in.

---

## Phase 9 — Packaging / Release

Status: complete. `july --version` (and `--version --json`) reports the package
version; `scripts/release.sh` builds the macOS targets and emits tarballs with
`SHA256SUMS`; `scripts/install.sh` / `scripts/uninstall.sh` install and remove
the binary while keeping `~/.july` unless `--purge` is confirmed interactively.
`tests/cli_bootstrap.rs` proves a fresh machine gets its database created and
migrated by the first command, and `tests/cli_version.rs` covers version output.
Homebrew packaging stays deferred until the release cadence is stable.

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

## Phase 10 — Interactive TUI — Implemented

Status: complete. The no-argument both-streams-TTY path now runs the Ratatui
shell. Non-TTY line REPL, named standalone streams, finite commands and JSON
output retain their Phase 8 contracts. PTY tests cover dispatch and terminal
restoration; reducer, rendering and integration tests cover the remaining DoD.
Fresh closure-gate evidence is recorded on `JULY_WORKSPACE-m5e.6`.

Replace only the no-argument REPL on an interactive terminal with a Ratatui
shell. Preserve the Phase 8 runtime, context isolation, command grammar,
non-TTY REPL, named interactive commands, finite CLI output, and JSON contract.

Implement:

- stable application-owned scrollback plus one mutable streamed Markdown tail;
- multiline input, resize, manual scroll, and follow-tail behavior;
- existing Root, Room, DM, and Thread navigation;
- exclusive permission modal and cancel-once active-turn behavior;
- terminal restoration on every normal and error return path.

Do not:

- repaint streamed ANSI text directly to stdout;
- add a daemon, GUI, mouse workflow, theme/plugin system, or new command grammar;
- change ACP, storage, recovery, or session-ownership contracts;
- move standalone `july dm` or `july thread open` into the TUI.

DoD:

- streamed paragraphs, quotes, lists, and fenced code render progressively
  without exposing raw fence markers;
- scrolling and permission prompts remain stable while deltas arrive;
- cancellation, failure, resize, and exit leave a usable/restored terminal;
- Phase 8 compatibility and full Rust gates remain green.

Detailed design and slice DAG:
`docs/superpowers/specs/2026-08-26-phase-10-interactive-tui-design.md`.

Live-provider smoke remains opt-in and is not Phase 10 closure evidence.

---

## Post-Phase 10 — Delivery operations

Status: implemented and verified under `JULY_WORKSPACE-sca.3`.

`july delivery list [--json]` and `/deliveries` expose deterministic,
workspace-wide FAILED delivery inspection. `july delivery retry <message-id>
--agent <agent> [--json]` and `/delivery retry ...` reuse the existing DM and
Thread failed-only claim engines and preserve the current REPL/TUI context.
Missing, non-failed and concurrently claimed rows are typed as
`delivery_not_retryable`.

This surface adds no schema, daemon, automatic retry/backoff, failure-reason
persistence or exactly-once guarantee. Retry remains explicit and at-least-once:
a crash after transport acceptance but before the `DELIVERED` write can cause a
duplicate exact-body delivery.

---

## Post-Phase 10 — Explicit Work control

Status: implemented and verified under `JULY_WORKSPACE-sca.5`.

The active Work context exposes explicit, deterministic mutations through
`/work assign`, `/work status`, and `/work result`. Each command names a Work
ID from the current context and reuses the existing Work lifecycle service.
This slice adds no semantic routing, automatic dependency transition, schema,
or A2A behavior.

---

## Post-Phase 10 — Human Decision inbox

Status: implemented and verified under `JULY_WORKSPACE-sca.2`.

`/decisions` lists only durable `pending` and `needs_decision` records with
their owner, alternatives and evidence. `/decision accept` settles a
user-owned Decision, `/decision reject` cancels it with an explicit reason,
and `/decision work` converts a decided Decision into one explicitly identified
Work item. These commands reuse the existing Decision and Work transactions.

This slice adds no voting, scoring, semantic facilitator, automatic debate,
schema, Room chat or A2A behavior.

## Future phase — Room agent communication via A2A

Status: deferred. It is not part of the current supplemental-feature phase and
must not resume until explicitly scheduled.

The active design is
`docs/24-JULY WORKSPACE — ROOM AGENT COMMUNICATION VIA A2A.md`:

- Room is the membership and durable shared-chat boundary;
- `RoomMessage` is canonical shared state;
- explicit mentions route and activate only Room members;
- A2A carries interactions between July-managed agents in the same Room;
- ACP remains the runtime execution layer;
- private runtime transcripts never enter Room chat;
- simple chat does not require Thread or Work;
- external agents, cross-Room messaging and agent-agent DM are deferred.

Future implementation order: audit, durable Room chat, mention routing, incremental
Room cursors, agent-facing messaging, internal A2A bridge, ACP recipient
delivery, shared-response normalization, structured Work integration,
recovery, then cleanup of the obsolete external-first direction.

`JULY_WORKSPACE-sca.5` is required only before structured Work integration;
the human Decision inbox is not a prerequisite for Room chat.

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
