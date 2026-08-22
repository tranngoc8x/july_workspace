UPDATE work_dependencies
SET status = 'waiting', result_id = NULL
WHERE NOT (
    (status IN ('waiting', 'failed') AND result_id IS NULL)
    OR (
        status IN ('satisfied', 'superseded')
        AND result_id IS NOT NULL
        AND EXISTS (
            SELECT 1
            FROM work_results
            WHERE work_results.id = work_dependencies.result_id
              AND work_results.work_id = work_dependencies.upstream_work_id
        )
    )
);

CREATE TRIGGER work_dependency_result_insert_guard
BEFORE INSERT ON work_dependencies
WHEN NEW.status <> 'waiting' OR NEW.result_id IS NOT NULL
BEGIN
    SELECT RAISE(ABORT, 'new work dependency must be waiting without a result');
END;

CREATE TRIGGER work_dependency_result_update_guard
BEFORE UPDATE OF upstream_work_id, status, result_id ON work_dependencies
WHEN NOT (
    (NEW.status IN ('waiting', 'failed') AND NEW.result_id IS NULL)
    OR (
        NEW.status IN ('satisfied', 'superseded')
        AND NEW.result_id IS NOT NULL
        AND EXISTS (
            SELECT 1
            FROM work_results
            WHERE work_results.id = NEW.result_id
              AND work_results.work_id = NEW.upstream_work_id
        )
    )
)
BEGIN
    SELECT RAISE(ABORT, 'work dependency result does not match status or upstream work');
END;

CREATE TRIGGER work_dependency_result_work_guard
BEFORE UPDATE OF work_id ON work_results
WHEN EXISTS (
    SELECT 1
    FROM work_dependencies
    WHERE work_dependencies.result_id = OLD.id
      AND work_dependencies.upstream_work_id <> NEW.work_id
)
BEGIN
    SELECT RAISE(ABORT, 'work result is referenced by a dependency for another upstream work');
END;
