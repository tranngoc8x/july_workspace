# July Workspace — Architecture Decision Records

## ADR-001 — July is a workspace, not a mandatory supervisor
Status: Accepted

July's primary role is durable multi-project workspace/runtime. Semantic coordination is optional and invoked only when valuable.

## ADR-002 — Room is namespace; Thread is working context
Status: Accepted

Room stores member pool and shared business knowledge. Concrete collaborative work happens in Thread.

## ADR-003 — DM is first-class
Status: Accepted

Single-project work should route directly to the project agent.

## ADR-004 — Conversations are never moved
Status: Accepted

DM→Thread or Thread→DM creates/forks a new conversation with capsule/reference. Full history is not implicitly transferred.

## ADR-005 — Agent identity is not session identity
Status: Accepted

Agent identity persists. Runtime sessions are replaceable implementation state.

## ADR-006 — SQLite is the only initial durable workspace store
Status: Accepted

Messages, work, results, dependencies, memory/checkpoints and session bindings use SQLite.

## ADR-007 — External task systems are out of the core
Status: Accepted

The core does not depend on Beads or another issue/task system. July models WorkItem/Result/Dependency directly.

## ADR-008 — ACP is the initial AgentTransport implementation
Status: Accepted

ACP is isolated behind a small transport abstraction. No headless CLI fallback is implemented initially.

## ADR-009 — Terminal tools are not architectural dependencies
Status: Accepted

Herdr, Zellij, tmux or future terminal tools may be optional integrations only.

## ADR-010 — Results cross conversation boundaries; transcripts do not
Status: Accepted

Use Result / Publish / ContextCapsule for handoff.

## ADR-011 — July owns durable memory; agents own active context
Status: Accepted

July persists facts, decisions, results and checkpoints. Claude/Codex harnesses manage active model context and compaction.

## ADR-012 — Codebase is part of memory
Status: Accepted

Do not copy information into July if an agent can cheaply and reliably rediscover it from source.

## ADR-013 — Rust is the implementation language
Status: Accepted

July is a long-running, stateful, concurrent local runtime/control plane. Typed state machines, async I/O, lifecycle correctness and binary distribution are higher-value than Python AI-framework compatibility.

## ADR-014 — Tokio is the async runtime
Status: Accepted

Tokio owns asynchronous ACP/process/event tasks. Long-lived tasks require explicit ownership, shutdown and error paths.

## ADR-015 — rusqlite with direct SQL, no ORM initially
Status: Accepted

The schema is explicit and small enough that an ORM would add unnecessary abstraction.

## ADR-016 — Greenfield, no backward compatibility
Status: Accepted

July Workspace does not maintain compatibility with previous July implementations, schemas, commands, sessions, task systems or runtime integrations. Historical implementations are reference material only.

## ADR-017 — Pin the stable ACP v1 boundary
Status: Accepted

The initial ACP adapter uses the official Rust SDK dependency
`agent-client-protocol = "=2.0.0"` while speaking only the stable ACP v1
protocol. Unstable SDK features and HTTP, RMCP, conductor or proxy transports
are out of scope. An agent process is launched only from an explicit
`AcpAgentConfig`; commands that resolve a moving version such as `@latest` are
not accepted.

ACP SDK requests, responses, events, responders and errors remain private to
`transport::acp`. `AgentTransport` exposes only July-owned types. Permission
events carry a July-owned correlation ID and advertised options; the explicit
response returns through the transport boundary. Transport disconnection is an
Agent-scoped event, distinct from a missing remote session.

## ADR-018 — One owned ACP connection task per agent
Status: Accepted

`SessionManager<T: AgentTransport>` uses static dispatch. The initial design
does not add `async-trait`, a transport factory or dynamic dispatch.

Each Agent has one Tokio-owned ACP connection task which can host multiple
independent sessions. Bounded command and event channels provide backpressure
and preserve ordering within each session. Text deltas may be coalesced under
pressure; permission, terminal-state and session-loss events are never
dropped. Every connection task has an owner, shutdown path and error path.

Cancellation is cooperative: send the stable ACP `session/cancel` notification
and continue draining events until a terminal event arrives. This is not the
generic JSON-RPC `$/cancel_request` mechanism. After a fixed 10-second grace
period, close the connection.

## ADR-019 — Permissions fail closed and resume never invents context
Status: Accepted

Every permission request requires an explicit response. An unresolved request
remains unapproved and fail-closed; cancellation, shutdown or an unknown
permission option returns `Cancelled`. July never silently auto-approves a
request.

Session binding lifecycle states are `Active`, `Disconnected`, `Lost` and
`Closed`. Resume is attempted only when the agent advertises the required
capability and the binding is the current generation. Resume does not replay a
transcript. If the remote session no longer exists, the binding becomes
`Lost`; creating replacement generation `N+1` remains Phase 7 work.

`Active` and `Disconnected` are current states; `Lost` and `Closed` are
historical states. Phase 2 adds migration `0002` for typed binding lifecycle
constraints, at most one current binding per Conversation/Agent pair, and
append-only permission decision audit records.

## ADR-020 — A bounded storage worker owns synchronous SQLite
Status: Accepted

Tokio runtime tasks access the synchronous `SqliteStore` through one bounded
storage worker which owns the connection. SQLite transactions never span an
`.await`, and the runtime does not put the store behind a global mutex.

## ADR-021 — Phase 4 collaboration commands are explicit and stateless
Status: Accepted

Phase 4 exposes deterministic application commands for Room and Thread
creation, listing, membership changes and opening one Thread for one Agent.
Thread IDs are canonical `ConversationId` values; titles are not identifiers.
Room and Agent references resolve only by exact case-sensitive name or their
canonical typed ID.

`OpenThreadForAgent(thread_id, agent_id)` requires an active Agent, active Room,
open Thread and active Agent membership in both scopes. It addresses exactly
one Agent. It never broadcasts, joins an Agent implicitly, copies another
conversation transcript or uses an LLM to choose a recipient. The future CLI
maps this command to `july thread open <thread-id> --agent <agent>`.

The full REPL, `room use`, implicit current context and `--json` remain Phase 8.
Mentions, Agent-originated membership changes and dynamic joins remain Phase 5.

## ADR-022 — Membership is a durable generational state machine
Status: Accepted

Room and Thread membership use retained generations. At most one generation
for a member is active at a time:

```text
Absent or Left + add    -> Active in generation N+1
Active + add            -> Active, unchanged
Active + remove         -> Left with left_at
Absent or Left + remove -> unchanged
```

An Agent must be active to join an active Room. An Agent must also be an active
Room member before joining an open Thread in that Room. Room membership never
implies Thread membership. Removing an Agent from a Room is rejected while the
Agent has any active Thread membership in that Room; July never cascades that
removal implicitly. Thread removal does not alter Room membership.

Only the local user may mutate membership in Phase 4. `role` remains descriptive
and grants no authorization. One command timestamp is used for every row it
changes. Repeated target-state commands preserve the original timestamps.
Membership removal immediately blocks new open/send operations after commit;
transport cancellation of an in-flight turn is best-effort and occurs after
the durable transition. It does not erase messages or merge session identity
with membership identity. Removal closes the active generation; closed
generations are immutable and rejoin inserts a new generation.

## ADR-023 — Thread and primary Work creation are one durable operation
Status: Accepted

Every Phase 4 Thread is created with exactly one primary WorkItem in one
`BEGIN IMMEDIATE` SQLite transaction. The transaction validates the active
Room and initial Agent memberships, inserts the open Thread, inserts the local
user and initial Agent members, then inserts an open primary WorkItem whose
title and goal mirror the Thread. Phase 4 leaves `owner_agent_id` null; Work
ownership and lifecycle behavior remain Phase 6.

Any validation or insert failure rolls back the Thread, memberships and primary
Work together. Session bindings, ACP calls, capsules, Messages, Results,
Publishes and Dependencies are outside this transaction. Agent session startup
is lazy after commit and transport failure cannot roll back durable creation.

`work_items.is_primary` plus a partial unique index guarantees at most one
primary WorkItem per conversation. The aggregate creation operation guarantees
exactly one for each new Thread. A circular `conversations.primary_work_id`
foreign key is not introduced.
