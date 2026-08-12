use super::{StoreError, records};
use crate::domain::{
    Agent, AgentId, Checkpoint, CheckpointId, Conversation, ConversationId, ConversationKind,
    ConversationMember, MemberType, Memory, MemoryId, Message, MessageId, PermissionDecision,
    PermissionOutcome, Publish, PublishId, ResultId, Room, RoomId, RoomMember, SessionBinding,
    SessionBindingId, SessionBindingStatus, WorkDependency, WorkItem, WorkItemId, WorkResult,
};
use rusqlite::{Connection, Params, Row, TransactionBehavior, params};
use std::path::Path;
use std::time::Duration;

const BUSY_TIMEOUT_MS: u64 = 5_000;
const MIGRATIONS: [Migration; 2] = [
    Migration {
        version: 1,
        sql: include_str!("migrations/0001_workspace.sql"),
    },
    Migration {
        version: 2,
        sql: include_str!("migrations/0002_session_runtime.sql"),
    },
];

pub struct SqliteStore {
    connection: Connection,
}

impl SqliteStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let mut connection = Connection::open(path)?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.busy_timeout(Duration::from_millis(BUSY_TIMEOUT_MS))?;
        apply_migrations(&mut connection, &MIGRATIONS)?;
        Ok(Self { connection })
    }

    pub fn schema_version(&self) -> Result<i64, StoreError> {
        current_schema_version(&self.connection)
    }

    pub fn insert_agent(&self, agent: &Agent) -> Result<(), StoreError> {
        insert_agent(&self.connection, agent)
    }

    pub fn get_agent(&self, id: AgentId) -> Result<Option<Agent>, StoreError> {
        query_optional(
            &self.connection,
            "SELECT id, name, project_root, transport_type, transport_config_json, status,
                    metadata_json, created_at, updated_at
             FROM agents WHERE id = ?1",
            params![id.to_string()],
            records::agent,
        )
    }

    pub fn get_agent_by_name(&self, name: &str) -> Result<Option<Agent>, StoreError> {
        query_optional(
            &self.connection,
            "SELECT id, name, project_root, transport_type, transport_config_json, status,
                    metadata_json, created_at, updated_at
             FROM agents WHERE name = ?1",
            params![name],
            records::agent,
        )
    }

    pub fn update_agent(&self, agent: &Agent) -> Result<bool, StoreError> {
        agent.validate()?;
        let transport_config = serde_json::to_string(&agent.transport_config)?;
        let metadata = serde_json::to_string(&agent.metadata)?;
        Ok(self.connection.execute(
            "UPDATE agents SET
                name = ?1, project_root = ?2, transport_type = ?3,
                transport_config_json = ?4, status = ?5, metadata_json = ?6, updated_at = ?7
             WHERE id = ?8",
            params![
                agent.name,
                agent.project_root,
                agent.transport_type,
                transport_config,
                agent.status,
                metadata,
                agent.updated_at,
                agent.id.to_string(),
            ],
        )? != 0)
    }

    pub fn insert_room(&self, room: &Room) -> Result<(), StoreError> {
        insert_room(&self.connection, room)
    }

    pub fn get_room(&self, id: RoomId) -> Result<Option<Room>, StoreError> {
        query_optional(
            &self.connection,
            "SELECT id, name, description, status, created_at, updated_at
             FROM rooms WHERE id = ?1",
            params![id.to_string()],
            records::room,
        )
    }

    pub fn insert_room_member(&self, member: &RoomMember) -> Result<(), StoreError> {
        insert_room_member(&self.connection, member)
    }

    pub fn list_room_members(&self, room_id: RoomId) -> Result<Vec<RoomMember>, StoreError> {
        query_all(
            &self.connection,
            "SELECT room_id, agent_id, role, joined_at
             FROM room_members WHERE room_id = ?1 ORDER BY joined_at, agent_id",
            params![room_id.to_string()],
            records::room_member,
        )
    }

    pub fn insert_room_with_members(
        &mut self,
        room: &Room,
        members: &[RoomMember],
    ) -> Result<(), StoreError> {
        if let Some(member) = members.iter().find(|member| member.room_id != room.id) {
            return Err(StoreError::RoomMemberParentMismatch {
                expected: room.id,
                found: member.room_id,
            });
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        insert_room(&transaction, room)?;
        for member in members {
            insert_room_member(&transaction, member)?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn insert_conversation(&self, conversation: &Conversation) -> Result<(), StoreError> {
        insert_conversation(&self.connection, conversation)
    }

    pub fn get_conversation(&self, id: ConversationId) -> Result<Option<Conversation>, StoreError> {
        query_optional(
            &self.connection,
            "SELECT id, type, room_id, title, goal, parent_conversation_id,
                    origin_conversation_id, status, created_at, updated_at
             FROM conversations WHERE id = ?1",
            params![id.to_string()],
            records::conversation,
        )
    }

    pub fn insert_conversation_member(
        &self,
        member: &ConversationMember,
    ) -> Result<(), StoreError> {
        insert_conversation_member(&self.connection, member)
    }

    pub fn list_conversation_members(
        &self,
        conversation_id: ConversationId,
    ) -> Result<Vec<ConversationMember>, StoreError> {
        query_all(
            &self.connection,
            "SELECT conversation_id, member_type, member_id, joined_at, left_at
             FROM conversation_members WHERE conversation_id = ?1
             ORDER BY joined_at, member_type, member_id",
            params![conversation_id.to_string()],
            records::conversation_member,
        )
    }

    pub fn insert_conversation_with_members(
        &mut self,
        conversation: &Conversation,
        members: &[ConversationMember],
    ) -> Result<(), StoreError> {
        if let Some(member) = members
            .iter()
            .find(|member| member.conversation_id != conversation.id)
        {
            return Err(StoreError::ConversationMemberParentMismatch {
                expected: conversation.id,
                found: member.conversation_id,
            });
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        insert_conversation(&transaction, conversation)?;
        for member in members {
            insert_conversation_member(&transaction, member)?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn get_or_create_dm(
        &mut self,
        user_id: &str,
        agent_id: AgentId,
        now: &str,
    ) -> Result<Conversation, StoreError> {
        let conversation = Conversation {
            id: ConversationId::new(),
            kind: ConversationKind::Dm,
            room_id: None,
            title: None,
            goal: None,
            parent_conversation_id: None,
            origin_conversation_id: None,
            status: "open".into(),
            created_at: now.into(),
            updated_at: now.into(),
        };
        let members = [
            ConversationMember {
                conversation_id: conversation.id,
                member_type: MemberType::User,
                member_id: user_id.into(),
                joined_at: now.into(),
                left_at: None,
            },
            ConversationMember {
                conversation_id: conversation.id,
                member_type: MemberType::Agent,
                member_id: agent_id.to_string(),
                joined_at: now.into(),
                left_at: None,
            },
        ];
        conversation.validate()?;
        for member in &members {
            member.validate()?;
        }

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = query_optional(
            &transaction,
            "SELECT c.id, c.type, c.room_id, c.title, c.goal, c.parent_conversation_id,
                    c.origin_conversation_id, c.status, c.created_at, c.updated_at
             FROM conversations c
             WHERE c.type = 'dm' AND c.status = 'open'
               AND EXISTS (
                   SELECT 1 FROM conversation_members m
                   WHERE m.conversation_id = c.id AND m.member_type = 'user'
                     AND m.member_id = ?1 AND m.left_at IS NULL
               )
               AND EXISTS (
                   SELECT 1 FROM conversation_members m
                   WHERE m.conversation_id = c.id AND m.member_type = 'agent'
                     AND m.member_id = ?2 AND m.left_at IS NULL
               )
               AND 2 = (
                   SELECT COUNT(*) FROM conversation_members m
                   WHERE m.conversation_id = c.id AND m.left_at IS NULL
               )
             ORDER BY c.created_at, c.id
             LIMIT 1",
            params![user_id, agent_id.to_string()],
            records::conversation,
        )?;
        if let Some(existing) = existing {
            transaction.commit()?;
            return Ok(existing);
        }

        insert_conversation(&transaction, &conversation)?;
        for member in &members {
            insert_conversation_member(&transaction, member)?;
        }
        transaction.commit()?;
        Ok(conversation)
    }

    pub fn insert_message(&self, message: &Message) -> Result<(), StoreError> {
        message.validate()?;
        let metadata = if message.metadata.is_null() {
            None
        } else {
            Some(serde_json::to_string(&message.metadata)?)
        };
        self.connection.execute(
            "INSERT INTO messages(
                id, conversation_id, sender_type, sender_id, body, reply_to,
                metadata_json, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                message.id.to_string(),
                message.conversation_id.to_string(),
                message.sender_type.to_string(),
                message.sender_id,
                message.body,
                message.reply_to.map(|id| id.to_string()),
                metadata,
                message.created_at,
            ],
        )?;
        Ok(())
    }

    pub fn get_message(&self, id: MessageId) -> Result<Option<Message>, StoreError> {
        query_optional(
            &self.connection,
            "SELECT id, conversation_id, sender_type, sender_id, body, reply_to,
                    metadata_json, created_at
             FROM messages WHERE id = ?1",
            params![id.to_string()],
            records::message,
        )
    }

    pub fn list_messages(
        &self,
        conversation_id: ConversationId,
    ) -> Result<Vec<Message>, StoreError> {
        query_all(
            &self.connection,
            "SELECT id, conversation_id, sender_type, sender_id, body, reply_to,
                    metadata_json, created_at
             FROM messages WHERE conversation_id = ?1 ORDER BY created_at, id",
            params![conversation_id.to_string()],
            records::message,
        )
    }

    pub fn insert_work_item(&self, work_item: &WorkItem) -> Result<(), StoreError> {
        work_item.validate()?;
        self.connection.execute(
            "INSERT INTO work_items(
                id, conversation_id, title, goal, status, owner_agent_id,
                created_at, updated_at, completed_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                work_item.id.to_string(),
                work_item.conversation_id.to_string(),
                work_item.title,
                work_item.goal,
                work_item.status.to_string(),
                work_item.owner_agent_id.map(|id| id.to_string()),
                work_item.created_at,
                work_item.updated_at,
                work_item.completed_at,
            ],
        )?;
        Ok(())
    }

    pub fn get_work_item(&self, id: WorkItemId) -> Result<Option<WorkItem>, StoreError> {
        query_optional(
            &self.connection,
            "SELECT id, conversation_id, title, goal, status, owner_agent_id,
                    created_at, updated_at, completed_at
             FROM work_items WHERE id = ?1",
            params![id.to_string()],
            records::work_item,
        )
    }

    pub fn insert_work_dependency(&self, dependency: &WorkDependency) -> Result<(), StoreError> {
        dependency.validate()?;
        self.connection.execute(
            "INSERT INTO work_dependencies(
                upstream_work_id, downstream_work_id, dependency_type, created_at
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                dependency.upstream_work_id.to_string(),
                dependency.downstream_work_id.to_string(),
                dependency.dependency_type.to_string(),
                dependency.created_at,
            ],
        )?;
        Ok(())
    }

    pub fn get_work_dependency(
        &self,
        upstream_work_id: WorkItemId,
        downstream_work_id: WorkItemId,
    ) -> Result<Option<WorkDependency>, StoreError> {
        query_optional(
            &self.connection,
            "SELECT upstream_work_id, downstream_work_id, dependency_type, created_at
             FROM work_dependencies
             WHERE upstream_work_id = ?1 AND downstream_work_id = ?2",
            params![upstream_work_id.to_string(), downstream_work_id.to_string()],
            records::work_dependency,
        )
    }

    pub fn insert_work_result(&self, result: &WorkResult) -> Result<(), StoreError> {
        result.validate()?;
        let outputs = serde_json::to_string(&result.outputs)?;
        let evidence = serde_json::to_string(&result.evidence)?;
        self.connection.execute(
            "INSERT INTO work_results(
                id, work_id, status, summary, outputs_json, evidence_json,
                supersedes_result_id, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                result.id.to_string(),
                result.work_id.to_string(),
                result.status,
                result.summary,
                outputs,
                evidence,
                result.supersedes_result_id.map(|id| id.to_string()),
                result.created_at,
            ],
        )?;
        Ok(())
    }

    pub fn get_work_result(&self, id: ResultId) -> Result<Option<WorkResult>, StoreError> {
        query_optional(
            &self.connection,
            "SELECT id, work_id, status, summary, outputs_json, evidence_json,
                    supersedes_result_id, created_at
             FROM work_results WHERE id = ?1",
            params![id.to_string()],
            records::work_result,
        )
    }

    pub fn insert_publish(&self, publish: &Publish) -> Result<(), StoreError> {
        publish.validate()?;
        self.connection.execute(
            "INSERT INTO publishes(
                id, result_id, source_conversation_id, target_conversation_id, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                publish.id.to_string(),
                publish.result_id.to_string(),
                publish.source_conversation_id.to_string(),
                publish.target_conversation_id.to_string(),
                publish.created_at,
            ],
        )?;
        Ok(())
    }

    pub fn get_publish(&self, id: PublishId) -> Result<Option<Publish>, StoreError> {
        query_optional(
            &self.connection,
            "SELECT id, result_id, source_conversation_id, target_conversation_id, created_at
             FROM publishes WHERE id = ?1",
            params![id.to_string()],
            records::publish,
        )
    }

    pub fn insert_session_binding(&self, binding: &SessionBinding) -> Result<(), StoreError> {
        binding.validate()?;
        let generation =
            i64::try_from(binding.generation).map_err(|_| StoreError::IntegerOutOfRange {
                field: "session_bindings.generation",
                value: i128::from(binding.generation),
            })?;
        self.connection.execute(
            "INSERT INTO session_bindings(
                id, conversation_id, agent_id, transport_type, remote_session_id,
                generation, status, created_at, last_used_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                binding.id.to_string(),
                binding.conversation_id.to_string(),
                binding.agent_id.to_string(),
                binding.transport_type,
                binding.remote_session_id,
                generation,
                binding.status.to_string(),
                binding.created_at,
                binding.last_used_at,
            ],
        )?;
        Ok(())
    }

    pub fn get_session_binding(
        &self,
        id: SessionBindingId,
    ) -> Result<Option<SessionBinding>, StoreError> {
        query_optional(
            &self.connection,
            "SELECT id, conversation_id, agent_id, transport_type, remote_session_id,
                    generation, status, created_at, last_used_at
             FROM session_bindings WHERE id = ?1",
            params![id.to_string()],
            records::session_binding,
        )
    }

    pub fn get_current_session_binding(
        &self,
        conversation_id: ConversationId,
        agent_id: AgentId,
    ) -> Result<Option<SessionBinding>, StoreError> {
        query_optional(
            &self.connection,
            "SELECT id, conversation_id, agent_id, transport_type, remote_session_id,
                    generation, status, created_at, last_used_at
             FROM session_bindings
             WHERE conversation_id = ?1 AND agent_id = ?2
               AND status IN ('active', 'disconnected')",
            params![conversation_id.to_string(), agent_id.to_string()],
            records::session_binding,
        )
    }

    pub fn get_latest_session_binding(
        &self,
        conversation_id: ConversationId,
        agent_id: AgentId,
    ) -> Result<Option<SessionBinding>, StoreError> {
        query_optional(
            &self.connection,
            "SELECT id, conversation_id, agent_id, transport_type, remote_session_id,
                    generation, status, created_at, last_used_at
             FROM session_bindings
             WHERE conversation_id = ?1 AND agent_id = ?2
             ORDER BY generation DESC
             LIMIT 1",
            params![conversation_id.to_string(), agent_id.to_string()],
            records::session_binding,
        )
    }

    pub fn list_current_session_bindings_for_agent(
        &self,
        agent_id: AgentId,
    ) -> Result<Vec<SessionBinding>, StoreError> {
        query_all(
            &self.connection,
            "SELECT id, conversation_id, agent_id, transport_type, remote_session_id,
                    generation, status, created_at, last_used_at
             FROM session_bindings
             WHERE agent_id = ?1 AND status IN ('active', 'disconnected')
             ORDER BY conversation_id",
            params![agent_id.to_string()],
            records::session_binding,
        )
    }

    pub fn update_session_binding_status(
        &self,
        id: SessionBindingId,
        status: SessionBindingStatus,
        last_used_at: &str,
    ) -> Result<bool, StoreError> {
        Ok(self.connection.execute(
            "UPDATE session_bindings SET status = ?1, last_used_at = ?2 WHERE id = ?3",
            params![status.to_string(), last_used_at, id.to_string()],
        )? != 0)
    }

    pub fn mark_current_bindings_disconnected(
        &self,
        agent_id: AgentId,
        last_used_at: &str,
    ) -> Result<usize, StoreError> {
        Ok(self.connection.execute(
            "UPDATE session_bindings
             SET status = 'disconnected', last_used_at = ?1
             WHERE agent_id = ?2 AND status IN ('active', 'disconnected')",
            params![last_used_at, agent_id.to_string()],
        )?)
    }

    pub fn insert_permission_decision(
        &self,
        decision: &PermissionDecision,
    ) -> Result<(), StoreError> {
        decision.validate()?;
        let options = serde_json::Value::Array(
            decision
                .options
                .iter()
                .map(|option| {
                    serde_json::json!({
                        "id": option.id,
                        "label": option.label,
                    })
                })
                .collect(),
        );
        let (outcome, selected_option_id) = match &decision.outcome {
            PermissionOutcome::Selected(option_id) => ("selected", Some(option_id.as_str())),
            PermissionOutcome::Cancelled => ("cancelled", None),
        };
        self.connection.execute(
            "INSERT INTO permission_decisions(
                id, session_binding_id, correlation_id, options_json,
                outcome, selected_option_id, decided_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                decision.id,
                decision.session_binding_id.to_string(),
                decision.correlation_id,
                options.to_string(),
                outcome,
                selected_option_id,
                decision.decided_at,
            ],
        )?;
        Ok(())
    }

    pub fn get_permission_decision(
        &self,
        id: &str,
    ) -> Result<Option<PermissionDecision>, StoreError> {
        query_optional(
            &self.connection,
            "SELECT id, session_binding_id, correlation_id, options_json,
                    outcome, selected_option_id, decided_at
             FROM permission_decisions WHERE id = ?1",
            params![id],
            records::permission_decision,
        )
    }

    pub fn insert_checkpoint(&self, checkpoint: &Checkpoint) -> Result<(), StoreError> {
        checkpoint.validate()?;
        let decisions = serde_json::to_string(&checkpoint.decisions)?;
        let open_items = serde_json::to_string(&checkpoint.open_items)?;
        let references = serde_json::to_string(&checkpoint.references)?;
        self.connection.execute(
            "INSERT INTO checkpoints(
                id, conversation_id, agent_id, goal, current_state, decisions_json,
                open_items_json, references_json, last_message_id, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                checkpoint.id.to_string(),
                checkpoint.conversation_id.to_string(),
                checkpoint.agent_id.to_string(),
                checkpoint.goal,
                checkpoint.current_state,
                decisions,
                open_items,
                references,
                checkpoint.last_message_id.map(|id| id.to_string()),
                checkpoint.created_at,
            ],
        )?;
        Ok(())
    }

    pub fn get_checkpoint(&self, id: CheckpointId) -> Result<Option<Checkpoint>, StoreError> {
        query_optional(
            &self.connection,
            "SELECT id, conversation_id, agent_id, goal, current_state, decisions_json,
                    open_items_json, references_json, last_message_id, created_at
             FROM checkpoints WHERE id = ?1",
            params![id.to_string()],
            records::checkpoint,
        )
    }

    pub fn insert_memory(&self, memory: &Memory) -> Result<(), StoreError> {
        memory.validate()?;
        let evidence = serde_json::to_string(&memory.evidence)?;
        self.connection.execute(
            "INSERT INTO memories(
                id, scope_type, scope_id, kind, content, source_conversation_id,
                evidence_json, supersedes_memory_id, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                memory.id.to_string(),
                memory.scope_type.to_string(),
                memory.scope_id,
                memory.kind.to_string(),
                memory.content,
                memory.source_conversation_id.map(|id| id.to_string()),
                evidence,
                memory.supersedes_memory_id.map(|id| id.to_string()),
                memory.created_at,
            ],
        )?;
        Ok(())
    }

    pub fn get_memory(&self, id: MemoryId) -> Result<Option<Memory>, StoreError> {
        query_optional(
            &self.connection,
            "SELECT id, scope_type, scope_id, kind, content, source_conversation_id,
                    evidence_json, supersedes_memory_id, created_at
             FROM memories WHERE id = ?1",
            params![id.to_string()],
            records::memory,
        )
    }
}

fn insert_agent(connection: &Connection, agent: &Agent) -> Result<(), StoreError> {
    agent.validate()?;
    let transport_config = serde_json::to_string(&agent.transport_config)?;
    let metadata = serde_json::to_string(&agent.metadata)?;
    connection.execute(
        "INSERT INTO agents(
            id, name, project_root, transport_type, transport_config_json,
            status, metadata_json, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            agent.id.to_string(),
            agent.name,
            agent.project_root,
            agent.transport_type,
            transport_config,
            agent.status,
            metadata,
            agent.created_at,
            agent.updated_at,
        ],
    )?;
    Ok(())
}

fn insert_room(connection: &Connection, room: &Room) -> Result<(), StoreError> {
    room.validate()?;
    connection.execute(
        "INSERT INTO rooms(id, name, description, status, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            room.id.to_string(),
            room.name,
            room.description,
            room.status,
            room.created_at,
            room.updated_at,
        ],
    )?;
    Ok(())
}

fn insert_room_member(connection: &Connection, member: &RoomMember) -> Result<(), StoreError> {
    member.validate()?;
    connection.execute(
        "INSERT INTO room_members(room_id, agent_id, role, joined_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            member.room_id.to_string(),
            member.agent_id.to_string(),
            member.role,
            member.joined_at,
        ],
    )?;
    Ok(())
}

fn insert_conversation(
    connection: &Connection,
    conversation: &Conversation,
) -> Result<(), StoreError> {
    conversation.validate()?;
    connection.execute(
        "INSERT INTO conversations(
            id, type, room_id, title, goal, parent_conversation_id,
            origin_conversation_id, status, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            conversation.id.to_string(),
            conversation.kind.to_string(),
            conversation.room_id.map(|id| id.to_string()),
            conversation.title,
            conversation.goal,
            conversation.parent_conversation_id.map(|id| id.to_string()),
            conversation.origin_conversation_id.map(|id| id.to_string()),
            conversation.status,
            conversation.created_at,
            conversation.updated_at,
        ],
    )?;
    Ok(())
}

fn insert_conversation_member(
    connection: &Connection,
    member: &ConversationMember,
) -> Result<(), StoreError> {
    member.validate()?;
    connection.execute(
        "INSERT INTO conversation_members(
            conversation_id, member_type, member_id, joined_at, left_at
         ) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            member.conversation_id.to_string(),
            member.member_type.to_string(),
            member.member_id,
            member.joined_at,
            member.left_at,
        ],
    )?;
    Ok(())
}

fn query_optional<P, T>(
    connection: &Connection,
    sql: &str,
    parameters: P,
    map: fn(&Row<'_>) -> Result<T, StoreError>,
) -> Result<Option<T>, StoreError>
where
    P: Params,
{
    let mut statement = connection.prepare(sql)?;
    let mut rows = statement.query(parameters)?;
    rows.next()?.map(map).transpose()
}

fn query_all<P, T>(
    connection: &Connection,
    sql: &str,
    parameters: P,
    map: fn(&Row<'_>) -> Result<T, StoreError>,
) -> Result<Vec<T>, StoreError>
where
    P: Params,
{
    let mut statement = connection.prepare(sql)?;
    let mut rows = statement.query(parameters)?;
    let mut records = Vec::new();
    while let Some(row) = rows.next()? {
        records.push(map(row)?);
    }
    Ok(records)
}

#[derive(Clone, Copy)]
struct Migration {
    version: i64,
    sql: &'static str,
}

fn apply_migrations(
    connection: &mut Connection,
    migrations: &[Migration],
) -> Result<(), StoreError> {
    let current = current_schema_version(connection)?;
    let supported = migrations.last().map_or(0, |migration| migration.version);
    if current > supported {
        return Err(StoreError::DatabaseTooNew {
            found: current,
            supported,
        });
    }

    for migration in migrations
        .iter()
        .filter(|migration| migration.version > current)
    {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(migration.sql)?;
        transaction.execute(
            "INSERT INTO schema_migrations(version) VALUES (?1)",
            params![migration.version],
        )?;
        transaction.commit()?;
    }

    Ok(())
}

fn current_schema_version(connection: &Connection) -> Result<i64, StoreError> {
    let has_migrations: bool = connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM sqlite_schema
            WHERE type = 'table' AND name = 'schema_migrations'
        )",
        [],
        |row| row.get(0),
    )?;
    if !has_migrations {
        return Ok(0);
    }

    Ok(connection.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?)
}

#[cfg(test)]
mod tests {
    use super::{MIGRATIONS, Migration, SqliteStore, apply_migrations};
    use crate::storage::StoreError;
    use rusqlite::{Connection, params};
    use std::env;
    use std::fs;
    use std::path::{Path, PathBuf};
    use ulid::Ulid;

    struct TestDatabase {
        directory: PathBuf,
        path: PathBuf,
    }

    impl TestDatabase {
        fn new() -> Self {
            let directory =
                env::temp_dir().join(format!("july-workspace-storage-test-{}", Ulid::generate()));
            fs::create_dir(&directory).expect("create test database directory");
            let path = directory.join("workspace.db");
            assert!(!path.starts_with(env!("CARGO_MANIFEST_DIR")));
            Self { directory, path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestDatabase {
        fn drop(&mut self) {
            if let Err(error) = fs::remove_dir_all(&self.directory)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                eprintln!("failed to clean up {}: {error}", self.directory.display());
            }
        }
    }

    fn seed_conversation(connection: &Connection) {
        connection
            .execute(
                "INSERT INTO conversations(id, type, status, created_at, updated_at)
                 VALUES ('conversation-1', 'dm', 'open', '2026-08-09T00:00:00Z', '2026-08-09T00:00:00Z')",
                [],
            )
            .unwrap();
    }

    fn fts_count(connection: &Connection, table: &str, query: &str) -> i64 {
        connection
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE {table} MATCH ?1"),
                params![query],
                |row| row.get(0),
            )
            .unwrap()
    }

    #[test]
    fn fresh_database_has_schema_version_two() {
        let database = TestDatabase::new();
        let store = SqliteStore::open(database.path()).expect("open fresh database");

        assert_eq!(store.schema_version().unwrap(), 2);
    }

    #[test]
    fn fresh_database_contains_canonical_and_search_tables() {
        let database = TestDatabase::new();
        let store = SqliteStore::open(database.path()).expect("open fresh database");
        let expected = [
            "agents",
            "rooms",
            "room_members",
            "conversations",
            "conversation_members",
            "messages",
            "work_items",
            "work_dependencies",
            "work_results",
            "publishes",
            "session_bindings",
            "permission_decisions",
            "checkpoints",
            "memories",
            "messages_fts",
            "work_results_fts",
            "memories_fts",
        ];

        for table in expected {
            let count: i64 = store
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name = ?1",
                    params![table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "missing table {table}");
        }
    }

    #[test]
    fn all_foreign_keys_use_no_action_deletes() {
        let database = TestDatabase::new();
        let store = SqliteStore::open(database.path()).expect("open fresh database");
        let tables = [
            "room_members",
            "conversations",
            "conversation_members",
            "messages",
            "work_items",
            "work_dependencies",
            "work_results",
            "publishes",
            "session_bindings",
            "checkpoints",
            "memories",
            "permission_decisions",
        ];
        let mut foreign_key_count = 0;

        for table in tables {
            let mut statement = store
                .connection
                .prepare(&format!("PRAGMA foreign_key_list({table})"))
                .unwrap();
            let actions = statement
                .query_map([], |row| row.get::<_, String>(6))
                .unwrap();
            for action in actions {
                assert_eq!(
                    action.unwrap(),
                    "NO ACTION",
                    "unexpected delete for {table}"
                );
                foreign_key_count += 1;
            }
        }

        assert_eq!(foreign_key_count, 25);
    }

    #[test]
    fn message_metadata_accepts_null_and_rejects_malformed_json() {
        let database = TestDatabase::new();
        let store = SqliteStore::open(database.path()).expect("open fresh database");
        seed_conversation(&store.connection);

        store
            .connection
            .execute(
                "INSERT INTO messages(
                    id, conversation_id, sender_type, sender_id, body, metadata_json, created_at
                 ) VALUES (
                    'message-null', 'conversation-1', 'user', 'tony', 'hello', NULL,
                    '2026-08-09T00:00:00Z'
                 )",
                [],
            )
            .unwrap();
        let is_null: bool = store
            .connection
            .query_row(
                "SELECT metadata_json IS NULL FROM messages WHERE id = 'message-null'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(is_null);

        let malformed = store.connection.execute(
            "INSERT INTO messages(
                id, conversation_id, sender_type, sender_id, body, metadata_json, created_at
             ) VALUES (
                'message-invalid', 'conversation-1', 'user', 'tony', 'hello', '{',
                '2026-08-09T00:00:00Z'
             )",
            [],
        );
        assert!(malformed.is_err());
    }

    #[test]
    fn required_indexes_exist() {
        let database = TestDatabase::new();
        let store = SqliteStore::open(database.path()).expect("open fresh database");
        let expected = [
            "idx_messages_conversation_created",
            "idx_work_conversation",
            "idx_session_binding_lookup",
            "idx_memory_scope",
            "idx_session_binding_generation",
            "uq_session_bindings_current",
        ];

        for index in expected {
            let count: i64 = store
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'index' AND name = ?1",
                    params![index],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "missing index {index}");
        }
    }

    #[test]
    fn message_fts_tracks_insert_update_and_delete() {
        let database = TestDatabase::new();
        let store = SqliteStore::open(database.path()).expect("open fresh database");
        seed_conversation(&store.connection);

        store
            .connection
            .execute(
                "INSERT INTO messages(
                    id, conversation_id, sender_type, sender_id, body, created_at
                 ) VALUES (
                    'message-1', 'conversation-1', 'user', 'tony', 'initialword',
                    '2026-08-09T00:00:00Z'
                 )",
                [],
            )
            .unwrap();
        assert_eq!(
            fts_count(&store.connection, "messages_fts", "initialword"),
            1
        );

        store
            .connection
            .execute(
                "UPDATE messages SET body = 'revisedword' WHERE id = 'message-1'",
                [],
            )
            .unwrap();
        assert_eq!(
            fts_count(&store.connection, "messages_fts", "initialword"),
            0
        );
        assert_eq!(
            fts_count(&store.connection, "messages_fts", "revisedword"),
            1
        );

        store
            .connection
            .execute("DELETE FROM messages WHERE id = 'message-1'", [])
            .unwrap();
        assert_eq!(
            fts_count(&store.connection, "messages_fts", "revisedword"),
            0
        );
    }

    #[test]
    fn work_result_fts_tracks_insert_update_and_delete() {
        let database = TestDatabase::new();
        let store = SqliteStore::open(database.path()).expect("open fresh database");
        seed_conversation(&store.connection);
        store
            .connection
            .execute(
                "INSERT INTO work_items(
                    id, conversation_id, title, status, created_at, updated_at
                 ) VALUES (
                    'work-1', 'conversation-1', 'test search', 'open',
                    '2026-08-09T00:00:00Z', '2026-08-09T00:00:00Z'
                 )",
                [],
            )
            .unwrap();

        store
            .connection
            .execute(
                "INSERT INTO work_results(
                    id, work_id, status, summary, outputs_json, evidence_json, created_at
                 ) VALUES (
                    'result-1', 'work-1', 'accepted', 'initialsummary',
                    '[\"initialoutput\"]', '[\"initialevidence\"]',
                    '2026-08-09T00:00:00Z'
                 )",
                [],
            )
            .unwrap();
        assert_eq!(
            fts_count(
                &store.connection,
                "work_results_fts",
                "initialsummary initialoutput initialevidence"
            ),
            1
        );

        store
            .connection
            .execute(
                "UPDATE work_results SET
                    summary = 'revisedsummary',
                    outputs_json = '[\"revisedoutput\"]',
                    evidence_json = '[\"revisedevidence\"]'
                 WHERE id = 'result-1'",
                [],
            )
            .unwrap();
        assert_eq!(
            fts_count(&store.connection, "work_results_fts", "initialsummary"),
            0
        );
        assert_eq!(
            fts_count(
                &store.connection,
                "work_results_fts",
                "revisedsummary revisedoutput revisedevidence"
            ),
            1
        );

        store
            .connection
            .execute("DELETE FROM work_results WHERE id = 'result-1'", [])
            .unwrap();
        assert_eq!(
            fts_count(&store.connection, "work_results_fts", "revisedsummary"),
            0
        );
    }

    #[test]
    fn memory_fts_tracks_insert_update_and_delete() {
        let database = TestDatabase::new();
        let store = SqliteStore::open(database.path()).expect("open fresh database");

        store
            .connection
            .execute(
                "INSERT INTO memories(
                    id, scope_type, scope_id, kind, content, evidence_json, created_at
                 ) VALUES (
                    'memory-1', 'project', 'july', 'fact', 'initialmemory',
                    '[\"initialproof\"]', '2026-08-09T00:00:00Z'
                 )",
                [],
            )
            .unwrap();
        assert_eq!(
            fts_count(
                &store.connection,
                "memories_fts",
                "initialmemory initialproof"
            ),
            1
        );

        store
            .connection
            .execute(
                "UPDATE memories SET
                    content = 'revisedmemory', evidence_json = '[\"revisedproof\"]'
                 WHERE id = 'memory-1'",
                [],
            )
            .unwrap();
        assert_eq!(
            fts_count(&store.connection, "memories_fts", "initialmemory"),
            0
        );
        assert_eq!(
            fts_count(
                &store.connection,
                "memories_fts",
                "revisedmemory revisedproof"
            ),
            1
        );

        store
            .connection
            .execute("DELETE FROM memories WHERE id = 'memory-1'", [])
            .unwrap();
        assert_eq!(
            fts_count(&store.connection, "memories_fts", "revisedmemory"),
            0
        );
    }

    #[test]
    fn file_connections_use_required_sqlite_settings() {
        let database = TestDatabase::new();
        let store = SqliteStore::open(database.path()).expect("open fresh database");

        let foreign_keys: i64 = store
            .connection
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        let journal_mode: String = store
            .connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        let busy_timeout_ms: u32 = store
            .connection
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .unwrap();

        assert_eq!(foreign_keys, 1);
        assert_eq!(journal_mode, "wal");
        assert_eq!(busy_timeout_ms, 5_000);
    }

    #[test]
    fn reopening_database_is_idempotent() {
        let database = TestDatabase::new();
        assert_eq!(
            SqliteStore::open(database.path())
                .unwrap()
                .schema_version()
                .unwrap(),
            2
        );
        assert_eq!(
            SqliteStore::open(database.path())
                .unwrap()
                .schema_version()
                .unwrap(),
            2
        );
    }

    #[test]
    fn failed_migration_leaves_no_partial_user_table() {
        let database = TestDatabase::new();
        let mut connection = Connection::open(database.path()).unwrap();
        let migrations = [Migration {
            version: 1,
            sql: "
                CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY);
                CREATE TABLE partial_user_table (id TEXT PRIMARY KEY);
                THIS IS NOT VALID SQL;
            ",
        }];

        assert!(apply_migrations(&mut connection, &migrations).is_err());
        let count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE name = 'partial_user_table'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn database_newer_than_supported_is_rejected() {
        let database = TestDatabase::new();
        let connection = Connection::open(database.path()).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY);
                 INSERT INTO schema_migrations(version) VALUES (3);",
            )
            .unwrap();
        drop(connection);

        match SqliteStore::open(database.path()) {
            Err(StoreError::DatabaseTooNew {
                found: 3,
                supported: 2,
            }) => {}
            Err(error) => panic!("unexpected error: {error}"),
            Ok(_) => panic!("newer database was accepted"),
        }
    }

    fn seed_session_parent_rows(connection: &Connection) {
        connection
            .execute(
                "INSERT INTO agents(
                    id, name, project_root, transport_type, transport_config_json,
                    status, metadata_json, created_at, updated_at
                 ) VALUES (
                    'agent-1', 'agent-one', '/workspace', 'acp', '{}',
                    'active', '{}', '2026-08-09T00:00:00Z', '2026-08-09T00:00:00Z'
                 )",
                [],
            )
            .unwrap();
        seed_conversation(connection);
    }

    fn insert_raw_binding(connection: &Connection, id: &str, generation: i64, status: &str) {
        connection
            .execute(
                "INSERT INTO session_bindings(
                    id, conversation_id, agent_id, transport_type, generation,
                    status, created_at, last_used_at
                 ) VALUES (
                    ?1, 'conversation-1', 'agent-1', 'acp', ?2,
                    ?3, '2026-08-09T00:00:00Z', '2026-08-09T00:00:00Z'
                 )",
                params![id, generation, status],
            )
            .unwrap();
    }

    #[test]
    fn migration_two_rejects_unknown_v1_status_without_partial_changes() {
        let database = TestDatabase::new();
        let mut connection = Connection::open(database.path()).unwrap();
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .unwrap();
        apply_migrations(&mut connection, &MIGRATIONS[..1]).unwrap();
        seed_session_parent_rows(&connection);
        insert_raw_binding(&connection, "binding-1", 1, "legacy");

        assert!(apply_migrations(&mut connection, &MIGRATIONS).is_err());
        assert_eq!(super::current_schema_version(&connection).unwrap(), 1);
        assert_eq!(
            connection
                .query_row(
                    "SELECT status FROM session_bindings WHERE id = 'binding-1'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "legacy"
        );
        for index in [
            "idx_session_binding_lookup",
            "idx_session_binding_generation",
        ] {
            let count: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'index' AND name = ?1",
                    [index],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "migration lost v1 index {index}");
        }
    }

    #[test]
    fn migration_two_rejects_duplicate_current_generations_atomically() {
        let database = TestDatabase::new();
        let mut connection = Connection::open(database.path()).unwrap();
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .unwrap();
        apply_migrations(&mut connection, &MIGRATIONS[..1]).unwrap();
        seed_session_parent_rows(&connection);
        insert_raw_binding(&connection, "binding-1", 1, "active");
        insert_raw_binding(&connection, "binding-2", 2, "disconnected");

        assert!(apply_migrations(&mut connection, &MIGRATIONS).is_err());
        assert_eq!(super::current_schema_version(&connection).unwrap(), 1);
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM session_bindings", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            2
        );
    }

    #[test]
    fn session_status_and_current_binding_constraints_are_enforced() {
        let database = TestDatabase::new();
        let store = SqliteStore::open(database.path()).unwrap();
        seed_session_parent_rows(&store.connection);
        insert_raw_binding(&store.connection, "binding-1", 1, "active");

        assert!(
            store
                .connection
                .execute(
                    "INSERT INTO session_bindings(
                        id, conversation_id, agent_id, transport_type, generation,
                        status, created_at, last_used_at
                     ) VALUES (
                        'binding-2', 'conversation-1', 'agent-1', 'acp', 2,
                        'disconnected', 'now', 'now'
                     )",
                    [],
                )
                .is_err()
        );
        store
            .connection
            .execute(
                "UPDATE session_bindings SET status = 'lost' WHERE id = 'binding-1'",
                [],
            )
            .unwrap();
        insert_raw_binding(&store.connection, "binding-2", 2, "disconnected");
        insert_raw_binding(&store.connection, "binding-3", 3, "closed");
        assert!(
            store
                .connection
                .execute(
                    "UPDATE session_bindings SET status = 'unknown' WHERE id = 'binding-3'",
                    [],
                )
                .is_err()
        );
    }

    #[test]
    fn permission_decisions_are_validated_and_append_only() {
        let database = TestDatabase::new();
        let store = SqliteStore::open(database.path()).unwrap();
        seed_session_parent_rows(&store.connection);
        insert_raw_binding(&store.connection, "binding-1", 1, "active");
        store
            .connection
            .execute(
                "INSERT INTO permission_decisions(
                    id, session_binding_id, correlation_id, options_json,
                    outcome, selected_option_id, decided_at
                 ) VALUES (
                    'decision-1', 'binding-1', 'request-1',
                    '[{\"id\":\"allow-once\",\"label\":\"Allow once\"}]',
                    'selected', 'allow-once', '2026-08-09T00:00:00Z'
                 )",
                [],
            )
            .unwrap();

        assert!(
            store
                .connection
                .execute(
                    "UPDATE permission_decisions SET outcome = 'cancelled' WHERE id = 'decision-1'",
                    [],
                )
                .is_err()
        );
        assert!(
            store
                .connection
                .execute(
                    "DELETE FROM permission_decisions WHERE id = 'decision-1'",
                    []
                )
                .is_err()
        );
        for (id, options, selected) in [
            ("malformed", "[\"allow-once\"]", "allow-once"),
            ("missing-label", "[{\"id\":\"allow-once\"}]", "allow-once"),
            (
                "unadvertised",
                "[{\"id\":\"reject-once\",\"label\":\"Reject\"}]",
                "allow-once",
            ),
        ] {
            assert!(
                store
                    .connection
                    .execute(
                        "INSERT INTO permission_decisions(
                            id, session_binding_id, correlation_id, options_json,
                            outcome, selected_option_id, decided_at
                         ) VALUES (?1, 'binding-1', ?1, ?2, 'selected', ?3, 'now')",
                        params![id, options, selected],
                    )
                    .is_err(),
                "invalid permission decision {id} was accepted"
            );
        }
    }
}
