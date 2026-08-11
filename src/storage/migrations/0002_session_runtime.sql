DROP INDEX idx_session_binding_lookup;
DROP INDEX idx_session_binding_generation;

ALTER TABLE session_bindings RENAME TO session_bindings_v1;

CREATE TABLE session_bindings (
    id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL REFERENCES conversations(id),
    agent_id TEXT NOT NULL REFERENCES agents(id),
    transport_type TEXT NOT NULL CHECK (trim(transport_type) <> ''),
    remote_session_id TEXT,
    generation INTEGER NOT NULL DEFAULT 1 CHECK (generation > 0),
    status TEXT NOT NULL
        CHECK (status IN ('active', 'disconnected', 'lost', 'closed')),
    created_at TEXT NOT NULL CHECK (trim(created_at) <> ''),
    last_used_at TEXT NOT NULL CHECK (trim(last_used_at) <> ''),
    UNIQUE (conversation_id, agent_id, generation)
);

INSERT INTO session_bindings(
    id, conversation_id, agent_id, transport_type, remote_session_id,
    generation, status, created_at, last_used_at
)
SELECT
    id, conversation_id, agent_id, transport_type, remote_session_id,
    generation, status, created_at, last_used_at
FROM session_bindings_v1;

DROP TABLE session_bindings_v1;

CREATE INDEX idx_session_binding_lookup
ON session_bindings(conversation_id, agent_id, status);

CREATE INDEX idx_session_binding_generation
ON session_bindings(conversation_id, agent_id, generation DESC);

CREATE UNIQUE INDEX uq_session_bindings_current
ON session_bindings(conversation_id, agent_id)
WHERE status IN ('active', 'disconnected');

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
