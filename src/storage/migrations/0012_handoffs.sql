CREATE TABLE handoffs (
    id TEXT PRIMARY KEY,
    thread_id TEXT NOT NULL REFERENCES conversations(id),
    work_id TEXT NOT NULL REFERENCES work_items(id),
    from_agent_id TEXT NOT NULL REFERENCES agents(id),
    to_agent_id TEXT NOT NULL REFERENCES agents(id),
    status TEXT NOT NULL CHECK (
        status IN (
            'proposed', 'accepted', 'rejected', 'partial',
            'disputed', 'resolved', 'cancelled'
        )
    ),
    reason TEXT,
    evidence_json TEXT NOT NULL DEFAULT '[]',
    owned_scope_json TEXT NOT NULL DEFAULT '[]',
    rejected_scope_json TEXT NOT NULL DEFAULT '[]',
    proposed_owner_id TEXT REFERENCES agents(id),
    round_count INTEGER NOT NULL DEFAULT 0 CHECK (round_count >= 0),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    CHECK (from_agent_id <> to_agent_id)
);

CREATE INDEX handoffs_work ON handoffs(work_id);
CREATE INDEX handoffs_thread ON handoffs(thread_id);

-- One negotiation per work item at a time: a second open handoff would let two
-- agents claim the same ownership question in parallel.
CREATE UNIQUE INDEX handoffs_one_open_per_work
ON handoffs(work_id)
WHERE status IN ('proposed', 'rejected', 'partial', 'disputed');
