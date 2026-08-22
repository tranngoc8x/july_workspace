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

Publish transfers Result, not history.

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

## Manual publish

CLI-level idea:

```text
/publish result to payment-flow
```

Natural language UX may wrap this later.

## Dependencies

Directed graph:

```text
Work A
  ↓ requires
Work B
```

Dependency state:
- WAITING
- SATISFIED
- FAILED
- SUPERSEDED

## Automatic propagation

If B requires A:

```text
A READY
→ dependency SATISFIED
→ publish dependency_update to B
```

This must be runtime logic, not LLM reasoning.

## Result immutability

Published result should be immutable.

Corrections create a new Result with:
- `supersedes_result_id`.

## READY semantics

READY means downstream can safely consume output.

It does not necessarily mean all cleanup/documentation on source thread is finished.

## Quick coordination vs shared work

Use:
- DM for a quick question;
- Thread for collaboration with lifecycle, owner, state and result.

This boundary prevents every minor question from becoming a heavy task object.
