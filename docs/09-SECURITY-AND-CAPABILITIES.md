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

## Audit

Persist at least:
- agent/session creation;
- permission decisions;
- membership changes;
- work status changes;
- result creation/publish;
- recovery events.

Do not store hidden model reasoning.
