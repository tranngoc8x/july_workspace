# July Workspace — Conversation Model

## Core rule

DM, Room and Thread are not modes of one shared context.

```text
fork context
→ transfer capsule/reference
→ work independently
→ publish selected result
```

## DM workflow

```text
/dm cashpoint
```

Flow:

```text
User ↔ cashpoint agent ↔ cashpoint repo
```

If target is explicit, no July LLM is required.

## DM agent needs another agent

Quick question:

```text
cashpoint → pay DM
```

Longer shared investigation:

```text
cashpoint DM
→ create/join Room Thread
→ cashpoint + pay collaborate
```

The source DM remains intact.

## Room workflow

Room is a member pool and shared business scope.

Agents do not receive every room activity.

Example:

```text
Room VNA
members: cashpoint, pay, infra, mobile
```

`Thread payment` may include only:
- cashpoint
- pay

Infra/mobile are not woken and do not receive the thread transcript.

## Dynamic thread membership

If Cashpoint says:

```text
@pay check this contract
```

and Pay is not a member:

1. add Pay to thread;
2. create/resume Pay session for this thread;
3. send compact thread capsule;
4. deliver the new message.

## Context capsule

Cross-context handoff payload:

- origin;
- goal;
- known findings;
- accepted decisions;
- open questions;
- references.

Never default to copying all source messages.

## DM from Thread

A side issue should create a new DM context:

```text
Thread VNA/payment
→ DM cashpoint / redis-side-issue
```

The main thread remains clean.

## Parent/child threads

Large task:

```text
payment-main
├── pay-contract
└── callback-investigation
```

Child threads return Results upward.

## Anti-noise invariants

1. Room membership does not imply thread subscription.
2. Thread membership does not imply access to unrelated DMs.
3. Cross-context transfer is explicit.
4. Publish carries Result, not transcript.
5. One persistent agent identity may own many isolated sessions.
