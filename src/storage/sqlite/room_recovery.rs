use super::*;

pub(crate) struct RoomRecoveryContext {
    pub messages: Vec<RoomMessage>,
    pub messages_truncated: bool,
    pub work: Vec<(WorkItem, Option<String>)>,
    pub work_truncated: bool,
}

/// Bounded durable snapshot; observing it never claims or retries a turn.
pub(crate) struct RoomOverview {
    pub work: Vec<RoomOverviewWork>,
    pub work_truncated: bool,
    pub interrupted: Vec<RoomInterruptedTurn>,
    pub interrupted_truncated: bool,
}

pub(crate) struct RoomOverviewWork {
    pub work: WorkItem,
    pub owner_name: Option<String>,
    pub latest_result_summary: Option<String>,
}

pub(crate) struct RoomInterruptedTurn {
    pub message_id: RoomMessageId,
    pub agent_id: AgentId,
    pub agent_name: String,
    pub status: String,
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
    pub(crate) fn room_overview(
        &self,
        room: RoomId,
        target: Option<AgentId>,
    ) -> Result<RoomOverview, StoreError> {
        let tx = self.connection.unchecked_transaction()?;
        require_active_room(&tx, room)?;
        let mut work = Vec::new();
        let mut work_truncated = false;
        // Bound each group separately so completed history cannot hide unfinished work.
        for group in [0, 1, 2] {
            let mut entries = query_all(&tx,
                "SELECT w.id,w.conversation_id,substr(w.title,1,240),substr(w.goal,1,512),w.status,w.owner_agent_id,w.is_primary,w.created_at,w.updated_at,w.completed_at,w.room_id,
                 substr(a.name,1,120),(SELECT substr(r.summary,1,512) FROM work_results r WHERE r.work_id=w.id ORDER BY r.created_at DESC,r.id DESC LIMIT 1)
                 FROM work_items w LEFT JOIN agents a ON a.id=w.owner_agent_id
                 WHERE w.room_id=?1 AND (?2 IS NULL OR w.owner_agent_id=?2)
                 AND CASE WHEN w.status='done' THEN 0 WHEN w.status IN ('failed','cancelled') THEN 1 ELSE 2 END=?3
                 ORDER BY w.updated_at DESC,w.id DESC LIMIT 21",
                params![room.to_string(),target.map(|id| id.to_string()),group],
                |row| Ok(RoomOverviewWork { work: records::work_item(row)?, owner_name: row.get(11)?, latest_result_summary: row.get(12)? }))?;
            work_truncated |= entries.len() > 20;
            entries.truncate(20);
            work.extend(entries);
        }
        let mut interrupted = query_all(&tx,
            "SELECT m.id,a.agent_id,substr(owner.name,1,120),a.status
             FROM room_message_activations a JOIN room_messages m ON m.id=a.message_id
             JOIN room_message_order o ON o.message_id=m.id JOIN agents owner ON owner.id=a.agent_id
             WHERE m.room_id=?1 AND (?2 IS NULL OR a.agent_id=?2) AND a.status IN ('failed','cancelled')
             AND NOT EXISTS (
                 SELECT 1 FROM room_message_activations newer
                 JOIN room_messages followup ON followup.id=newer.message_id
                 JOIN room_message_order next_order ON next_order.message_id=followup.id
                 WHERE newer.agent_id=a.agent_id AND newer.status='completed'
                 AND followup.room_id=m.room_id AND followup.reply_to=m.id AND next_order.sequence>o.sequence)
             ORDER BY o.sequence DESC,a.agent_id LIMIT 21",
            params![room.to_string(),target.map(|id| id.to_string())],
            |row| Ok(RoomInterruptedTurn { message_id: row.get::<_,String>(0)?.parse()?, agent_id: row.get::<_,String>(1)?.parse()?, agent_name: row.get(2)?, status: row.get(3)? }))?;
        let interrupted_truncated = interrupted.len() > 20;
        interrupted.truncate(20);
        tx.commit()?;
        Ok(RoomOverview {
            work,
            work_truncated,
            interrupted,
            interrupted_truncated,
        })
    }

    /// Start a new current session without erasing shared history or other agents' sessions.
    pub(crate) fn new_room_session(
        &mut self,
        room: RoomId,
        agent: AgentId,
        at: &str,
    ) -> Result<RoomSessionBinding, StoreError> {
        require_work_timestamp(at)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_active_room(&tx, room)?;
        let agent_record = require_active_agent_record(&tx, agent)?;
        require_active_room_membership(&tx, room, agent)?;
        let current = query_optional(&tx,
            "SELECT id,room_id,agent_id,transport_type,remote_session_id,generation,status,created_at,last_used_at
             FROM session_bindings WHERE room_id=?1 AND agent_id=?2 ORDER BY generation DESC LIMIT 1",
            params![room.to_string(), agent.to_string()], records::room_session_binding)?;
        let busy: Option<String> = query_optional(
            &tx,
            "SELECT b.id FROM session_bindings b WHERE b.room_id=?1 AND b.agent_id=?2
             AND (b.status='active' OR EXISTS(SELECT 1 FROM room_message_activations a
                 WHERE a.session_binding_id=b.id AND a.status IN ('claimed','sent'))) LIMIT 1",
            params![room.to_string(), agent.to_string()],
            |row| Ok(row.get(0)?),
        )?;
        if let Some(binding) = busy {
            return Err(StoreError::RoomSessionUnavailable(binding.parse()?));
        }
        let generation = current
            .as_ref()
            .map_or(0, |binding| binding.generation)
            .checked_add(1)
            .ok_or(StoreError::InvalidStoredValue(
                "Room binding generation overflow",
            ))?;
        let generation_sql = i64::try_from(generation)
            .map_err(|_| StoreError::InvalidStoredValue("Room binding generation overflow"))?;
        if let Some(current) = current {
            tx.execute(
                "UPDATE session_bindings SET status='closed',last_used_at=?2 WHERE id=?1",
                params![current.id.to_string(), at],
            )?;
        }
        let binding = RoomSessionBinding {
            id: SessionBindingId::new(),
            room_id: room,
            agent_id: agent,
            transport_type: agent_record.transport_type,
            remote_session_id: None,
            generation,
            status: SessionBindingStatus::Disconnected,
            created_at: at.into(),
            last_used_at: at.into(),
        };
        tx.execute("INSERT INTO session_bindings(id,room_id,agent_id,transport_type,generation,status,created_at,last_used_at)
            VALUES (?1,?2,?3,?4,?5,'disconnected',?6,?6)",
            params![binding.id.to_string(),room.to_string(),agent.to_string(),binding.transport_type,generation_sql,at])?;
        tx.commit()?;
        Ok(binding)
    }

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
                sender_id: "july".into(),
                body: "Continue".into(),
                mentions: vec![agent],
                reply_to: None,
                created_at: NOW.into(),
            })
            .unwrap();
        id
    }

    #[test]
    fn room_overview_is_scoped_bounded_read_only_and_keeps_latest_result() {
        let (mut store, room, agent) = fixture(Path::new(":memory:"));
        let foreign = RoomId::new();
        let mut other = store.get_room(room).unwrap().unwrap();
        other.id = foreign;
        other.name = "foreign".into();
        store.create_room(&other).unwrap();
        for index in 0..25 {
            let work = WorkItem {
                id: WorkItemId::new(),
                scope: WorkScope::Room(room),
                title: format!("work-{index}"),
                goal: None,
                status: WorkStatus::Open,
                owner_agent_id: None,
                is_primary: false,
                created_at: NOW.into(),
                updated_at: NOW.into(),
                completed_at: None,
            };
            store.insert_work_item(&work).unwrap();
            store.assign_work_owner(work.id, agent, NOW).unwrap();
        }
        let foreign_work = WorkItem {
            id: WorkItemId::new(),
            scope: WorkScope::Room(foreign),
            title: "foreign secret".into(),
            goal: None,
            status: WorkStatus::Open,
            owner_agent_id: None,
            is_primary: false,
            created_at: NOW.into(),
            updated_at: NOW.into(),
            completed_at: None,
        };
        store.insert_work_item(&foreign_work).unwrap();
        let before = store.connection.total_changes();
        let overview = store.room_overview(room, Some(agent)).unwrap();
        assert_eq!(store.connection.total_changes(), before);
        assert_eq!(overview.work.len(), 20);
        assert!(overview.work_truncated);
        assert!(
            overview
                .work
                .iter()
                .all(|w| w.work.scope == WorkScope::Room(room)
                    && w.owner_name.as_deref() == Some("owner"))
        );
        let work = overview.work[0].work.id;
        store.connection.execute("INSERT INTO work_results(id,work_id,status,summary,outputs_json,evidence_json,created_at) VALUES (?1,?2,'success','latest verified summary','[]','[]',?3)", params![ResultId::new().to_string(),work.to_string(),RESTART]).unwrap();
        store
            .connection
            .execute(
                "UPDATE work_items SET status='done',completed_at=?2 WHERE id=?1",
                params![work.to_string(), RESTART],
            )
            .unwrap();
        let failed = overview.work[1].work.id;
        store
            .connection
            .execute(
                "UPDATE work_items SET status='failed',completed_at=?2 WHERE id=?1",
                params![failed.to_string(), RESTART],
            )
            .unwrap();
        let after_insert = store.connection.total_changes();
        let overview = store.room_overview(room, Some(agent)).unwrap();
        assert_eq!(
            overview.work[0].latest_result_summary.as_deref(),
            Some("latest verified summary")
        );
        assert_eq!(
            overview
                .work
                .iter()
                .filter(|entry| entry.work.status == WorkStatus::Done)
                .count(),
            1
        );
        assert_eq!(
            overview
                .work
                .iter()
                .filter(|entry| entry.work.status == WorkStatus::Failed)
                .count(),
            1
        );
        assert_eq!(
            overview
                .work
                .iter()
                .filter(|entry| !entry.work.status.is_terminal())
                .count(),
            20
        );
        let ids: Vec<_> = overview.work.iter().map(|entry| entry.work.id).collect();
        store
            .connection
            .execute_batch("PRAGMA query_only=ON")
            .unwrap();
        assert_eq!(
            store
                .room_overview(room, Some(agent))
                .unwrap()
                .work
                .iter()
                .map(|entry| entry.work.id)
                .collect::<Vec<_>>(),
            ids
        );
        assert_eq!(store.connection.total_changes(), after_insert);
        assert!(after_insert > before);
        assert!(
            store
                .room_overview(room, Some(AgentId::new()))
                .unwrap()
                .work
                .is_empty()
        );
    }

    #[test]
    fn room_overview_interruption_requires_linked_success_to_be_superseded() {
        let (mut store, room, agent) = fixture(Path::new(":memory:"));
        let interrupted = message(&mut store, room, agent);
        store
            .claim_room_activation(interrupted, agent, NOW)
            .unwrap();
        store
            .set_room_activation_status(interrupted, agent, "failed", NOW)
            .unwrap();
        assert_eq!(
            store.room_overview(room, None).unwrap().interrupted.len(),
            1
        );
        let unrelated = message(&mut store, room, agent);
        store.claim_room_activation(unrelated, agent, NOW).unwrap();
        store
            .set_room_activation_status(unrelated, agent, "completed", NOW)
            .unwrap();
        assert_eq!(
            store.room_overview(room, None).unwrap().interrupted.len(),
            1
        );
        let continuation = RoomMessage {
            id: RoomMessageId::new(),
            room_id: room,
            sender_type: MemberType::User,
            sender_id: "july".into(),
            body: "new continuation".into(),
            mentions: vec![agent],
            reply_to: Some(interrupted),
            created_at: RESTART.into(),
        };
        store.append_room_message(&continuation).unwrap();
        store
            .claim_room_activation(continuation.id, agent, RESTART)
            .unwrap();
        store
            .set_room_activation_status(continuation.id, agent, "completed", RESTART)
            .unwrap();
        assert!(
            store
                .room_overview(room, None)
                .unwrap()
                .interrupted
                .is_empty()
        );
    }

    #[test]
    fn room_overview_interrupted_turns_are_bounded_and_exclude_other_rooms() {
        let (mut store, room, agent) = fixture(Path::new(":memory:"));
        let mut other = store.get_room(room).unwrap().unwrap();
        other.id = RoomId::new();
        other.name = "other".into();
        store.create_room(&other).unwrap();
        store.add_room_member(other.id, agent, None, NOW).unwrap();
        let foreign = message(&mut store, other.id, agent);
        store.claim_room_activation(foreign, agent, NOW).unwrap();
        store
            .set_room_activation_status(foreign, agent, "failed", NOW)
            .unwrap();
        let mut latest = foreign;
        for _ in 0..25 {
            latest = message(&mut store, room, agent);
            store.claim_room_activation(latest, agent, NOW).unwrap();
            store
                .set_room_activation_status(latest, agent, "failed", NOW)
                .unwrap();
        }
        let overview = store.room_overview(room, None).unwrap();
        assert_eq!(overview.interrupted.len(), 20);
        assert!(overview.interrupted_truncated);
        assert_eq!(overview.interrupted[0].message_id, latest);
        assert!(
            overview
                .interrupted
                .iter()
                .all(|turn| turn.message_id != foreign && turn.status == "failed")
        );
    }

    #[test]
    fn explicit_new_room_session_preserves_ledger_and_advances_durably() {
        let path =
            std::env::temp_dir().join(format!("july-new-room-{}.db", ulid::Ulid::generate()));
        let (mut store, room, agent) = fixture(&path);
        let trigger = message(&mut store, room, agent);
        let source = store
            .claim_room_activation(trigger, agent, NOW)
            .unwrap()
            .unwrap()
            .binding;
        assert!(store.new_room_session(room, agent, RESTART).is_err());
        store
            .set_room_activation_status(trigger, agent, "sent", NOW)
            .unwrap();
        assert!(store.new_room_session(room, agent, RESTART).is_err());
        store
            .set_room_activation_status(trigger, agent, "completed", NOW)
            .unwrap();
        let fresh = store.new_room_session(room, agent, RESTART).unwrap();
        assert_eq!(fresh.generation, 2);
        assert_eq!(fresh.remote_session_id, None);
        let old_status: String = store
            .connection
            .query_row(
                "SELECT status FROM session_bindings WHERE id=?1",
                [source.id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(old_status, "closed");
        assert_eq!(
            store.list_recent_room_messages(room, 10).unwrap().0[0].id,
            trigger
        );
        drop(store);
        let mut store = SqliteStore::open(&path).unwrap();
        assert_eq!(
            store.get_room_session_binding(room, agent).unwrap(),
            Some(fresh.clone())
        );
        let next = message(&mut store, room, agent);
        let claim = store
            .claim_room_activation(next, agent, RESTART)
            .unwrap()
            .unwrap();
        assert_eq!(claim.binding.id, fresh.id);
        assert!(!claim.recovering);
        store
            .set_room_activation_status(next, agent, "completed", RESTART)
            .unwrap();
        store.remove_room_member(room, agent, RESTART).unwrap();
        assert!(store.new_room_session(room, agent, RESTART).is_err());
        assert_eq!(
            store.get_room_session_binding(room, agent).unwrap(),
            Some(fresh)
        );
    }

    #[test]
    fn explicit_new_room_session_validates_scope_and_preserves_other_agents() {
        let (mut store, room, agent) = fixture(Path::new(":memory:"));
        let fresh = store.new_room_session(room, agent, NOW).unwrap();
        assert_eq!(fresh.generation, 1);
        let mut other = store.get_agent(agent).unwrap().unwrap();
        other.id = AgentId::new();
        other.name = "other".into();
        store.insert_agent(&other).unwrap();
        assert!(store.new_room_session(room, other.id, NOW).is_err());
        store.add_room_member(room, other.id, None, NOW).unwrap();
        let untouched = store.new_room_session(room, other.id, NOW).unwrap();
        store.new_room_session(room, agent, RESTART).unwrap();
        assert_eq!(
            store.get_room_session_binding(room, other.id).unwrap(),
            Some(untouched)
        );
        store
            .connection
            .execute(
                "UPDATE agents SET status='disabled' WHERE id=?1",
                [agent.to_string()],
            )
            .unwrap();
        assert!(store.new_room_session(room, agent, RESTART).is_err());
        store
            .connection
            .execute(
                "UPDATE rooms SET status='archived' WHERE id=?1",
                [room.to_string()],
            )
            .unwrap();
        assert!(store.new_room_session(room, other.id, RESTART).is_err());
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
