CREATE TABLE schema_migrations (
    version INTEGER PRIMARY KEY CHECK (version > 0),
    applied_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE TABLE agents (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE CHECK (trim(name) <> ''),
    project_root TEXT NOT NULL CHECK (trim(project_root) <> ''),
    transport_type TEXT NOT NULL CHECK (trim(transport_type) <> ''),
    transport_config_json TEXT NOT NULL CHECK (json_valid(transport_config_json)),
    status TEXT NOT NULL DEFAULT 'active' CHECK (trim(status) <> ''),
    metadata_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(metadata_json)),
    created_at TEXT NOT NULL CHECK (trim(created_at) <> ''),
    updated_at TEXT NOT NULL CHECK (trim(updated_at) <> '')
);

CREATE TABLE rooms (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE CHECK (trim(name) <> ''),
    description TEXT,
    status TEXT NOT NULL DEFAULT 'active' CHECK (trim(status) <> ''),
    created_at TEXT NOT NULL CHECK (trim(created_at) <> ''),
    updated_at TEXT NOT NULL CHECK (trim(updated_at) <> '')
);

CREATE TABLE room_members (
    room_id TEXT NOT NULL REFERENCES rooms(id),
    agent_id TEXT NOT NULL REFERENCES agents(id),
    role TEXT,
    joined_at TEXT NOT NULL CHECK (trim(joined_at) <> ''),
    PRIMARY KEY (room_id, agent_id)
);

CREATE TABLE conversations (
    id TEXT PRIMARY KEY,
    type TEXT NOT NULL CHECK (type IN ('dm', 'thread')),
    room_id TEXT REFERENCES rooms(id),
    title TEXT,
    goal TEXT,
    parent_conversation_id TEXT REFERENCES conversations(id),
    origin_conversation_id TEXT REFERENCES conversations(id),
    status TEXT NOT NULL DEFAULT 'open' CHECK (trim(status) <> ''),
    created_at TEXT NOT NULL CHECK (trim(created_at) <> ''),
    updated_at TEXT NOT NULL CHECK (trim(updated_at) <> ''),
    CHECK (
        (type = 'dm' AND room_id IS NULL)
        OR (type = 'thread' AND room_id IS NOT NULL)
    ),
    CHECK (type = 'dm' OR (title IS NOT NULL AND trim(title) <> ''))
);

CREATE TABLE conversation_members (
    conversation_id TEXT NOT NULL REFERENCES conversations(id),
    member_type TEXT NOT NULL CHECK (member_type IN ('user', 'agent')),
    member_id TEXT NOT NULL CHECK (trim(member_id) <> ''),
    joined_at TEXT NOT NULL CHECK (trim(joined_at) <> ''),
    left_at TEXT,
    PRIMARY KEY (conversation_id, member_type, member_id)
);

CREATE TABLE messages (
    id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL REFERENCES conversations(id),
    sender_type TEXT NOT NULL CHECK (sender_type IN ('user', 'agent')),
    sender_id TEXT NOT NULL CHECK (trim(sender_id) <> ''),
    body TEXT NOT NULL CHECK (trim(body) <> ''),
    reply_to TEXT REFERENCES messages(id),
    metadata_json TEXT CHECK (metadata_json IS NULL OR json_valid(metadata_json)),
    created_at TEXT NOT NULL CHECK (trim(created_at) <> '')
);

CREATE TABLE work_items (
    id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL REFERENCES conversations(id),
    title TEXT NOT NULL CHECK (trim(title) <> ''),
    goal TEXT,
    status TEXT NOT NULL CHECK (
        status IN ('open', 'working', 'blocked', 'ready', 'done', 'failed', 'cancelled')
    ),
    owner_agent_id TEXT REFERENCES agents(id),
    created_at TEXT NOT NULL CHECK (trim(created_at) <> ''),
    updated_at TEXT NOT NULL CHECK (trim(updated_at) <> ''),
    completed_at TEXT
);

CREATE TABLE work_dependencies (
    upstream_work_id TEXT NOT NULL REFERENCES work_items(id),
    downstream_work_id TEXT NOT NULL REFERENCES work_items(id),
    dependency_type TEXT NOT NULL DEFAULT 'requires' CHECK (dependency_type = 'requires'),
    created_at TEXT NOT NULL CHECK (trim(created_at) <> ''),
    PRIMARY KEY (upstream_work_id, downstream_work_id),
    CHECK (upstream_work_id <> downstream_work_id)
);

CREATE TABLE work_results (
    id TEXT PRIMARY KEY,
    work_id TEXT NOT NULL REFERENCES work_items(id),
    status TEXT NOT NULL CHECK (trim(status) <> ''),
    summary TEXT NOT NULL CHECK (trim(summary) <> ''),
    outputs_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(outputs_json)),
    evidence_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(evidence_json)),
    supersedes_result_id TEXT REFERENCES work_results(id),
    created_at TEXT NOT NULL CHECK (trim(created_at) <> '')
);

CREATE TABLE publishes (
    id TEXT PRIMARY KEY,
    result_id TEXT NOT NULL REFERENCES work_results(id),
    source_conversation_id TEXT NOT NULL REFERENCES conversations(id),
    target_conversation_id TEXT NOT NULL REFERENCES conversations(id),
    created_at TEXT NOT NULL CHECK (trim(created_at) <> ''),
    UNIQUE (result_id, target_conversation_id)
);

CREATE TABLE session_bindings (
    id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL REFERENCES conversations(id),
    agent_id TEXT NOT NULL REFERENCES agents(id),
    transport_type TEXT NOT NULL CHECK (trim(transport_type) <> ''),
    remote_session_id TEXT,
    generation INTEGER NOT NULL DEFAULT 1 CHECK (generation > 0),
    status TEXT NOT NULL CHECK (trim(status) <> ''),
    created_at TEXT NOT NULL CHECK (trim(created_at) <> ''),
    last_used_at TEXT NOT NULL CHECK (trim(last_used_at) <> ''),
    UNIQUE (conversation_id, agent_id, generation)
);

CREATE TABLE checkpoints (
    id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL REFERENCES conversations(id),
    agent_id TEXT NOT NULL REFERENCES agents(id),
    goal TEXT,
    current_state TEXT,
    decisions_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(decisions_json)),
    open_items_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(open_items_json)),
    references_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(references_json)),
    last_message_id TEXT REFERENCES messages(id),
    created_at TEXT NOT NULL CHECK (trim(created_at) <> '')
);

CREATE TABLE memories (
    id TEXT PRIMARY KEY,
    scope_type TEXT NOT NULL CHECK (scope_type IN ('project', 'room', 'agent')),
    scope_id TEXT NOT NULL CHECK (trim(scope_id) <> ''),
    kind TEXT NOT NULL CHECK (kind IN ('fact', 'decision', 'constraint', 'result', 'reference')),
    content TEXT NOT NULL CHECK (trim(content) <> ''),
    source_conversation_id TEXT REFERENCES conversations(id),
    evidence_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(evidence_json)),
    supersedes_memory_id TEXT REFERENCES memories(id),
    created_at TEXT NOT NULL CHECK (trim(created_at) <> '')
);

CREATE INDEX idx_messages_conversation_created
ON messages(conversation_id, created_at, id);

CREATE INDEX idx_work_conversation
ON work_items(conversation_id);

CREATE INDEX idx_session_binding_lookup
ON session_bindings(conversation_id, agent_id, status);

CREATE INDEX idx_memory_scope
ON memories(scope_type, scope_id, kind);

CREATE INDEX idx_session_binding_generation
ON session_bindings(conversation_id, agent_id, generation DESC);

CREATE VIRTUAL TABLE messages_fts USING fts5(
    body,
    content = 'messages',
    content_rowid = 'rowid'
);

CREATE TRIGGER messages_fts_insert AFTER INSERT ON messages BEGIN
    INSERT INTO messages_fts(rowid, body) VALUES (new.rowid, new.body);
END;

CREATE TRIGGER messages_fts_delete AFTER DELETE ON messages BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, body)
    VALUES ('delete', old.rowid, old.body);
END;

CREATE TRIGGER messages_fts_update AFTER UPDATE ON messages BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, body)
    VALUES ('delete', old.rowid, old.body);
    INSERT INTO messages_fts(rowid, body) VALUES (new.rowid, new.body);
END;

CREATE VIRTUAL TABLE work_results_fts USING fts5(
    summary,
    outputs_json,
    evidence_json,
    content = 'work_results',
    content_rowid = 'rowid'
);

CREATE TRIGGER work_results_fts_insert AFTER INSERT ON work_results BEGIN
    INSERT INTO work_results_fts(rowid, summary, outputs_json, evidence_json)
    VALUES (new.rowid, new.summary, new.outputs_json, new.evidence_json);
END;

CREATE TRIGGER work_results_fts_delete AFTER DELETE ON work_results BEGIN
    INSERT INTO work_results_fts(work_results_fts, rowid, summary, outputs_json, evidence_json)
    VALUES ('delete', old.rowid, old.summary, old.outputs_json, old.evidence_json);
END;

CREATE TRIGGER work_results_fts_update AFTER UPDATE ON work_results BEGIN
    INSERT INTO work_results_fts(work_results_fts, rowid, summary, outputs_json, evidence_json)
    VALUES ('delete', old.rowid, old.summary, old.outputs_json, old.evidence_json);
    INSERT INTO work_results_fts(rowid, summary, outputs_json, evidence_json)
    VALUES (new.rowid, new.summary, new.outputs_json, new.evidence_json);
END;

CREATE VIRTUAL TABLE memories_fts USING fts5(
    content,
    evidence_json,
    content = 'memories',
    content_rowid = 'rowid'
);

CREATE TRIGGER memories_fts_insert AFTER INSERT ON memories BEGIN
    INSERT INTO memories_fts(rowid, content, evidence_json)
    VALUES (new.rowid, new.content, new.evidence_json);
END;

CREATE TRIGGER memories_fts_delete AFTER DELETE ON memories BEGIN
    INSERT INTO memories_fts(memories_fts, rowid, content, evidence_json)
    VALUES ('delete', old.rowid, old.content, old.evidence_json);
END;

CREATE TRIGGER memories_fts_update AFTER UPDATE ON memories BEGIN
    INSERT INTO memories_fts(memories_fts, rowid, content, evidence_json)
    VALUES ('delete', old.rowid, old.content, old.evidence_json);
    INSERT INTO memories_fts(rowid, content, evidence_json)
    VALUES (new.rowid, new.content, new.evidence_json);
END;
