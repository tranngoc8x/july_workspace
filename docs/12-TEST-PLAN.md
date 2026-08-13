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
- valid transitions;
- invalid transition rejection.

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

Test:
- user → agent DM;
- agent → agent DM;
- thread mention;
- dynamic member join;
- offline recipient;
- retry after transport failure.

## 4. Publish

Assert:
- Result copied structurally;
- source transcript not copied;
- duplicate publish idempotent;
- superseding result handled.

## 5. Dependency

```text
A READY → B SATISFIED
A FAILED → B notified
A superseded → downstream sees new state
```

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
