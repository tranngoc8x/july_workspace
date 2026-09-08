CREATE TABLE room_message_publications (
    trigger_message_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    request_id TEXT NOT NULL CHECK (trim(request_id) <> ''),
    published_message_id TEXT NOT NULL UNIQUE REFERENCES room_messages(id),
    PRIMARY KEY (trigger_message_id, agent_id, request_id),
    FOREIGN KEY (trigger_message_id, agent_id) REFERENCES room_message_activations(message_id, agent_id)
);
CREATE TRIGGER room_message_publications_no_update BEFORE UPDATE ON room_message_publications
BEGIN SELECT RAISE(ABORT, 'room publications are append only'); END;
CREATE TRIGGER room_message_publications_no_delete BEFORE DELETE ON room_message_publications
BEGIN SELECT RAISE(ABORT, 'room publications are append only'); END;
