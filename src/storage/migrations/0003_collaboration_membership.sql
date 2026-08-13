ALTER TABLE room_members RENAME TO room_members_v2;

CREATE TABLE room_members (
    room_id TEXT NOT NULL REFERENCES rooms(id),
    agent_id TEXT NOT NULL REFERENCES agents(id),
    role TEXT,
    generation INTEGER NOT NULL CHECK (generation > 0),
    joined_at TEXT NOT NULL CHECK (trim(joined_at) <> ''),
    left_at TEXT,
    PRIMARY KEY (room_id, agent_id, generation)
);

INSERT INTO room_members(room_id, agent_id, role, generation, joined_at, left_at)
SELECT room_id, agent_id, role, 1, joined_at, NULL
FROM room_members_v2;

DROP TABLE room_members_v2;

CREATE UNIQUE INDEX uq_room_members_active
ON room_members(room_id, agent_id)
WHERE left_at IS NULL;

ALTER TABLE conversation_members RENAME TO conversation_members_v2;

CREATE TABLE conversation_members (
    conversation_id TEXT NOT NULL REFERENCES conversations(id),
    member_type TEXT NOT NULL CHECK (member_type IN ('user', 'agent')),
    member_id TEXT NOT NULL CHECK (trim(member_id) <> ''),
    generation INTEGER NOT NULL CHECK (generation > 0),
    joined_at TEXT NOT NULL CHECK (trim(joined_at) <> ''),
    left_at TEXT,
    PRIMARY KEY (conversation_id, member_type, member_id, generation)
);

INSERT INTO conversation_members(
    conversation_id, member_type, member_id, generation, joined_at, left_at
)
SELECT conversation_id, member_type, member_id, 1, joined_at, left_at
FROM conversation_members_v2;

DROP TABLE conversation_members_v2;

CREATE UNIQUE INDEX uq_conversation_members_active
ON conversation_members(conversation_id, member_type, member_id)
WHERE left_at IS NULL;

ALTER TABLE work_items
ADD COLUMN is_primary INTEGER NOT NULL DEFAULT 0
CHECK (is_primary IN (0, 1));

CREATE UNIQUE INDEX uq_work_items_primary_conversation
ON work_items(conversation_id)
WHERE is_primary = 1;
