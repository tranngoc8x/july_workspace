UPDATE work_results AS current
SET supersedes_result_id = NULL
WHERE current.supersedes_result_id IS NOT NULL
  AND NOT EXISTS (
      SELECT 1
      FROM work_results AS previous
      WHERE previous.id = current.supersedes_result_id
        AND previous.id <> current.id
        AND previous.work_id = current.work_id
  );

UPDATE publishes AS publish
SET source_conversation_id = (
    SELECT work.conversation_id
    FROM work_results AS result
    JOIN work_items AS work ON work.id = result.work_id
    WHERE result.id = publish.result_id
)
WHERE publish.source_conversation_id <> (
    SELECT work.conversation_id
    FROM work_results AS result
    JOIN work_items AS work ON work.id = result.work_id
    WHERE result.id = publish.result_id
);

UPDATE work_dependencies AS dependency
SET status = 'waiting', result_id = NULL
WHERE dependency.status IN ('satisfied', 'superseded')
  AND NOT EXISTS (
      SELECT 1
      FROM work_results AS result
      JOIN work_items AS upstream ON upstream.id = dependency.upstream_work_id
      WHERE result.id = dependency.result_id
        AND result.work_id = dependency.upstream_work_id
        AND upstream.status IN ('ready', 'done')
  );

CREATE TRIGGER work_results_no_update
BEFORE UPDATE ON work_results
BEGIN
    SELECT RAISE(ABORT, 'work results are immutable');
END;

CREATE TRIGGER work_results_no_delete
BEFORE DELETE ON work_results
BEGIN
    SELECT RAISE(ABORT, 'work results are immutable');
END;

CREATE TRIGGER work_results_supersede_insert_guard
BEFORE INSERT ON work_results
WHEN NEW.supersedes_result_id IS NOT NULL
  AND NOT EXISTS (
      SELECT 1
      FROM work_results AS previous
      WHERE previous.id = NEW.supersedes_result_id
        AND previous.id <> NEW.id
        AND previous.work_id = NEW.work_id
  )
BEGIN
    SELECT RAISE(ABORT, 'superseded result must be a different result from the same work');
END;

CREATE TRIGGER publishes_source_insert_guard
BEFORE INSERT ON publishes
WHEN NOT EXISTS (
    SELECT 1
    FROM work_results AS result
    JOIN work_items AS work ON work.id = result.work_id
    WHERE result.id = NEW.result_id
      AND work.conversation_id = NEW.source_conversation_id
)
BEGIN
    SELECT RAISE(ABORT, 'publish source must match result work conversation');
END;

CREATE TRIGGER publishes_source_update_guard
BEFORE UPDATE ON publishes
WHEN NOT EXISTS (
    SELECT 1
    FROM work_results AS result
    JOIN work_items AS work ON work.id = result.work_id
    WHERE result.id = NEW.result_id
      AND work.conversation_id = NEW.source_conversation_id
)
BEGIN
    SELECT RAISE(ABORT, 'publish source must match result work conversation');
END;

DROP TRIGGER work_dependency_result_update_guard;

CREATE TRIGGER work_dependency_result_update_guard
BEFORE UPDATE OF upstream_work_id, status, result_id ON work_dependencies
WHEN NOT (
    (NEW.status IN ('waiting', 'failed') AND NEW.result_id IS NULL)
    OR (
        NEW.status IN ('satisfied', 'superseded')
        AND NEW.result_id IS NOT NULL
        AND EXISTS (
            SELECT 1
            FROM work_results AS result
            JOIN work_items AS upstream ON upstream.id = NEW.upstream_work_id
            WHERE result.id = NEW.result_id
              AND result.work_id = NEW.upstream_work_id
              AND upstream.status IN ('ready', 'done')
        )
    )
)
BEGIN
    SELECT RAISE(ABORT, 'work dependency result is not consumable from its upstream work');
END;

CREATE TRIGGER work_items_dependency_outcome_guard
BEFORE UPDATE OF status ON work_items
WHEN NEW.status NOT IN ('ready', 'done')
  AND EXISTS (
      SELECT 1
      FROM work_dependencies AS dependency
      WHERE dependency.upstream_work_id = OLD.id
        AND dependency.status IN ('satisfied', 'superseded')
  )
BEGIN
    SELECT RAISE(ABORT, 'work status cannot invalidate a consumable dependency outcome');
END;
