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
The following operation groups remain requirements for the phases that add
their lifecycle behavior.

Must be atomic:
- thread + members + primary work creation;
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

## No dual source of truth

Do not keep Beads or runtime JSON registries synchronized with SQLite.

Generated debug/export files may be produced from SQLite, but never become canonical.
