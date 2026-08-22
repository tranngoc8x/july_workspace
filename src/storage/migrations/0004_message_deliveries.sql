CREATE TABLE message_deliveries (
    message_id TEXT NOT NULL REFERENCES messages(id),
    target_agent_id TEXT NOT NULL REFERENCES agents(id),
    status TEXT NOT NULL CHECK (status IN ('pending', 'delivered', 'failed')),
    capsule TEXT CHECK (capsule IS NULL OR trim(capsule) <> ''),
    capsule_delivered_at TEXT CHECK (
        capsule_delivered_at IS NULL OR trim(capsule_delivered_at) <> ''
    ),
    created_at TEXT NOT NULL CHECK (trim(created_at) <> ''),
    updated_at TEXT NOT NULL CHECK (trim(updated_at) <> ''),
    delivered_at TEXT CHECK (delivered_at IS NULL OR trim(delivered_at) <> ''),
    PRIMARY KEY (message_id, target_agent_id),
    CHECK (capsule_delivered_at IS NULL OR capsule IS NOT NULL),
    CHECK (
        (status = 'delivered' AND delivered_at IS NOT NULL)
        OR (status <> 'delivered' AND delivered_at IS NULL)
    )
);
