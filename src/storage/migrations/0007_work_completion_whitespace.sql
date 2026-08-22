DROP TRIGGER work_items_validate_completion_insert;
DROP TRIGGER work_items_validate_completion_update;

UPDATE work_items
SET completed_at = updated_at
WHERE status IN ('done', 'failed', 'cancelled')
  AND trim(
      completed_at,
      char(
          9, 10, 11, 12, 13, 32, 133, 160, 5760,
          8192, 8193, 8194, 8195, 8196, 8197, 8198, 8199, 8200, 8201, 8202,
          8232, 8233, 8239, 8287, 12288
      )
  ) = '';

CREATE TRIGGER work_items_validate_completion_insert
BEFORE INSERT ON work_items
WHEN (
    NEW.status IN ('done', 'failed', 'cancelled')
    AND (
        NEW.completed_at IS NULL
        OR trim(
            NEW.completed_at,
            char(
                9, 10, 11, 12, 13, 32, 133, 160, 5760,
                8192, 8193, 8194, 8195, 8196, 8197, 8198, 8199, 8200, 8201, 8202,
                8232, 8233, 8239, 8287, 12288
            )
        ) = ''
    )
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
    AND (
        NEW.completed_at IS NULL
        OR trim(
            NEW.completed_at,
            char(
                9, 10, 11, 12, 13, 32, 133, 160, 5760,
                8192, 8193, 8194, 8195, 8196, 8197, 8198, 8199, 8200, 8201, 8202,
                8232, 8233, 8239, 8287, 12288
            )
        ) = ''
    )
)
OR (
    NEW.status NOT IN ('done', 'failed', 'cancelled')
    AND NEW.completed_at IS NOT NULL
)
BEGIN
    SELECT RAISE(ABORT, 'work completed_at does not match status');
END;
