-- Rebuild the binding and its FK children in the migration transaction.
-- Copy every existing row before dropping the old tables; keep FK enforcement on.
DROP TRIGGER permission_decisions_no_update;
DROP TRIGGER permission_decisions_no_delete;
DROP TRIGGER permission_decisions_validate_selection;
DROP TRIGGER session_recoveries_update_guard;
DROP INDEX idx_session_binding_lookup;
DROP INDEX idx_session_binding_generation;
DROP INDEX uq_session_bindings_current;
ALTER TABLE permission_decisions RENAME TO permission_decisions_v16;
ALTER TABLE session_recoveries RENAME TO session_recoveries_v16;
ALTER TABLE session_bindings RENAME TO session_bindings_v16;

CREATE TABLE session_bindings (
    id TEXT PRIMARY KEY,
    conversation_id TEXT REFERENCES conversations(id),
    room_id TEXT REFERENCES rooms(id),
    agent_id TEXT NOT NULL REFERENCES agents(id),
    transport_type TEXT NOT NULL CHECK (trim(transport_type) <> ''),
    remote_session_id TEXT,
    generation INTEGER NOT NULL DEFAULT 1 CHECK (generation > 0),
    status TEXT NOT NULL
        CHECK (status IN ('active', 'disconnected', 'lost', 'closed')),
    created_at TEXT NOT NULL CHECK (trim(created_at) <> ''),
    last_used_at TEXT NOT NULL CHECK (trim(last_used_at) <> ''),
    CHECK ((conversation_id IS NULL) <> (room_id IS NULL)),
    UNIQUE (conversation_id, agent_id, generation),
    UNIQUE (room_id, agent_id, generation)
);

INSERT INTO session_bindings(
    id, conversation_id, agent_id, transport_type, remote_session_id,
    generation, status, created_at, last_used_at
) SELECT id, conversation_id, agent_id, transport_type, remote_session_id,
         generation, status, created_at, last_used_at FROM session_bindings_v16;

CREATE INDEX idx_session_binding_lookup
ON session_bindings(conversation_id, agent_id, status);

CREATE INDEX idx_session_binding_generation
ON session_bindings(conversation_id, agent_id, generation DESC);

CREATE UNIQUE INDEX uq_session_bindings_current
ON session_bindings(conversation_id, agent_id)
WHERE status IN ('active', 'disconnected');

CREATE UNIQUE INDEX uq_room_session_current ON session_bindings(room_id, agent_id)
WHERE room_id IS NOT NULL AND status IN ('active', 'disconnected');

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
CREATE TABLE session_recoveries (
    session_binding_id TEXT PRIMARY KEY NOT NULL REFERENCES session_bindings(id),
    source_binding_id TEXT NOT NULL UNIQUE REFERENCES session_bindings(id),
    capsule TEXT NOT NULL CHECK (trim(capsule) <> ''),
    capsule_delivered_at TEXT CHECK (
        capsule_delivered_at IS NULL OR trim(capsule_delivered_at) <> ''
    ),
    created_at TEXT NOT NULL CHECK (trim(created_at) <> ''),
    CHECK (session_binding_id <> source_binding_id)
);

CREATE TRIGGER session_recoveries_update_guard
BEFORE UPDATE ON session_recoveries
WHEN NEW.session_binding_id <> OLD.session_binding_id
  OR NEW.source_binding_id <> OLD.source_binding_id
  OR NEW.capsule <> OLD.capsule
  OR NEW.created_at <> OLD.created_at
  OR OLD.capsule_delivered_at IS NOT NULL
  OR NEW.capsule_delivered_at IS NULL
BEGIN
    SELECT RAISE(ABORT, 'session recovery permits one delivery progress update only');
END;

INSERT INTO permission_decisions SELECT * FROM permission_decisions_v16;
INSERT INTO session_recoveries SELECT * FROM session_recoveries_v16;
DROP TABLE permission_decisions_v16;
DROP TABLE session_recoveries_v16;
DROP TABLE session_bindings_v16;

CREATE TABLE room_message_activations (
    message_id TEXT NOT NULL REFERENCES room_messages(id),
    agent_id TEXT NOT NULL REFERENCES agents(id),
    session_binding_id TEXT NOT NULL REFERENCES session_bindings(id),
    status TEXT NOT NULL CHECK (status IN ('claimed', 'sent', 'completed', 'failed')),
    updated_at TEXT NOT NULL CHECK (trim(updated_at) <> ''),
    PRIMARY KEY (message_id, agent_id)
);
