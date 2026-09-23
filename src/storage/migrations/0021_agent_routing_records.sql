-- Every routing judgment July acted on, including the ones it refused to act
-- on: a miss is the measurement that matters most.
CREATE TABLE agent_routing_records (
    id TEXT PRIMARY KEY,
    room_id TEXT NOT NULL REFERENCES rooms(id),
    -- Present only when the decision actually sent a message.
    message_id TEXT REFERENCES room_messages(id),
    task TEXT NOT NULL CHECK (trim(task) <> ''),
    source TEXT NOT NULL CHECK (source IN ('explicit_mention', 'rule', 'jev', 'human')),
    selected_agent_id TEXT REFERENCES agents(id),
    confidence REAL CHECK (confidence IS NULL OR (confidence >= 0 AND confidence <= 1)),
    candidate_ids_json TEXT NOT NULL CHECK (json_valid(candidate_ids_json)),
    created_at TEXT NOT NULL CHECK (trim(created_at) <> ''),
    -- A record that sent a message must name the agent it sent to.
    CHECK (message_id IS NULL OR selected_agent_id IS NOT NULL)
);

CREATE INDEX idx_agent_routing_records_order ON agent_routing_records(room_id, created_at, id);
