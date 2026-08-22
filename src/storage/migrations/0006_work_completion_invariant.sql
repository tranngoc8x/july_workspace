UPDATE work_items
SET completed_at = updated_at
WHERE status IN ('done', 'failed', 'cancelled')
  AND (completed_at IS NULL OR trim(completed_at) = '');

UPDATE work_items
SET completed_at = NULL
WHERE status NOT IN ('done', 'failed', 'cancelled')
  AND completed_at IS NOT NULL;

CREATE TRIGGER work_items_validate_completion_insert
BEFORE INSERT ON work_items
WHEN (
    NEW.status IN ('done', 'failed', 'cancelled')
    AND (NEW.completed_at IS NULL OR trim(NEW.completed_at) = '')
)
OR (
    NEW.status NOT IN ('done', 'failed', 'cancelled')
    AND NEW.completed_at IS NOT NULL
)
BEGIN
    SELECT RAISE(ABORT, 'work completed_at does not match status');
END;

CREATE TRIGGER work_items_validate_completion_update
BEFORE UPDATE OF status, completed_at ON work_items
WHEN (
    NEW.status IN ('done', 'failed', 'cancelled')
    AND (NEW.completed_at IS NULL OR trim(NEW.completed_at) = '')
)
OR (
    NEW.status NOT IN ('done', 'failed', 'cancelled')
    AND NEW.completed_at IS NOT NULL
)
BEGIN
    SELECT RAISE(ABORT, 'work completed_at does not match status');
END;
