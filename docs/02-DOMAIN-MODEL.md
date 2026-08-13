# July Workspace — Domain Model

## Agent

Persistent identity for a codebase owner.

Fields:
- `id`
- `name`
- `project_root`
- `transport_profile`
- `status`
- metadata

Important:

```text
Agent Identity != Runtime Session
```

## Room

Business/workstream namespace.

Examples:
- VNA
- GrabGift
- Loyalty

Contains:
- member pool;
- shared room memory;
- thread list;
- high-level decisions.

Room is not one giant LLM conversation.

### Membership

Room and Thread membership are durable generations, not mutable presence
flags. Each generation records `joined_at` and an optional `left_at`; rejoining
creates a new generation. At most one generation for a member is active.

Room membership is eligibility to collaborate. It does not subscribe the Agent
to every Thread and does not grant filesystem access.

## Conversation

Two concrete types:

```text
Conversation
├── DM
└── Thread
```

## DM

1:1 context:
- user ↔ agent
- agent ↔ agent

Used for:
- single-project work;
- quick questions;
- private side work.

## Thread

Working context for a concrete collaboration.

Fields:
- `room_id`
- title
- members
- status
- goal
- optional parent thread
- optional origin conversation
- one primary WorkItem

## WorkItem

Durable unit of execution.

Statuses:

```text
OPEN
WORKING
BLOCKED
READY
DONE
FAILED
CANCELLED
```

A DM may have zero or more WorkItems.
Every Phase 4 Thread has exactly one primary WorkItem created atomically with
the Thread. Phase 4 leaves its owner unset; ownership and status transitions
remain Phase 6 work.

## Message

Transcript event.

A Message:
- is not automatically a fact;
- is not automatically memory;
- is not automatically a result.

## Result

Structured outcome from work.

Fields:
- status;
- summary;
- outputs;
- evidence;
- source work/conversation.

## Publish

Transfers a Result from one context to another without transferring transcript history.

## Dependency

Directed relation between WorkItems.

```text
upstream READY
→ downstream may continue
```

## SessionBinding

Maps:

```text
conversation + agent
→ transport + remote session
```

Each thread member gets its own runtime session.

## Checkpoint

Compact durable working state used for session recovery.

Contains:
- goal;
- current state;
- decisions;
- open items;
- references;
- last processed message.

## Memory

Promoted long-lived knowledge.

Kinds:
- FACT
- DECISION
- CONSTRAINT
- RESULT
- REFERENCE

Scopes:
- PROJECT
- ROOM
- AGENT

Thread working state should remain checkpoint/result data unless explicitly promoted.
