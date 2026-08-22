# July Workspace — SQLite Storage

## Goals

SQLite is the canonical workspace store.

Requirements:
- local-first;
- transactional;
- crash-safe;
- zero server dependency;
- inspectable;
- migration-friendly;
- searchable with FTS5;
- suitable for single-user concurrent agent activity.

Recommended database:

```text
~/.july/workspace.db
```

Do not store runtime DB inside project repositories.

Phase 1 uses canonical 26-character ULID text through distinct Rust ID types.
`SqliteStore` owns one synchronous `rusqlite::Connection`; async runtime and
connection pooling are deferred until a measured runtime need exists.

## Tables

The snippets below show the record shape. The executable source of constraints,
indexes, and triggers is `src/storage/migrations/0001_workspace.sql`.

### agents

```sql
CREATE TABLE agents (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL UNIQUE,
  project_root TEXT NOT NULL,
  transport_type TEXT NOT NULL,
  transport_config_json TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'active',
  metadata_json TEXT NOT NULL DEFAULT '{}',
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
```

### rooms

```sql
CREATE TABLE rooms (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL UNIQUE,
  description TEXT,
  status TEXT NOT NULL DEFAULT 'active',
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
```

### room_members

```sql
CREATE TABLE room_members (
  room_id TEXT NOT NULL REFERENCES rooms(id),
  agent_id TEXT NOT NULL REFERENCES agents(id),
  role TEXT,
  joined_at TEXT NOT NULL,
  PRIMARY KEY(room_id, agent_id)
);
```

### conversations

```sql
CREATE TABLE conversations (
  id TEXT PRIMARY KEY,
  type TEXT NOT NULL, -- dm | thread
  room_id TEXT REFERENCES rooms(id),
  title TEXT,
  goal TEXT,
  parent_conversation_id TEXT REFERENCES conversations(id),
  origin_conversation_id TEXT REFERENCES conversations(id),
  status TEXT NOT NULL DEFAULT 'open',
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
```

### conversation_members

```sql
CREATE TABLE conversation_members (
  conversation_id TEXT NOT NULL REFERENCES conversations(id),
  member_type TEXT NOT NULL, -- user | agent
  member_id TEXT NOT NULL,
  joined_at TEXT NOT NULL,
  left_at TEXT,
  PRIMARY KEY(conversation_id, member_type, member_id)
);
```

### messages

```sql
CREATE TABLE messages (
  id TEXT PRIMARY KEY,
  conversation_id TEXT NOT NULL REFERENCES conversations(id),
  sender_type TEXT NOT NULL,
  sender_id TEXT NOT NULL,
  body TEXT NOT NULL,
  reply_to TEXT REFERENCES messages(id),
  metadata_json TEXT,
  created_at TEXT NOT NULL
);
```

### message_deliveries

Phase 5.3 adds one delivery row per `(message_id, target_agent_id)`. The
durable state is exactly `pending`, `delivered`, or `failed`:

```sql
CREATE TABLE message_deliveries (
  message_id TEXT NOT NULL REFERENCES messages(id),
  target_agent_id TEXT NOT NULL REFERENCES agents(id),
  status TEXT NOT NULL CHECK (status IN ('pending', 'delivered', 'failed')),
  capsule TEXT,
  capsule_delivered_at TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  delivered_at TEXT,
  PRIMARY KEY(message_id, target_agent_id)
);
```

The executable migration also rejects blank timestamps/capsules, capsule
progress without a capsule, and inconsistent `delivered_at` values. Message
and target delivery are inserted atomically before transport. `delivered` is
terminal and means the existing transport accepted the send. Owner, open, or
send failures leave the message durable and transition its delivery to
`failed`; the typed runtime outcome reports the structured failure, while raw
provider errors are not stored as a new persistence field. Existing messages
without a delivery row remain valid.

Explicit retry claims only `failed`, restores `pending`, and reuses the stored
message body and exact target. Thread retry revalidates active Agent, Room, and
Thread membership without implicitly rejoining; a persisted Thread capsule is
tracked separately so a successful capsule is not resent.

At Phase 5.4 startup, pre-existing `pending` rows are reconciled to `failed`
before the storage worker is ready. Reconciliation sends nothing: only an
explicit retry after restart may claim the row, retaining its stored target,
body, and capsule progress.

### work_items

```sql
CREATE TABLE work_items (
  id TEXT PRIMARY KEY,
  conversation_id TEXT NOT NULL REFERENCES conversations(id),
  title TEXT NOT NULL,
  goal TEXT,
  status TEXT NOT NULL,
  owner_agent_id TEXT REFERENCES agents(id),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  completed_at TEXT
);
```

### work_dependencies

```sql
CREATE TABLE work_dependencies (
  upstream_work_id TEXT NOT NULL REFERENCES work_items(id),
  downstream_work_id TEXT NOT NULL REFERENCES work_items(id),
  dependency_type TEXT NOT NULL DEFAULT 'requires',
  status TEXT NOT NULL DEFAULT 'waiting'
    CHECK (status IN ('waiting', 'satisfied', 'failed', 'superseded')),
  result_id TEXT REFERENCES work_results(id),
  created_at TEXT NOT NULL,
  PRIMARY KEY(upstream_work_id, downstream_work_id)
);
```

### work_results

```sql
CREATE TABLE work_results (
  id TEXT PRIMARY KEY,
  work_id TEXT NOT NULL REFERENCES work_items(id),
  status TEXT NOT NULL,
  summary TEXT NOT NULL,
  outputs_json TEXT NOT NULL DEFAULT '[]',
  evidence_json TEXT NOT NULL DEFAULT '[]',
  supersedes_result_id TEXT REFERENCES work_results(id),
  created_at TEXT NOT NULL
);
```

### publishes

```sql
CREATE TABLE publishes (
  id TEXT PRIMARY KEY,
  result_id TEXT NOT NULL REFERENCES work_results(id),
  source_conversation_id TEXT NOT NULL REFERENCES conversations(id),
  target_conversation_id TEXT NOT NULL REFERENCES conversations(id),
  created_at TEXT NOT NULL
);
```

### session_bindings

```sql
CREATE TABLE session_bindings (
  id TEXT PRIMARY KEY,
  conversation_id TEXT NOT NULL REFERENCES conversations(id),
  agent_id TEXT NOT NULL REFERENCES agents(id),
  transport_type TEXT NOT NULL,
  remote_session_id TEXT,
  generation INTEGER NOT NULL DEFAULT 1,
  status TEXT NOT NULL,
  created_at TEXT NOT NULL,
  last_used_at TEXT NOT NULL
);
```

### checkpoints

```sql
CREATE TABLE checkpoints (
  id TEXT PRIMARY KEY,
  conversation_id TEXT NOT NULL REFERENCES conversations(id),
  agent_id TEXT NOT NULL REFERENCES agents(id),
  goal TEXT,
  current_state TEXT,
  decisions_json TEXT NOT NULL DEFAULT '[]',
  open_items_json TEXT NOT NULL DEFAULT '[]',
  references_json TEXT NOT NULL DEFAULT '[]',
  last_message_id TEXT REFERENCES messages(id),
  created_at TEXT NOT NULL
);
```

### memories

```sql
CREATE TABLE memories (
  id TEXT PRIMARY KEY,
  scope_type TEXT NOT NULL, -- project | room | agent
  scope_id TEXT NOT NULL,
  kind TEXT NOT NULL, -- fact | decision | constraint | result | reference
  content TEXT NOT NULL,
  source_conversation_id TEXT REFERENCES conversations(id),
  evidence_json TEXT NOT NULL DEFAULT '[]',
  supersedes_memory_id TEXT REFERENCES memories(id),
  created_at TEXT NOT NULL
);
```

## Indexes

At minimum:

```sql
CREATE INDEX idx_messages_conversation_created
ON messages(conversation_id, created_at, id);

CREATE INDEX idx_work_conversation
ON work_items(conversation_id);

CREATE INDEX idx_session_binding_lookup
ON session_bindings(conversation_id, agent_id, status);

CREATE INDEX idx_memory_scope
ON memories(scope_type, scope_id, kind);
```

## SQLite settings

Applied when `SqliteStore` opens the database:

```sql
PRAGMA journal_mode=WAL;
PRAGMA foreign_keys=ON;
PRAGMA busy_timeout=5000;
```

## Transactions

Phase 1 implements atomic Room/member and Conversation/member batch inserts.
Phase 4 replaces Thread creation with one aggregate `BEGIN IMMEDIATE`
transaction that validates the active Room and initial Agent memberships, then
inserts the open Thread, local-user membership, initial Agent memberships and
one open primary WorkItem. The Work title and goal mirror the Thread; its owner
is null until Phase 6. Any failure rolls back the complete aggregate.

Session bindings, ACP calls, capsules, Messages, Results, Publishes and
Dependencies never participate in the Thread creation transaction. Session
startup is lazy after commit.

The following operation groups remain requirements for the phases that add
their lifecycle behavior.

Must be atomic:
- work completion + result creation;
- result publish;
- dependency update;
- session generation replacement;
- memory promotion.

## FTS5

Phase 1 creates external-content FTS5 tables and insert/update/delete sync
triggers only for:
- messages;
- results;
- memories.

Do not introduce embeddings initially.

Structured fields use SQLite JSON validation. Message metadata is nullable;
non-null metadata and all array/object payload columns must contain valid JSON.

All foreign keys use SQLite's restrictive default action. Parent rows cannot
be deleted while durable child records still reference them.

Phase 3 message writes are exact-idempotent by `message.id`: retrying the same
record succeeds after an ambiguous worker acknowledgement, while a different
record with the same ID returns a typed conflict and leaves SQLite unchanged.

## Schema migrations

Use immutable numbered migrations and:

```sql
CREATE TABLE schema_migrations (
  version INTEGER PRIMARY KEY,
  applied_at TEXT NOT NULL
);
```

The greenfield baseline is `0001_workspace.sql`; there are no historical July
migrations.

### Phase 2 migration `0002`

`0002_session_runtime.sql` rebuilds `session_bindings`, copies every existing
column and row, and preserves its foreign keys and
`UNIQUE (conversation_id, agent_id, generation)` constraint. It adds the exact
status constraint below.

```sql
status TEXT NOT NULL
  CHECK (status IN ('active', 'disconnected', 'lost', 'closed'))
```

After the rebuild, the migration recreates both v1 indexes and adds one
current-binding constraint:

```sql
CREATE INDEX idx_session_binding_lookup
ON session_bindings(conversation_id, agent_id, status);

CREATE INDEX idx_session_binding_generation
ON session_bindings(conversation_id, agent_id, generation DESC);

CREATE UNIQUE INDEX uq_session_bindings_current
ON session_bindings(conversation_id, agent_id)
WHERE status IN ('active', 'disconnected');
```

Copying a row with an unknown status fails the new `CHECK`. Creating the partial
index fails if v1 contains more than one current generation for a
Conversation/Agent pair. Either failure rolls back the entire migration; July
does not guess a status, select a winning generation or partially rebuild the
table.

Permission decisions use one append-only row per resolved request:

```sql
CREATE TABLE permission_decisions (
  id TEXT PRIMARY KEY,
  session_binding_id TEXT NOT NULL REFERENCES session_bindings(id),
  correlation_id TEXT NOT NULL,
  options_json TEXT NOT NULL
    CHECK (json_valid(options_json) AND json_type(options_json) = 'array'),
  outcome TEXT NOT NULL CHECK (outcome IN ('selected', 'cancelled')),
  selected_option_id TEXT,
  decided_at TEXT NOT NULL CHECK (trim(decided_at) <> ''),
  UNIQUE (session_binding_id, correlation_id),
  CHECK (
    (outcome = 'selected' AND selected_option_id IS NOT NULL
      AND trim(selected_option_id) <> '')
    OR (outcome = 'cancelled' AND selected_option_id IS NULL)
  )
);

CREATE TRIGGER permission_decisions_no_update
BEFORE UPDATE ON permission_decisions BEGIN
  SELECT RAISE(ABORT, 'permission decisions are append-only');
END;

CREATE TRIGGER permission_decisions_no_delete
BEFORE DELETE ON permission_decisions BEGIN
  SELECT RAISE(ABORT, 'permission decisions are append-only');
END;

CREATE TRIGGER permission_decisions_validate_selection
BEFORE INSERT ON permission_decisions
WHEN EXISTS (
  SELECT 1 FROM json_each(NEW.options_json)
  WHERE json_type(NEW.options_json, '$[' || key || ']') IS NOT 'object'
     OR json_type(NEW.options_json, '$[' || key || '].id') IS NOT 'text'
     OR trim(json_extract(NEW.options_json, '$[' || key || '].id')) = ''
     OR json_type(NEW.options_json, '$[' || key || '].label') IS NOT 'text'
     OR trim(json_extract(NEW.options_json, '$[' || key || '].label')) = ''
)
OR (
  NEW.outcome = 'selected'
  AND NOT EXISTS (
    SELECT 1 FROM json_each(NEW.options_json)
    WHERE json_extract(NEW.options_json, '$[' || key || '].id')
      = NEW.selected_option_id
  )
)
BEGIN
  SELECT RAISE(ABORT, 'selected permission option was not advertised');
END;
```

`options_json` is an array of normalized July-owned `{ "id", "label" }`
objects. Raw
ACP payloads, tool arguments, secrets and hidden reasoning are not stored.
The migration is one transaction through the existing migration runner.

### Phase 4 migration `0003`

The locked Phase 4 migration rebuilds both membership tables as generational
history. Existing rows become generation `1`. Existing `conversation_members`
retain their current `left_at`; existing `room_members` remain active because
the v1 table has no leave column.

```text
room_members new columns:
  generation INTEGER NOT NULL CHECK (generation > 0)
  left_at TEXT

room_members primary key:
  (room_id, agent_id, generation)

conversation_members new column:
  generation INTEGER NOT NULL CHECK (generation > 0)

conversation_members primary key:
  (conversation_id, member_type, member_id, generation)
```

Partial unique indexes allow at most one active generation for each natural
membership key:

```sql
CREATE UNIQUE INDEX uq_room_members_active
ON room_members(room_id, agent_id)
WHERE left_at IS NULL;

CREATE UNIQUE INDEX uq_conversation_members_active
ON conversation_members(conversation_id, member_type, member_id)
WHERE left_at IS NULL;
```

The rebuild preserves `role`, `joined_at`, existing `left_at` values and every
restrictive foreign key. An add while active is a no-op; a rejoin inserts
generation `MAX(generation) + 1`; removal sets `left_at` only on the active
generation. All membership transitions use one application-generated UTC
timestamp, and no transition deletes or rewrites a historical generation.

`0003` also adds the primary marker without rebuilding `work_items`:

```sql
ALTER TABLE work_items
ADD COLUMN is_primary INTEGER NOT NULL DEFAULT 0
CHECK (is_primary IN (0, 1));

CREATE UNIQUE INDEX uq_work_items_primary_conversation
ON work_items(conversation_id)
WHERE is_primary = 1;
```

Using `ALTER TABLE` preserves `idx_work_conversation` and avoids rebuilding a
table already referenced by dependencies and results. Existing WorkItems
migrate with `is_primary = 0`; July does not infer historical primary
ownership. The aggregate Phase 4 create operation is responsible for inserting
one primary WorkItem for every new Thread.

### Phase 6 migrations `0005` through `0010`

Phase 6 adds dependency status and the optional structured Result reference.
New public Work inserts are non-primary, `open`, and unowned; ownership,
lifecycle transitions, and the first Result/`ready` transition use their
guarded operations.

`0010_phase6_invariants.sql` reconciles legacy rows before installing guards:

- invalid self/cross-Work Result correction links are cleared;
- Publish source is re-derived from Result -> Work -> Conversation;
- `satisfied`/`superseded` dependency rows become `waiting` without a Result
  unless the Result belongs to the upstream Work and that Work is `ready` or
  `done`.

After migration, WorkResults are append-only. A correction must reference an
existing different Result from the same Work. Publish source remains derived,
and a consumable dependency cannot outlive the matching upstream
Result/`ready|done` state. The migration and its schema-version record commit
atomically.

Phase 5.4 message delivery is at-least-once. A crash after transport accepts a
message but before SQLite records `delivered` can cause an explicit retry to
send the same exact body again. Exactly-once delivery is not promised.

## No dual source of truth

Do not keep Beads or runtime JSON registries synchronized with SQLite.

Generated debug/export files may be produced from SQLite, but never become canonical.
