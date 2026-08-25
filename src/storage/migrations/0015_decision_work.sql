-- Which work items a decision generated. The link makes the conversion
-- auditable and lets a retry recognise work it already created.
CREATE TABLE decision_work_items (
    decision_id TEXT NOT NULL REFERENCES decisions(id),
    work_id TEXT NOT NULL REFERENCES work_items(id),
    created_at TEXT NOT NULL,
    PRIMARY KEY (decision_id, work_id)
);

CREATE INDEX decision_work_items_work ON decision_work_items(work_id);
