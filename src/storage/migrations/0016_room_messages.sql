CREATE TABLE room_messages (
    id TEXT PRIMARY KEY,
    room_id TEXT NOT NULL REFERENCES rooms(id),
    sender_type TEXT NOT NULL CHECK (sender_type IN ('user', 'agent')),
    sender_id TEXT NOT NULL CHECK (trim(sender_id) <> ''),
    body TEXT NOT NULL CHECK (trim(body) <> ''),
    mentions_json TEXT NOT NULL CHECK (json_valid(mentions_json)),
    reply_to TEXT REFERENCES room_messages(id),
    created_at TEXT NOT NULL CHECK (trim(created_at) <> '')
);

CREATE INDEX idx_room_messages_order ON room_messages(room_id, created_at, id);
