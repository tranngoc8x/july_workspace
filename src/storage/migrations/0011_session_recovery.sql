CREATE TABLE session_recoveries (
    session_binding_id TEXT PRIMARY KEY NOT NULL REFERENCES session_bindings(id),
    source_binding_id TEXT NOT NULL UNIQUE REFERENCES session_bindings(id),
    capsule TEXT NOT NULL CHECK (trim(capsule) <> ''),
    capsule_delivered_at TEXT CHECK (
        capsule_delivered_at IS NULL OR trim(capsule_delivered_at) <> ''
    ),
    created_at TEXT NOT NULL CHECK (trim(created_at) <> ''),
    CHECK (session_binding_id <> source_binding_id)
);

CREATE TRIGGER session_recoveries_update_guard
BEFORE UPDATE ON session_recoveries
WHEN NEW.session_binding_id <> OLD.session_binding_id
  OR NEW.source_binding_id <> OLD.source_binding_id
  OR NEW.capsule <> OLD.capsule
  OR NEW.created_at <> OLD.created_at
  OR OLD.capsule_delivered_at IS NOT NULL
  OR NEW.capsule_delivered_at IS NULL
BEGIN
    SELECT RAISE(ABORT, 'session recovery permits one delivery progress update only');
END;
