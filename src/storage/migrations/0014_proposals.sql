CREATE TABLE proposals (
    id TEXT PRIMARY KEY,
    thread_id TEXT NOT NULL REFERENCES conversations(id),
    author_agent_id TEXT NOT NULL REFERENCES agents(id),
    title TEXT NOT NULL,
    problem_statement TEXT,
    approach TEXT,
    benefits_json TEXT NOT NULL DEFAULT '[]',
    costs_json TEXT NOT NULL DEFAULT '[]',
    risks_json TEXT NOT NULL DEFAULT '[]',
    assumptions_json TEXT NOT NULL DEFAULT '[]',
    evidence_json TEXT NOT NULL DEFAULT '[]',
    status TEXT NOT NULL CHECK (
        status IN ('open', 'amended', 'accepted', 'rejected', 'superseded', 'withdrawn')
    ),
    supersedes_proposal_id TEXT REFERENCES proposals(id),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX proposals_thread ON proposals(thread_id);

-- A superseded proposal is replaced exactly once, so revisions form a chain.
CREATE UNIQUE INDEX proposals_single_successor
ON proposals(supersedes_proposal_id)
WHERE supersedes_proposal_id IS NOT NULL;

CREATE TABLE proposal_responses (
    id TEXT PRIMARY KEY,
    proposal_id TEXT NOT NULL REFERENCES proposals(id),
    agent_id TEXT NOT NULL REFERENCES agents(id),
    response_type TEXT NOT NULL CHECK (
        response_type IN ('support', 'challenge', 'amend', 'reject')
    ),
    reason TEXT,
    evidence_json TEXT NOT NULL DEFAULT '[]',
    created_at TEXT NOT NULL
);

CREATE INDEX proposal_responses_proposal ON proposal_responses(proposal_id);
