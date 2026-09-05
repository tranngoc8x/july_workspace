# JULY WORKSPACE — A2A INTEROPERABILITY DESIGN

> **Superseded:** This external-interoperability design is retained only as
> historical context. The active product direction and source of truth is
> `docs/24-JULY WORKSPACE — ROOM AGENT COMMUNICATION VIA A2A.md`.
> External agents are not part of the current Definition of Done.

Status: Approved roadmap; implementation plans are written and approved per phase.

Protocol baseline: A2A v1.

## 1. Goal

July interoperates with external agents through A2A while keeping July's
`WorkItem`, `Message`, `WorkResult`, and collaboration rules canonical.

The finished capability includes:

- outbound delegation from July to an A2A agent;
- inbound delegation from an A2A client to July;
- task discovery, lifecycle, messages, artifacts, streaming, multi-turn work,
  cancellation, restart recovery, authentication, and operator controls;
- conformance for every capability and protocol binding July advertises.

July does not become a generic A2A gateway, service mesh, or server framework.
Optional protocol features remain disabled unless July has a concrete use for
them.

## 2. Architecture invariants

1. July `WorkItem` is the local source of truth. An A2A task identifier is a
   durable external binding, never the primary July identity.
2. A2A protocol types stop at the adapter boundary. Application and storage
   services receive July-owned commands and domain values.
3. Results and explicitly selected context may cross the boundary. Private
   runtime transcripts, unrelated artifacts, project-wide context, and
   credentials do not.
4. A remote event cannot bypass July's existing ownership, scope, lifecycle,
   Result immutability, Publish, or dependency invariants.
5. Ambiguous network outcomes are reconciled by remote task identity. July
   does not blindly resend a request that the remote agent may have accepted.
6. ACP behavior and its single per-Agent connection owner remain unchanged.

```text
Room / Thread
      |
      v
July Work and Result services
      |
      +-------------------+
      |                   |
      v                   v
ACP runtime          A2A application port
                          |
                          v
                    A2A client/server adapter
                          |
                          v
                    External A2A peer
```

## 3. Chosen approach

Implementation uses vertical slices. Each phase produces independently
testable behavior before the next phase starts.

Rejected alternatives:

- A protocol-first horizontal layer delays end-to-end proof and encourages
  unused abstractions.
- A separate gateway service adds deployment and failure boundaries that July
  does not need.
- Reusing `AgentTransport` would leak ACP-specific session, permission, and
  turn semantics into A2A task operations.

The A2A boundary exposes only the operations used by a phase. It is extended
when a later approved phase needs another operation.

## 4. Protocol and binding scope

The first supported binding is JSON-RPC over HTTP with SSE for streaming. July
declares only that binding and the capabilities it implements.

Required A2A operations by completion:

- Agent Card discovery;
- Send Message and Send Streaming Message;
- Get Task, List Tasks, Cancel Task, and Subscribe to Task.

Deferred until a concrete consumer requires them:

- push notification configuration;
- extended or signed Agent Cards;
- additional REST or gRPC bindings;
- custom A2A extensions.

Deferring an optional operation is conformant because July does not advertise
that capability.

## 5. Canonical mappings

### 5.1 Agent

The existing July `Agent` record is reused:

```text
transport_type = "a2a"
transport_config = {
  card_url,
  selected_interface,
  protocol_version,
  credential_ref
}
metadata = {
  normalized_capabilities,
  normalized_skills,
  card_cache_metadata
}
```

Secrets are resolved through `credential_ref`; they are not stored in
`transport_config`, SQLite snapshots, or logs.

### 5.2 Task binding

A dedicated durable binding owns remote identity and recovery state:

```text
A2aTaskBinding {
  work_id,
  agent_id,
  remote_task_id,
  remote_context_id,
  protocol_version,
  selected_interface,
  remote_state,
  created_at,
  updated_at
}
```

The natural key is `(work_id, agent_id)`. An exact local retry reuses the
binding instead of creating another remote task.

### 5.3 Lifecycle

```text
A2A submitted or working      -> July Working
A2A input-required            -> July Blocked plus pending Decision
A2A auth-required             -> July Blocked plus pending Decision
A2A completed with artifacts  -> immutable Result plus July Ready
A2A failed or rejected        -> July Failed
A2A canceled                  -> July Cancelled
unknown state                 -> no transition plus recorded ProtocolError
```

Dispatch transitions an eligible Work to `Working` before terminal remote
states are applied. A remote completion never moves `Ready` to `Done`; July
retains that local completion decision.

Events are monotonic and idempotent. Duplicate or stale events cannot move a
Work backward or create a second equivalent Result.

### 5.4 Messages and artifacts

- A2A Messages become scoped July Messages only when they carry a request,
  clarification, or status useful to the collaboration.
- A2A Artifacts become immutable July Result output.
- Task output is not encoded as transcript text.
- Text and structured-data parts are required for the first outbound slice.
- File parts are accepted only through bounded inline content or an explicitly
  permitted reference. MIME type, size, and destination are validated.
- Phase 4 adds protocol-neutral `WorkArtifact` and ordered `WorkArtifactPart`
  records keyed to `WorkResult`. They preserve part kind, media type, content
  or reference, and metadata without adding A2A fields to `WorkResult`.
- Existing `WorkResult.outputs` remains the human-readable output summary for
  compatibility; `WorkArtifactPart` is the lossless machine-readable payload.

## 6. Error model

The adapter normalizes protocol and transport failures into a small
application-facing set:

```text
AgentUnavailable
AuthenticationFailed
RemoteTaskFailed
ProtocolError
Timeout
UnsupportedCapability
UnsafeEndpoint
PayloadRejected
```

Raw SDK errors remain available as redacted diagnostic metadata. They do not
become domain variants or user-facing contracts.

Timeout after submission is an ambiguous outcome. July records the attempt and
reconciles by binding/task identity; it does not automatically submit again.

## 7. Security boundary

Every external A2A peer is untrusted.

Outbound controls:

- HTTPS verification outside explicit local test mode;
- endpoint allowlist and redirect validation;
- SSRF protection for Agent Card, interface, and artifact URLs;
- credential references instead of stored secrets;
- request, response, artifact, timeout, and concurrency limits.

Inbound controls:

- authenticated caller identity;
- authorization by Agent, Room, Conversation, and Work scope;
- validation before persistence or task execution;
- no access to unrelated messages, Results, files, or project roots.

All logs and audit records redact credentials and sensitive payloads.

## 8. Delivery phases

### Phase 0 — Operational prerequisites

Complete the existing dependency chain:

```text
JULY_WORKSPACE-sca.3
  -> JULY_WORKSPACE-sca.5
  -> JULY_WORKSPACE-sca.2
  -> JULY_WORKSPACE-sca.1
```

Exit criteria:

- failed deliveries are inspectable and retried only by explicit action;
- Work owner, status, and Result mutations are available through validated
  application commands;
- a remote `input-required` condition can enter the human Decision workflow.

### Phase 1 — Protocol contract and conformance fixture

Deliver:

- pin the A2A v1 Rust SDK version compatible with Rust 1.96;
- record the JSON-RPC/HTTP plus SSE binding decision;
- define protocol-neutral request, event, and error boundaries;
- add a deterministic mock A2A agent and wire fixtures.

Exit criteria:

- Agent Card and selected protocol objects round-trip through fixtures;
- the dependency compiles with the workspace toolchain;
- no July domain or ACP behavior changes.

### Phase 2 — Agent discovery and registration

Deliver:

- fetch, validate, and cache an Agent Card;
- select a compatible interface and protocol version;
- normalize skills and capabilities into the existing `Agent` record;
- add or update an A2A agent through existing scriptable agent management.

Exit criteria:

- one real external Agent Card registers successfully;
- malformed cards, unsafe endpoints, unsupported versions, and missing required
  capabilities fail before dispatch;
- secrets are absent from persisted agent configuration.

### Phase 3 — Outbound task dispatch

Deliver:

- send one July Work request to an A2A agent;
- persist `A2aTaskBinding` atomically with the accepted remote identity;
- make exact dispatch retries idempotent by local binding identity;
- expose a focused operator-visible dispatch result.

Exit criteria:

- a mock and one real external agent accept a Work request;
- restart preserves the binding;
- an exact local retry does not create another remote task;
- existing ACP tests stay green.

### Phase 4 — Lifecycle, Message, and Result mapping

Deliver:

- implement the canonical status mapping;
- persist selected remote Messages in the correct conversation;
- convert completed artifacts into an immutable Result;
- persist lossless protocol-neutral Work Artifacts and ordered Parts alongside
  the Result;
- apply Result creation and the `Ready` transition atomically;
- record normalized failures without partial state.

Exit criteria:

- completed, failed, rejected, canceled, input-required, auth-required, stale,
  duplicate, and unknown events have regression coverage;
- terminal event replay is a no-op;
- no external transcript is copied into another context.

### Phase 5 — Streaming, multi-turn, cancellation, and recovery

Deliver:

- consume SSE task status and artifact events;
- continue an existing task using its remote task/context identifiers;
- route clarification and auth requests through the Decision inbox;
- cancel by explicit operator action;
- reconnect using Subscribe/Get Task and reconcile durable state.

Exit criteria:

- disconnect and process restart do not lose the task;
- duplicate or out-of-order events do not regress state;
- multi-turn input remains in the bound context;
- ambiguous sends are surfaced for reconciliation, not blindly retried.

### Phase 6 — Inbound A2A server

Deliver:

- publish a July Agent Card;
- implement Send Message, Send Streaming Message, Get Task, List Tasks, Cancel
  Task, and Subscribe to Task for declared agents;
- map inbound operations onto the same July application services and
  invariants used by local commands;
- isolate each remote caller to its authorized scope.

Exit criteria:

- an external A2A client can create, observe, continue, stream, and cancel a
  July-backed task;
- unauthorized cross-scope reads and mutations fail;
- inbound and outbound paths share lifecycle and Result semantics.

### Phase 7 — Security and operational controls

Deliver:

- complete outbound and inbound authentication policy;
- enforce endpoint, redirect, payload, MIME, timeout, and concurrency limits;
- add inspect binding, reconcile, cancel, and safe retry commands;
- add structured redacted logs and audit records.

Exit criteria:

- negative tests cover SSRF, authentication, authorization, oversized payload,
  unsafe artifact, timeout, and cross-scope access;
- DB and log snapshots contain no secrets;
- every remote lifecycle mutation is traceable to its binding and event.

### Phase 8 — Conformance and production pilot

Deliver:

- run the A2A TCK for every advertised server capability;
- cross-test with at least one independent A2A SDK/client;
- run an end-to-end external-agent pilot;
- verify database migration, restart, disconnect, event replay, and protocol
  version failure paths;
- document setup, operation, recovery, and troubleshooting.

Exit criteria:

```text
Internal agent
  -> July Work
  -> external A2A agent
  -> status / clarification / artifacts
  -> immutable July Result
  -> Publish to the owning Thread
```

- the reverse inbound flow also passes;
- transcript isolation and July scope validation remain intact;
- all Rust formatting, lint, test, build, and ACP regression gates pass;
- the Beads phase issues are closed only after their evidence is recorded.

## 9. Verification strategy

Each implementation phase follows TDD and leaves the smallest meaningful
regression at the responsible boundary.

Verification layers:

1. pure mapping tests for states, Messages, Parts, Artifacts, and errors;
2. storage tests for binding uniqueness, atomicity, replay, and migrations;
3. adapter integration tests against the deterministic mock agent;
4. runtime tests for disconnect, cancellation, multi-turn, and recovery;
5. security tests at every external trust boundary;
6. TCK and cross-SDK tests for advertised A2A server behavior;
7. full workspace gates to protect ACP and existing CLI/TUI contracts.

No phase is complete based only on mocked unit tests when its acceptance
criteria require a real peer, restart, or protocol conformance proof.

## 10. Beads and implementation governance

`JULY_WORKSPACE-sca.1` owns the full A2A program and remains blocked by
`JULY_WORKSPACE-sca.2`. Phase 1 also depends directly on `sca.2` because Beads
does not propagate a parent's blocker to its children. Phases 2 through 8 then
depend sequentially on the prior phase because each consumes its durable
boundary.

For every phase:

1. write and approve its focused implementation plan;
2. claim only that phase issue;
3. implement with TDD;
4. run focused and full relevant gates;
5. review the diff against this design and the phase acceptance criteria;
6. close the phase issue and record evidence;
7. commit only with explicit authority; never push implicitly.

## 11. Definition of done

The A2A program is complete when:

1. July operates as both an A2A client and server for its advertised v1
   capabilities.
2. One external agent and one independent client pass documented end-to-end
   flows.
3. Remote task bindings, streaming progress, multi-turn context, cancellation,
   and recovery survive restart without duplicate Results.
4. July remains canonical for identity, lifecycle, scope, Results, Publish, and
   dependencies.
5. External transcripts and credentials remain isolated.
6. Security, migration, conformance, and ACP regression gates pass.
7. Optional features not implemented are not advertised.
