# July Workspace — Security and Capabilities

## Philosophy

Use real capability boundaries instead of large prose guardrails where possible.

## Agent codebase ownership

Each agent has:
- `project_root`;
- allowed write roots;
- optional read-only roots;
- optional network/tool policy.

Cashpoint must not silently gain write access to Pay.

## Cross-project collaboration

Preferred path:

```text
cashpoint asks pay agent
```

rather than:

```text
cashpoint reads/writes pay repo directly
```

Explicit read-only cross-project access may be allowed where justified.

## ACP permission flow

```text
agent request
→ ACP PermissionRequested
→ July policy
→ allow / deny / ask user
```

## High-risk actions

Do not silently auto-approve:
- production deployment;
- destructive DB migration;
- credentials/secrets mutation;
- remote destructive operations.

## Communication trust

Agent messages are claims, not unquestioned truth.

For cross-thread coordination prefer:

```text
Result + evidence
```

## Room membership vs filesystem permission

Room membership means:
- can be invited/communicate.

It does not mean:
- can read/write every room member's codebase.

Thread delivery and session opening require an active Agent membership in both
the Thread and its Room. Every open/send path rechecks these durable facts
before transport use. Room membership never subscribes an Agent to a Thread.

Only the local user may mutate membership in Phase 4. An add requires an active
Agent and active parent scope. Removing a Room member is rejected while that
Agent still has an active membership in any Thread in the Room; the user must
leave those Threads explicitly. `role` is descriptive metadata and grants no
authorization.

A committed Thread removal blocks new delivery immediately. Cancellation of an
already active turn is best-effort after commit and cannot undo the durable
membership transition.

## Audit

Persist at least:
- agent/session creation;
- permission decisions;
- membership changes;
- work status changes;
- result creation/publish;
- recovery events.

Membership audit is represented by retained membership generations with
`joined_at` and `left_at`. Removal closes the active generation; closed
generations are immutable, and rejoin creates a new generation instead of
erasing the previous interval.

Do not store hidden model reasoning.
