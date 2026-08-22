ALTER TABLE work_dependencies
ADD COLUMN result_id TEXT REFERENCES work_results(id);
