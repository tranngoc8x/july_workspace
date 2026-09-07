-- Stable append order independent of user timestamps, IDs and VACUUM.
CREATE TABLE room_message_order (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    message_id TEXT NOT NULL UNIQUE REFERENCES room_messages(id)
);
INSERT INTO room_message_order(message_id) SELECT id FROM room_messages ORDER BY rowid;
CREATE TRIGGER room_message_append_order AFTER INSERT ON room_messages BEGIN
    INSERT INTO room_message_order(message_id) VALUES (NEW.id);
END;

CREATE TABLE agent_room_cursors (
    agent_id TEXT NOT NULL REFERENCES agents(id),
    room_id TEXT NOT NULL REFERENCES rooms(id),
    last_seen_message_id TEXT NOT NULL REFERENCES room_messages(id),
    PRIMARY KEY (agent_id, room_id)
);
