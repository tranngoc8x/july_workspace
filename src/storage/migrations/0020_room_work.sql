CREATE TABLE room_a2a_task_bindings (
    work_id TEXT PRIMARY KEY REFERENCES work_items(id),
    task_id TEXT NOT NULL UNIQUE CHECK(trim(task_id) <> ''),
    room_id TEXT NOT NULL REFERENCES rooms(id),
    requester_agent_id TEXT NOT NULL REFERENCES agents(id),
    owner_agent_id TEXT NOT NULL REFERENCES agents(id)
);
CREATE TABLE room_message_work (
    message_id TEXT PRIMARY KEY REFERENCES room_messages(id),
    work_id TEXT NOT NULL REFERENCES room_a2a_task_bindings(work_id),
    intent_json TEXT NOT NULL CHECK(json_valid(intent_json))
);
CREATE TRIGGER room_task_binding_scope BEFORE INSERT ON room_a2a_task_bindings
WHEN NOT EXISTS (SELECT 1 FROM work_items w WHERE w.id=NEW.work_id
    AND w.room_id=NEW.room_id AND w.owner_agent_id=NEW.owner_agent_id)
BEGIN SELECT RAISE(ABORT, 'Room task requires matching Work scope and owner'); END;
CREATE TRIGGER room_task_binding_no_update BEFORE UPDATE ON room_a2a_task_bindings
BEGIN SELECT RAISE(ABORT, 'Room task bindings are immutable'); END;
CREATE TRIGGER room_task_binding_no_delete BEFORE DELETE ON room_a2a_task_bindings
BEGIN SELECT RAISE(ABORT, 'Room task bindings are immutable'); END;
CREATE TRIGGER room_task_work_scope BEFORE UPDATE OF room_id, conversation_id, owner_agent_id ON work_items
WHEN EXISTS (SELECT 1 FROM room_a2a_task_bindings b WHERE b.work_id=OLD.id
    AND (NEW.room_id IS NOT b.room_id OR NEW.conversation_id IS NOT NULL OR NEW.owner_agent_id IS NOT b.owner_agent_id))
BEGIN SELECT RAISE(ABORT, 'Bound Room task scope and owner are immutable'); END;
CREATE TRIGGER room_message_work_no_update BEFORE UPDATE ON room_message_work
BEGIN SELECT RAISE(ABORT, 'Room work links are immutable'); END;
CREATE TRIGGER room_message_work_no_delete BEFORE DELETE ON room_message_work
BEGIN SELECT RAISE(ABORT, 'Room work links are immutable'); END;
