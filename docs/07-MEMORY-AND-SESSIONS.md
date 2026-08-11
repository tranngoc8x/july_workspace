# July Workspace — Memory and Sessions

## Core principle

> July owns memory; agents own context.

## Session model

Remote Claude/Codex session is a runtime cache.

```text
Conversation
  └── SessionBinding
       └── remote session
```

If remote session exists:
- resume it.

If remote session disappears:
- conversation remains;
- create replacement session;
- recover from durable state.

## Recovery input

New/replacement session gets:

1. agent identity/project root;
2. minimal project memory;
3. relevant room memory;
4. latest checkpoint;
5. recent conversation messages;
6. explicit Result/reference links.

Never replay the entire transcript by default.

## Checkpoint

Compact working state.

Contains:
- goal;
- current state;
- accepted decisions;
- blockers;
- open items;
- relevant references;
- last processed message.

Create/update checkpoints:
- after milestones;
- when work becomes BLOCKED/READY;
- after meaningful decision;
- before intentional session replacement;
- periodically if a long-running thread justifies it.

Do not checkpoint every message.

## Recovery capsule example

```text
Agent: cashpoint
Conversation: VNA/payment-42

Goal:
Fix voucher pending after successful payment.

Current state:
- Pay side verified READY.
- Callback received successfully.
- Issue narrowed to VoucherService.

Decisions:
- Keep existing payment contract.
- Fix cashpoint side.

Open work:
- implement fix;
- run callback integration test.

References:
- src/VoucherService.php
- commit abc123
```

## Memory is not transcript

Message:
> I think Pay may return the wrong field.

must NOT automatically become:

```text
FACT: Pay returns wrong field
```

Memory promotion is explicit or based on verified structured results.

## Long-lived memory scopes

### Project
Non-obvious codebase/business constraints.

### Room
Shared business contracts and accepted decisions.

### Agent
Only if there is durable agent-specific operational knowledge.

## Thread data

Thread working state normally remains:
- checkpoint;
- work;
- result.

Do not promote all thread details into long-lived memory.

## Provenance

Every durable memory should retain:
- source conversation;
- kind;
- evidence where available;
- created time;
- superseded relation.

## No semantic memory initially

Use:

```text
SQLite + FTS5
```

Do not add:
- Mem0;
- Letta;
- vector DB;
- graph memory;
- embeddings pipeline.

## Codebase is memory

Do not persist what an agent can cheaply recover from source:
- obvious folder structure;
- implementation details;
- current code symbols.

Persist things expensive or impossible to infer:
- business decisions;
- intentional quirks;
- external constraints;
- production ownership;
- hidden operational assumptions.
