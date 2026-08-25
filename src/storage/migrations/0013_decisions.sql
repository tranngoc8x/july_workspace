CREATE TABLE decisions (
    id TEXT PRIMARY KEY,
    thread_id TEXT NOT NULL REFERENCES conversations(id),
    decision_type TEXT NOT NULL CHECK (decision_type IN ('ownership', 'technical', 'scope')),
    title TEXT NOT NULL,
    decision TEXT,
    reason TEXT,
    selected_proposal_id TEXT,
    alternatives_json TEXT NOT NULL DEFAULT '[]',
    evidence_json TEXT NOT NULL DEFAULT '[]',
    participants_json TEXT NOT NULL DEFAULT '[]',
    decision_owner TEXT NOT NULL,
    status TEXT NOT NULL CHECK (
        status IN ('pending', 'needs_decision', 'decided', 'superseded', 'cancelled')
    ),
    supersedes_decision_id TEXT REFERENCES decisions(id),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    -- A decision states its outcome once it is settled, and keeps stating it
    -- after a later decision supersedes it.
    CHECK (
        (status IN ('decided', 'superseded'))
        = (decision IS NOT NULL AND TRIM(decision) <> '')
    )
);

CREATE INDEX decisions_thread ON decisions(thread_id);

-- A superseded decision is replaced exactly once, so history stays a chain.
CREATE UNIQUE INDEX decisions_single_successor
ON decisions(supersedes_decision_id)
WHERE supersedes_decision_id IS NOT NULL;

-- An escalated handoff points at the decision that must settle it.
ALTER TABLE handoffs ADD COLUMN decision_id TEXT REFERENCES decisions(id);
