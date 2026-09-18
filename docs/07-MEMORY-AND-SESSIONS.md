# July Workspace — Memory and Sessions

## Phase 7 implementation status

Phase 7 is implemented for DM and Thread sessions. Recovery is explicit,
durable, uses bounded recent-message replay, and is handled by the existing
shared per-Agent runtime owner.

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

The deterministic `JULY_RECOVERY_V1` JSON capsule for a replacement session
contains:

1. agent identity/project root;
2. minimal project memory;
3. relevant room memory;
4. latest checkpoint;
5. at most the newest 20 messages strictly after the checkpoint anchor, emitted
   in chronological order;
6. their included count and truncation flag in `recent_messages_meta`;
7. compact published Result/reference links.

DM recovery omits Room memory. Thread recovery includes only its own Room
memory. Superseded memory and unrelated Room, Thread, DM, or source transcript
content are excluded. Normal resume sends no capsule or replay.

## Checkpoint

Compact working state.

Contains:

- goal;
- current state;
- accepted decisions;
- open items;
- relevant references;
- last processed message.

Create a new checkpoint:

- after milestones;
- when work becomes BLOCKED/READY;
- after meaningful decision;
- before intentional session replacement;
- periodically if a long-running thread justifies it.

Do not checkpoint every message.

Checkpoint creation is explicit. Latest lookup is scoped to the exact
Conversation/Agent pair and is deterministic even when timestamps tie.

## Recovery capsule example (abridged)

```json
{
  "version": "JULY_RECOVERY_V1",
  "agent": { "name": "agent_order", "project_root": "/workspace/agent_order" },
  "conversation": { "title": "VNA/payment-42" },
  "project_memories": [],
  "room_memories": [],
  "checkpoint": {
    "current_state": "Issue narrowed to VoucherService",
    "open_items": ["implement fix", "run callback integration test"],
    "references": ["src/VoucherService.php", "commit abc123"]
  },
  "published_results": [],
  "recent_messages_meta": { "included_count": 0, "truncated": false },
  "recent_messages": []
}
```

## Memory is not transcript

Message:

> I think Pay may return the wrong field.

must NOT automatically become:

```text
FACT: Pay returns wrong field
```

Memory promotion is always explicit. A message, hypothesis, or structured
Result never creates Memory automatically.

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

Promotion requires an existing source Conversation and validates the typed
scope and kind before writing. Self-supersession and supersession across scope
type or ID are rejected. Scope/kind queries return matching records in stable
order; capsule assembly separately excludes superseded records.

## Replacement lifecycle

When resume reports a missing remote session, or the provider reports the
current binding lost, July atomically marks generation `N` as `Lost`, creates
disconnected generation `N+1`, and stores its pending capsule. The shared
per-Agent owner then creates/attaches the remote session and sends that capsule
before normal work. DM and Thread adapters reuse this owner rather than opening
parallel transport connections.

A pending capsule survives restart and is retried unchanged. Transport
acceptance followed by a failed delivery-marker write may therefore send the
capsule more than once: recovery is at-least-once, not exactly-once. `Closed`
bindings are terminal and are never replaced. An unattached pending Thread
replacement reuses the same `N+1` binding and stored capsule instead of
advancing another generation.

Phase 7 adds no full-transcript replay, semantic memory/vector database,
background retry daemon, or exactly-once transport guarantee. Live provider
smoke remains opt-in.

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
