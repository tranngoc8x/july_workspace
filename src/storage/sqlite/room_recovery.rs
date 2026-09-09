use super::*;

pub(crate) struct RoomRecoveryContext {
    pub messages: Vec<RoomMessage>,
    pub messages_truncated: bool,
    pub work: Vec<(WorkItem, Option<String>)>,
    pub work_truncated: bool,
}

pub(super) fn replacement_binding(
    connection: &Connection,
    source: &RoomSessionBinding,
    at: &str,
) -> Result<RoomSessionBinding, StoreError> {
    let generation = source
        .generation
        .checked_add(1)
        .ok_or(StoreError::InvalidStoredValue(
            "Room binding generation overflow",
        ))?;
    let binding = RoomSessionBinding {
        id: SessionBindingId::new(),
        room_id: source.room_id,
        agent_id: source.agent_id,
        transport_type: source.transport_type.clone(),
        remote_session_id: None,
        generation,
        status: SessionBindingStatus::Disconnected,
        created_at: at.into(),
        last_used_at: at.into(),
    };
    connection.execute("INSERT INTO session_bindings(id,room_id,agent_id,transport_type,generation,status,created_at,last_used_at)
        VALUES (?1,?2,?3,?4,?5,'disconnected',?6,?6)",params![binding.id.to_string(),binding.room_id.to_string(),binding.agent_id.to_string(),binding.transport_type,i64::try_from(binding.generation).map_err(|_|StoreError::InvalidStoredValue("Room binding generation overflow"))?,at])?;
    Ok(binding)
}

impl SqliteStore {
    /// Replace only a claimed, unsent turn after ACP definitively reports SessionLost.
    pub(crate) fn replace_room_activation_binding(
        &mut self,
        message: RoomMessageId,
        agent: AgentId,
        source: SessionBindingId,
        at: &str,
    ) -> Result<RoomSessionBinding, StoreError> {
        require_work_timestamp(at)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (_, incoming) = validate_room_activation(&transaction, message, agent)?;
        let current = query_optional(&transaction,
            "SELECT id,room_id,agent_id,transport_type,remote_session_id,generation,status,created_at,last_used_at FROM session_bindings WHERE room_id=?1 AND agent_id=?2 ORDER BY generation DESC LIMIT 1",
            params![incoming.room_id.to_string(),agent.to_string()], records::room_session_binding)?.ok_or(StoreError::RoomSessionUnavailable(source))?;
        let claimed: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM room_message_activations WHERE message_id=?1 AND agent_id=?2 AND session_binding_id=?3 AND status='claimed')",
            params![message.to_string(),agent.to_string(),current.id.to_string()], |row|row.get(0))?;
        if !claimed || current.status == SessionBindingStatus::Closed {
            return Err(StoreError::RoomSessionUnavailable(source));
        }
        if current.id != source {
            // A repeated request may observe the replacement already committed for this claim.
            let previous_generation: Option<i64> = query_optional(
                &transaction,
                "SELECT generation FROM session_bindings WHERE id=?1 AND room_id=?2 AND agent_id=?3 AND status='lost'",
                params![
                    source.to_string(),
                    incoming.room_id.to_string(),
                    agent.to_string()
                ],
                |row| row.get(0).map_err(StoreError::from),
            )?;
            if previous_generation
                .and_then(|g| u64::try_from(g).ok())
                .and_then(|g| g.checked_add(1))
                == Some(current.generation)
            {
                return Ok(current);
            }
            return Err(StoreError::RoomSessionUnavailable(source));
        }
        transaction.execute(
            "UPDATE session_bindings SET status='lost',last_used_at=?2 WHERE id=?1",
            params![source.to_string(), at],
        )?;
        let replacement = replacement_binding(&transaction, &current, at)?;
        transaction.execute("UPDATE room_message_activations SET session_binding_id=?3,updated_at=?4 WHERE message_id=?1 AND agent_id=?2 AND status='claimed'",
            params![message.to_string(),agent.to_string(),replacement.id.to_string(),at])?;
        transaction.commit()?;
        Ok(replacement)
    }

    pub(crate) fn room_recovery_context(
        &mut self,
        message: RoomMessageId,
        agent: AgentId,
    ) -> Result<RoomRecoveryContext, StoreError> {
        let transaction = self.connection.transaction()?;
        let (_, incoming) = validate_room_activation(&transaction, message, agent)?;
        let mut messages = query_all(&transaction,
            "SELECT m.id,m.room_id,m.sender_type,m.sender_id,m.body,m.mentions_json,m.reply_to,m.created_at FROM room_messages m JOIN room_message_order o ON o.message_id=m.id
             WHERE m.room_id=?1 AND o.sequence<(SELECT sequence FROM room_message_order WHERE message_id=?2) ORDER BY o.sequence DESC LIMIT 51",
            params![incoming.room_id.to_string(),message.to_string()],records::room_message)?;
        let messages_truncated = messages.len() > 50;
        messages.truncate(50);
        messages.reverse();
        let mut work=query_all(&transaction,
            "SELECT w.id,w.conversation_id,w.title,w.goal,w.status,w.owner_agent_id,w.is_primary,w.created_at,w.updated_at,w.completed_at,w.room_id,b.task_id
             FROM work_items w LEFT JOIN room_a2a_task_bindings b ON b.work_id=w.id
             WHERE w.room_id=?1 AND w.status NOT IN ('done','failed','cancelled') AND (w.owner_agent_id=?2 OR b.requester_agent_id=?2)
             ORDER BY w.updated_at DESC,w.id DESC LIMIT 21",
            params![incoming.room_id.to_string(),agent.to_string()], |row|Ok((records::work_item(row)?,row.get(11)?)))?;
        let work_truncated = work.len() > 20;
        work.truncate(20);
        transaction.commit()?;
        Ok(RoomRecoveryContext {
            messages,
            messages_truncated,
            work,
            work_truncated,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::RoomWorkIntent;
    use serde_json::json;

    const NOW: &str = "2026-09-09T00:00:00Z";
    const RESTART: &str = "2026-09-09T01:00:00Z";

    fn fixture(path: &Path) -> (SqliteStore, RoomId, AgentId) {
        let mut store = SqliteStore::open(path).unwrap();
        let room = RoomId::new();
        store
            .create_room(&Room {
                id: room,
                name: "recovery".into(),
                description: None,
                status: "active".into(),
                created_at: NOW.into(),
                updated_at: NOW.into(),
            })
            .unwrap();
        let agent = AgentId::new();
        store
            .insert_agent(&Agent {
                id: agent,
                name: "owner".into(),
                project_root: "/tmp".into(),
                transport_type: "acp".into(),
                transport_config: json!({}),
                status: "active".into(),
                metadata: json!({}),
                created_at: NOW.into(),
                updated_at: NOW.into(),
            })
            .unwrap();
        store.add_room_member(room, agent, None, NOW).unwrap();
        (store, room, agent)
    }

    fn message(store: &mut SqliteStore, room: RoomId, agent: AgentId) -> RoomMessageId {
        let id = RoomMessageId::new();
        store
            .append_room_message(&RoomMessage {
                id,
                room_id: room,
                sender_type: MemberType::User,
                sender_id: "local-user".into(),
                body: "Continue".into(),
                mentions: vec![agent],
                reply_to: None,
                created_at: NOW.into(),
            })
            .unwrap();
        id
    }

    #[test]
    fn room_recovery_replacement_is_durable_idempotent_and_revalidates_membership() {
        let path =
            std::env::temp_dir().join(format!("july-room-recovery-{}.db", ulid::Ulid::generate()));
        let (mut store, room, agent) = fixture(&path);
        let trigger = message(&mut store, room, agent);
        let source = store
            .claim_room_activation(trigger, agent, NOW)
            .unwrap()
            .unwrap()
            .binding;
        store
            .attach_room_remote_session(source.id, "lost-remote", NOW)
            .unwrap();
        let replacement = store
            .replace_room_activation_binding(trigger, agent, source.id, RESTART)
            .unwrap();
        assert_eq!(replacement.generation, source.generation + 1);
        assert_eq!(replacement.remote_session_id, None);
        drop(store);
        let mut store = SqliteStore::open(&path).unwrap();
        assert_eq!(
            store
                .replace_room_activation_binding(trigger, agent, source.id, RESTART)
                .unwrap(),
            replacement
        );
        let count: i64 = store
            .connection
            .query_row("SELECT COUNT(*) FROM session_bindings", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2);
        let linked: String = store
            .connection
            .query_row(
                "SELECT session_binding_id FROM room_message_activations WHERE message_id=?1",
                [trigger.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(linked, replacement.id.to_string());
        store.remove_room_member(room, agent, RESTART).unwrap();
        assert!(
            store
                .replace_room_activation_binding(trigger, agent, source.id, RESTART)
                .is_err()
        );
    }

    #[test]
    fn room_recovery_cannot_replace_sent_or_closed_activation() {
        for closed in [false, true] {
            let (mut store, room, agent) = fixture(Path::new(":memory:"));
            let trigger = message(&mut store, room, agent);
            let binding = store
                .claim_room_activation(trigger, agent, NOW)
                .unwrap()
                .unwrap()
                .binding;
            if closed {
                store
                    .connection
                    .execute(
                        "UPDATE session_bindings SET status='closed' WHERE id=?1",
                        [binding.id.to_string()],
                    )
                    .unwrap();
            } else {
                store
                    .set_room_activation_status(trigger, agent, "sent", NOW)
                    .unwrap();
            }
            assert!(
                store
                    .replace_room_activation_binding(trigger, agent, binding.id, RESTART)
                    .is_err()
            );
            assert_eq!(
                store
                    .get_room_session_binding(room, agent)
                    .unwrap()
                    .unwrap()
                    .id,
                binding.id
            );
        }
    }

    #[test]
    fn room_recovery_restart_preserves_canonical_state_and_does_not_replay() {
        for interrupted in ["claimed", "sent", "completed"] {
            let path = std::env::temp_dir()
                .join(format!("july-room-recovery-{}.db", ulid::Ulid::generate()));
            let (mut store, room, agent) = fixture(&path);
            let acknowledged = message(&mut store, room, agent);
            let binding = store
                .claim_room_activation(acknowledged, agent, NOW)
                .unwrap()
                .unwrap()
                .binding;
            store
                .attach_room_remote_session(binding.id, "remote", NOW)
                .unwrap();
            store
                .set_room_activation_status(acknowledged, agent, "completed", NOW)
                .unwrap();
            let trigger = message(&mut store, room, agent);
            store
                .claim_room_activation(trigger, agent, NOW)
                .unwrap()
                .unwrap();
            let publication = store
                .send_agent_room_message(
                    trigger,
                    agent,
                    &SendRoomMessage {
                        targets: vec!["owner".into()],
                        body: "Task".into(),
                        reply_to: Some(trigger),
                        request_id: Some("task".into()),
                        work: Some(RoomWorkIntent::Create {
                            title: "Task".into(),
                            goal: None,
                        }),
                    },
                    NOW,
                    &AtomicBool::new(true),
                )
                .unwrap();
            let shared = store
                .get_room_message_work(publication.id)
                .unwrap()
                .unwrap();
            store
                .transition_work(shared.work.id, WorkStatus::Working, NOW)
                .unwrap();
            store
                .create_work_result(&WorkResult {
                    id: ResultId::new(),
                    work_id: shared.work.id,
                    status: "success".into(),
                    summary: "Evidence".into(),
                    outputs: vec!["output".into()],
                    evidence: vec!["check".into()],
                    supersedes_result_id: None,
                    created_at: NOW.into(),
                })
                .unwrap();
            let before = store
                .get_room_message_work(publication.id)
                .unwrap()
                .unwrap();
            store
                .set_room_activation_status(trigger, agent, interrupted, NOW)
                .unwrap();
            let cursor: String = store
                .connection
                .query_row(
                    "SELECT last_seen_message_id FROM agent_room_cursors",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            drop(store);
            let mut store = SqliteStore::open(&path).unwrap();
            store.reconcile_interrupted_runtime(RESTART).unwrap();
            store.reconcile_interrupted_runtime(RESTART).unwrap();
            let expected = if interrupted == "completed" {
                SessionBindingStatus::Disconnected
            } else {
                SessionBindingStatus::Lost
            };
            assert_eq!(
                store
                    .get_room_session_binding(room, agent)
                    .unwrap()
                    .unwrap()
                    .status,
                expected
            );
            let status: String = store
                .connection
                .query_row(
                    "SELECT status FROM room_message_activations WHERE message_id=?1",
                    [trigger.to_string()],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                status,
                if interrupted == "completed" {
                    "completed"
                } else {
                    "failed"
                }
            );
            let after = store
                .get_room_message_work(publication.id)
                .unwrap()
                .unwrap();
            assert_eq!(after.work, before.work);
            assert_eq!(after.results, before.results);
            assert_eq!(after.binding, before.binding);
            let recovered_cursor: String = store
                .connection
                .query_row(
                    "SELECT last_seen_message_id FROM agent_room_cursors",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(recovered_cursor, cursor);
            assert!(
                store
                    .claim_room_activation(trigger, agent, RESTART)
                    .unwrap()
                    .is_none()
            );
            let next = message(&mut store, room, agent);
            let claim = store
                .claim_room_activation(next, agent, RESTART)
                .unwrap()
                .unwrap();
            assert_eq!(
                claim.binding.generation,
                if interrupted == "completed" { 1 } else { 2 }
            );
        }
    }
}
