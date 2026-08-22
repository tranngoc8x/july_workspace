# July Workspace — Messaging, Publish and Dependencies

## Messaging goals

Messages should support:
- user ↔ agent DM;
- agent ↔ agent DM;
- thread messages;
- mentions;
- offline delivery;
- deterministic routing.

## Explicit target

If user writes:

```text
@cashpoint check callback
```

runtime routes directly.

No semantic router is required.

## Thread mention

If Pay is not a member:

```text
@pay check transaction UB123
```

runtime:
1. adds Pay;
2. creates/resumes Pay thread session;
3. sends a compact thread capsule;
4. delivers the message.

## Offline delivery

Phase 5.3 persists the message and one explicit target delivery before
transport. Delivery states are exactly:

- PENDING
- DELIVERED
- FAILED

`DELIVERED` means the existing transport `send_message` accepted the exact
body and is terminal. Ordinary owner, session-open, or send failures leave the
message durable and transition its target row to `FAILED`; the runtime returns
a typed structured failure outcome. Each target has its own row, so target-only
routing is preserved. A temporarily offline agent must not lose the message.

Explicit retry claims only `FAILED`. It reuses the stored exact target and body;
a delivered or concurrently claimed delivery is a no-op. For Thread mentions,
retry revalidates active Agent, Room, and Thread membership and never implicitly
rejoins. A capsule is persisted only when the initial mention joins or rejoins
the target, and capsule delivery is tracked separately so a successful capsule
is not resent.

Phase 5.4 startup reconciliation transitions pre-existing `PENDING` deliveries
to `FAILED` before the storage worker becomes ready. It does not send them or
schedule automatic retry: a caller must explicitly retry after restart. The
stored target/body and any successful Thread capsule progress are preserved.
Restart and cancellation/process-loss coverage proves this independently for DM
and Thread delivery, including isolation from sibling target context.

Delivery is at-least-once. If the process crashes after transport acceptance
but before the `DELIVERED` transition, a later explicit retry can deliver the
same body again. July makes no exactly-once promise and does not add a daemon,
automatic backoff, CLI, semantic routing, or a new public messaging syntax in
Phase 5.4.

## Work lifecycle

Phase 6 uses these explicit transitions:

| From | Allowed targets |
|---|---|
| `OPEN` | `WORKING`, `BLOCKED`, `CANCELLED` |
| `WORKING` | `BLOCKED`, `READY`, `FAILED`, `CANCELLED` |
| `BLOCKED` | `WORKING`, `FAILED`, `CANCELLED` |
| `READY` | `DONE` |
| `DONE`, `FAILED`, `CANCELLED` | none |

An exact transition retry is a no-op. `completed_at` is absent for `OPEN`,
`WORKING`, `BLOCKED`, and `READY`, and is set exactly for terminal `DONE`,
`FAILED`, and `CANCELLED`.

Phase 4 Work starts `OPEN` and unowned. An explicit assignment may set or
replace the owner of non-terminal Work only with an active Agent who is an
active member of the Work's conversation. Exact assignment retry is a no-op;
terminal Work keeps its last owner. Phase 6 does not add automatic assignment,
handoff negotiation, or CLI commands.

## Result

Conversation transcript is not the portable output.

A work result should look like:

```json
{
  "status": "READY",
  "summary": "Auth endpoint is available",
  "outputs": ["endpoint:/token"],
  "evidence": ["commit:abc123", "test:auth-contract"]
}
```

## Publish

Publish transfers an immutable structured Result plus its source conversation
reference, not history. The source is derived from Result -> Work ->
Conversation rather than accepted from caller input.

```text
Thread auth
→ Result READY
→ Publish
→ Thread payment
```

Target gets:
- summary;
- outputs;
- evidence;
- source reference.

The natural idempotency key is `(result_id, target_conversation_id)`. Repeating
the exact publish is a no-op; a conflicting publish ID fails without copying
Messages. Phase 6 exposes no CLI; presentation syntax remains Phase 8 work.

## Dependencies

Each directed edge is stored as:

```text
upstream_work_id = Work A (prerequisite)
downstream_work_id = Work B (consumer)

Work A -> Work B
Work B requires Work A
```

Dependency state is durable and exactly one of:

- `WAITING`: the prerequisite has no consumable Result yet;
- `SATISFIED`: the prerequisite produced a READY Result;
- `FAILED`: the prerequisite failed;
- `SUPERSEDED`: the Result which satisfied the edge was replaced by a newer
  immutable Result.

New edges start `WAITING`. The allowed propagation transitions are
`WAITING -> SATISFIED`, `WAITING -> FAILED`, and
`SATISFIED -> SUPERSEDED`; exact repeats are no-ops.

## Automatic propagation

If Work B requires Work A:

```text
Work A READY with Result
-> dependency SATISFIED
-> Work B can consume that structured Result
```

The Work/Result write and outgoing dependency transitions are one SQLite
transaction. This is deterministic runtime logic, not LLM reasoning, transcript
messaging, a daemon, or background retry.

## Result immutability

Every Result is immutable, whether published or not. The first Result is created
atomically with its Work's transition to `READY`. A correction creates a new
Result whose `supersedes_result_id` identifies an existing Result for the same
Work; the prior Result remains unchanged. Exact Result-ID retry is a no-op,
while conflicting content or cross-Work supersede fails without partial writes.

## READY semantics

READY means downstream can safely consume output.

It does not necessarily mean all cleanup/documentation on the source Thread is
finished. `DONE` is the later terminal state after that remaining work.

## Quick coordination vs shared work

Use:
- DM for a quick question;
- Thread for collaboration with lifecycle, owner, state and result.

This boundary prevents every minor question from becoming a heavy task object.
