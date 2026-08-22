ALTER TABLE work_dependencies
ADD COLUMN status TEXT NOT NULL DEFAULT 'waiting'
CHECK (status IN ('waiting', 'satisfied', 'failed', 'superseded'));
