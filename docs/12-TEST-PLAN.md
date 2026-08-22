# July Workspace — Test Plan

## 1. Domain tests

Agent:
- create/update;
- duplicate rejection.

Room:
- create;
- membership generation add/no-op/remove/no-op/rejoin;
- same agent in multiple rooms.

Conversation:
- DM;
- Thread;
- active Room membership required for Agent Thread membership;
- Room removal rejected while active Thread membership exists;
- local user automatically joins a new Thread;
- parent/child;
- origin relation.

Work:
- exactly one primary Work is created with each new Thread;
- valid lifecycle and owner transitions with exact-retry no-ops;
- invalid transition, owner, and terminal timestamp rejection;
- atomic first structured Result + `READY` and rollback on Result failure;
- immutable same-Work Result correction with supersede validation.

Current Phase 6 Work/Result coverage is in `tests/work_lifecycle.rs` and
`tests/work_results.rs`. SQLite migration tests additionally verify durable
Work completion and dependency Result-reference invariants.

## 2. Context isolation

### Same agent, two threads

```text
cashpoint / thread A
cashpoint / thread B
```

Assert:
- distinct session bindings;
- B bootstrap excludes A transcript;
- A result appears in B only through explicit publish.

### DM while thread active

Assert:
- DM recent history excludes thread messages;
- thread recent history excludes DM.

## 3. Messaging

Current Phase 5.1 coverage:
- exact active unordered Agent pair reuse;
- target-only exact-body delivery through the shared owner;
- deterministic typed routing to the target owner;
- durable source/target attribution for both sides of the DM.

Current Phase 5.2 coverage:
- explicit target-only Thread mention routing through the shared owner;
- atomic message persistence with idempotent join and generational rejoin;
- capsule-before-body delivery for new members and body-only active delivery;
- duplicate replay suppression and typed scope rejection before side effects;
- durable membership and message state after open or send failure;
- isolation from unrelated Agents, Threads, Rooms, and DMs.

Current Phase 5.3 coverage:
- per-target delivery rows with `PENDING`, `DELIVERED`, and `FAILED`;
- message-plus-delivery persistence before transport;
- `DELIVERED` after transport acceptance and `FAILED` on owner/open/send
  failure, with typed structured failure outcomes;
- explicit `FAILED`-only retry using the stored exact target/body;
- Thread capsule progress and retry without duplicate successful capsules;
- target-only routing, exact replay suppression, and at-least-once crash-window
  semantics.

Current Phase 5.4 coverage:
- startup reconciliation transitions persisted `PENDING` deliveries to `FAILED`
  before storage-worker readiness, without transport send or automatic retry;
- cancellation/process-loss DM and Thread delivery reopen real runtime and
  storage before explicit failed-only retry;
- restart retry reuses the exact stored target/body, preserves target-only and
  sibling-context isolation, and keeps successful Thread capsule progress
  without resending that capsule;
- at-least-once crash-window semantics; exactly-once delivery is not asserted.

## 4. Publish

Current Phase 6 coverage in `tests/result_publish.rs` asserts:
- the target query returns the complete immutable structured Result and source
  conversation reference;
- source transcript Messages are not copied;
- natural-key duplicate Publish is idempotent;
- conflicting IDs and missing Result, Work, or target roll back without a
  partial mapping;
- same-conversation and superseding-Result Publish follow the same contract.

## 5. Dependency

```text
A first Result + READY → matching outgoing edge SATISFIED + Result
A FAILED → matching outgoing WAITING edge FAILED
A corrected Result → matching outgoing SATISFIED edge SUPERSEDED + replacement Result
```

Current Phase 6 coverage in `tests/work_dependencies.rs` asserts:
- new edges start `WAITING`; exact add/retry is idempotent;
- missing Work, self-dependency, and recursive cycles are rejected without
  partial rows;
- downstream queries hydrate the same-upstream immutable Result, including its
  summary, outputs, and evidence, for `SATISFIED` and `SUPERSEDED`, and no
  Result for `WAITING` and `FAILED`;
- only matching outgoing edges change; unrelated edges, Work, Messages, and
  Publish rows remain unchanged;
- real SQLite trigger failures roll back each atomic `SATISFIED`, `FAILED`, and
  `SUPERSEDED` operation.

Migration and hydration tests in `src/storage/sqlite.rs` additionally verify
raw-write guards, conservative reconciliation of invalid legacy references,
and rejection of cross-Work Result references. Phase 6 does not notify through
Message/transport or automatically change downstream Work status.

Phase 6.5 deliberation, Phase 7 recovery, Phase 8 CLI, external A2A protocol
integration, semantic routing, and transcript transfer are outside this test
slice.

## 6. Session

Test:
- create;
- resume;
- disconnect/reconnect;
- remote session missing;
- replacement generation;
- cancel.

### Phase 2 transport acceptance

Deterministic integration tests use one test-only ACP JSON-RPC subprocess.
This is a protocol fixture, not a production transport or CLI-scraping
fallback. Add the fixture with the first `ACPTransport` failing test; a
standalone harness before that has nothing useful to exercise.

The fixture must prove:
- initialize rejects the wrong protocol or unexpected adapter identity;
- one Agent connection creates two independent sessions;
- only one active turn is admitted per session;
- events remain ordered per session while sessions interleave;
- text pressure coalesces without dropping permission or terminal events;
- permission selection uses an advertised option and unknown options cancel;
- cancellation sends `session/cancel`, drains a terminal event, and enforces
  the 10-second connection deadline across every binding on that connection;
- EOF maps once to Agent-scoped `TransportDisconnected`;
- remote-not-found during resume maps to session-scoped `SessionLost`;
- shutdown resolves permissions, reaps the child and leaves no owner task.

Migration tests for `0002_session_runtime.sql` must prove all four lifecycle
values, rejection of unknown values, one current binding per
Conversation/Agent pair, multiple historical generations, and SQLite-enforced
append-only permission decisions. They also prove migration rollback for an
unknown v1 status or duplicate current generations, preservation of both v1
session indexes, rejection of malformed option objects, and rejection of a
selected option that was not advertised.

Live adapter smoke tests are opt-in and never run in normal CI. For each pinned
profile they verify initialize, advertised capabilities, create, explicit
close, and clean process exit without sending a model prompt. Authenticated
prompt/stream/cancel/resume tests run only when provider credentials and quota
are intentionally supplied. Claude smoke additionally sets and verifies
manual `default` mode before any prompt. Codex resume coverage must use a
session that has completed at least one prompt because create alone may not
have produced a durable rollout.

## 7. Recovery

Delete remote session intentionally.

Expected:
- new session created;
- recovery capsule assembled;
- recent messages bounded;
- work continues;
- full transcript not replayed.

## 8. SQLite crash safety

Interrupt:
- create thread transaction;
- result + work completion;
- publish;
- session replacement.

Restart and verify no partial state.

Phase 4 migration tests prove membership generation history, at most one active
generation per natural key, preservation of existing rows as generation `1`,
and at most one primary Work per conversation. Aggregate tests interrupt or
fail each Thread creation insert and prove that Thread, members and primary
Work are either all committed or all absent. Transport startup failure after
commit must leave the durable aggregate intact and retryable.

## 9. Permission tests

- allowed write;
- denied cross-project write;
- permission prompt propagated;
- user denial.

## 10. Dependency absence tests

Run core suite with:
- Beads unavailable;
- Herdr unavailable;
- Zellij unavailable;
- tmux unavailable.

Everything must still pass.

## 11. Context/token regression

Track baseline for:
- DM bootstrap;
- thread join capsule;
- recovery capsule;
- room memory load.

Add budget checks after real baselines exist.

## 12. E2E scenarios

### A — Single-project DM
User → cashpoint → code change → result → resume later.

### B — Agent quick DM
Cashpoint asks Pay → Pay replies → Cashpoint continues.

### C — Room collaboration
VNA/payment starts with Cashpoint; Pay joins later.

### D — Cross-thread publish
Auth READY → Payment receives dependency update.

### E — Session loss
Kill session → recover → continue.

### F — Isolation
Unrelated Cashpoint DM runs while VNA/payment remains active; no leakage.
