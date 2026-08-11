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

Message persists first, then delivery is attempted.

Recommended delivery state:
- PENDING
- DELIVERED
- FAILED

A temporarily offline agent must not lose the message.

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
