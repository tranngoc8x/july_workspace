mod room_work;
use super::{StoreError, records};
use crate::application::FailedMessageDelivery;
use crate::domain::WorkScope;
use crate::domain::{
    Agent, AgentId, Checkpoint, CheckpointId, Conversation, ConversationId, ConversationKind,
    ConversationMember, Decision, DecisionId, DecisionOutcome, DecisionOwner, DecisionStatus,
    DecisionType, DecisionWork, DeliveryStatus, Handoff, HandoffChallenge, HandoffDecision,
    HandoffId, HandoffResponse, HandoffStatus, MemberType, Memory, MemoryId, MemoryKind,
    MemoryScopeType, Message, MessageDelivery, MessageId, PermissionDecision, PermissionOutcome,
    Proposal, ProposalId, ProposalResponse, ProposalResponseId, ProposalResponseType,
    ProposalStatus, Publish, PublishId, ResultId, Room, RoomId, RoomMember, RoomMessage,
    RoomMessageId, RoomSessionBinding, SendRoomMessage, SessionBinding, SessionBindingId,
    SessionBindingStatus, SessionRecovery, WorkDependency, WorkItem, WorkItemId, WorkResult,
    WorkStatus,
};
use rusqlite::{Connection, Params, Row, TransactionBehavior, params};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const BUSY_TIMEOUT_MS: u64 = 5_000;
const HANDOFF_COLUMNS: &str = "SELECT id, thread_id, work_id, from_agent_id, to_agent_id, status,
            reason, evidence_json, owned_scope_json, rejected_scope_json,
            proposed_owner_id, round_count, decision_id, created_at, updated_at
     FROM handoffs";
const DECISION_COLUMNS: &str = "SELECT id, thread_id, decision_type, title, decision, reason,
            selected_proposal_id, alternatives_json, evidence_json, decision_owner,
            participants_json, status, supersedes_decision_id, created_at, updated_at
     FROM decisions";
const PROPOSAL_COLUMNS: &str = "SELECT id, thread_id, author_agent_id, title, problem_statement,
            approach, benefits_json, costs_json, risks_json, assumptions_json,
            evidence_json, status, supersedes_proposal_id, created_at, updated_at
     FROM proposals";
const PROPOSAL_RESPONSE_COLUMNS: &str = "SELECT id, proposal_id, agent_id, response_type, reason,
            evidence_json, created_at
     FROM proposal_responses";
const MIGRATIONS: [Migration; 20] = [
    Migration {
        version: 1,
        sql: include_str!("migrations/0001_workspace.sql"),
    },
    Migration {
        version: 2,
        sql: include_str!("migrations/0002_session_runtime.sql"),
    },
    Migration {
        version: 3,
        sql: include_str!("migrations/0003_collaboration_membership.sql"),
    },
    Migration {
        version: 4,
        sql: include_str!("migrations/0004_message_deliveries.sql"),
    },
    Migration {
        version: 5,
        sql: include_str!("migrations/0005_phase6_workflow.sql"),
    },
    Migration {
        version: 6,
        sql: include_str!("migrations/0006_work_completion_invariant.sql"),
    },
    Migration {
        version: 7,
        sql: include_str!("migrations/0007_work_completion_whitespace.sql"),
    },
    Migration {
        version: 8,
        sql: include_str!("migrations/0008_dependency_result.sql"),
    },
    Migration {
        version: 9,
        sql: include_str!("migrations/0009_dependency_result_invariant.sql"),
    },
    Migration {
        version: 10,
        sql: include_str!("migrations/0010_phase6_invariants.sql"),
    },
    Migration {
        version: 11,
        sql: include_str!("migrations/0011_session_recovery.sql"),
    },
    Migration {
        version: 12,
        sql: include_str!("migrations/0012_handoffs.sql"),
    },
    Migration {
        version: 13,
        sql: include_str!("migrations/0013_decisions.sql"),
    },
    Migration {
        version: 14,
        sql: include_str!("migrations/0014_proposals.sql"),
    },
    Migration {
        version: 15,
        sql: include_str!("migrations/0015_decision_work.sql"),
    },
    Migration {
        version: 16,
        sql: include_str!("migrations/0016_room_messages.sql"),
    },
    Migration {
        version: 17,
        sql: include_str!("migrations/0017_room_runtime.sql"),
    },
    Migration {
        version: 18,
        sql: include_str!("migrations/0018_agent_room_cursors.sql"),
    },
    Migration {
        version: 19,
        sql: include_str!("migrations/0019_room_publications.sql"),
    },
    Migration {
        version: 20,
        sql: include_str!("migrations/0020_room_work.sql"),
    },
];

pub(crate) struct RoomActivationClaim {
    pub agent: Agent,
    pub message: RoomMessage,
    pub binding: RoomSessionBinding,
    pub context: Vec<RoomMessage>,
    pub truncated: bool,
}

/// Durable SQLite access which keeps Work Result lifecycle and Publish writes guarded.
///
/// Result rows cannot be inserted without their lifecycle transaction:
///
/// ```compile_fail
/// fn bypass(
///     store: &july_workspace::storage::SqliteStore,
///     result: &july_workspace::domain::WorkResult,
/// ) {
///     store.insert_work_result(result).unwrap();
/// }
/// ```
///
/// Publish source cannot be supplied through a raw insert:
///
/// ```compile_fail
/// fn bypass(
///     store: &july_workspace::storage::SqliteStore,
///     publish: &july_workspace::domain::Publish,
/// ) {
///     store.insert_publish(publish).unwrap();
/// }
/// ```
///
/// Dependency status and Result references cannot be supplied through a raw insert:
///
/// ```compile_fail
/// fn bypass(
///     store: &july_workspace::storage::SqliteStore,
///     dependency: &july_workspace::domain::WorkDependency,
/// ) {
///     store.insert_work_dependency(dependency).unwrap();
/// }
/// ```
pub struct SqliteStore {
    connection: Connection,
}

impl SqliteStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let mut connection = Connection::open(path)?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.busy_timeout(Duration::from_millis(BUSY_TIMEOUT_MS))?;
        let obsolete_work_schema: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='work_items') AND NOT EXISTS(SELECT 1 FROM pragma_table_info('work_items') WHERE name='room_id')",
            [], |row| row.get(0))?;
        if obsolete_work_schema {
            return Err(StoreError::InvalidStoredValue(
                "pre-Phase9 Work schema; use a fresh workspace database",
            ));
        }
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

    pub fn list_agents(&self) -> Result<Vec<Agent>, StoreError> {
        query_all(
            &self.connection,
            "SELECT id, name, project_root, transport_type, transport_config_json, status,
                    metadata_json, created_at, updated_at
             FROM agents ORDER BY name, id",
            [],
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

    pub fn create_room(&mut self, room: &Room) -> Result<(), StoreError> {
        room.validate()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id_exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM rooms WHERE id = ?1)",
            params![room.id.to_string()],
            |row| row.get(0),
        )?;
        if id_exists {
            return Err(StoreError::RoomIdConflict(room.id));
        }
        let name_exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM rooms WHERE name = ?1)",
            params![room.name],
            |row| row.get(0),
        )?;
        if name_exists {
            return Err(StoreError::RoomNameConflict(room.name.clone()));
        }
        insert_room(&transaction, room)?;
        transaction.commit()?;
        Ok(())
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

    pub fn get_room_by_name(&self, name: &str) -> Result<Option<Room>, StoreError> {
        query_optional(
            &self.connection,
            "SELECT id, name, description, status, created_at, updated_at
             FROM rooms WHERE name = ?1",
            params![name],
            records::room,
        )
    }

    pub fn list_rooms(&self) -> Result<Vec<Room>, StoreError> {
        query_all(
            &self.connection,
            "SELECT id, name, description, status, created_at, updated_at
             FROM rooms ORDER BY name, id",
            [],
            records::room,
        )
    }

    pub fn list_room_members(&self, room_id: RoomId) -> Result<Vec<RoomMember>, StoreError> {
        query_all(
            &self.connection,
            "SELECT room_id, agent_id, role, generation, joined_at, left_at
             FROM room_members WHERE room_id = ?1 ORDER BY generation, agent_id",
            params![room_id.to_string()],
            records::room_member,
        )
    }

    pub fn add_room_member(
        &mut self,
        room_id: RoomId,
        agent_id: AgentId,
        role: Option<&str>,
        now: &str,
    ) -> Result<bool, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let active: bool = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM room_members
                WHERE room_id = ?1 AND agent_id = ?2 AND left_at IS NULL
            )",
            params![room_id.to_string(), agent_id.to_string()],
            |row| row.get(0),
        )?;
        if active {
            transaction.commit()?;
            return Ok(false);
        }
        require_active_room(&transaction, room_id)?;
        require_active_agent(&transaction, agent_id)?;
        let generation = next_room_membership_generation(&transaction, room_id, agent_id)?;
        insert_room_member(
            &transaction,
            &RoomMember {
                room_id,
                agent_id,
                role: role.map(str::to_owned),
                generation,
                joined_at: now.into(),
                left_at: None,
            },
        )?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn remove_room_member(
        &mut self,
        room_id: RoomId,
        agent_id: AgentId,
        now: &str,
    ) -> Result<bool, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_room(&transaction, room_id)?;
        require_agent(&transaction, agent_id)?;
        let blocked: bool = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1
                FROM conversation_members member
                JOIN conversations conversation ON conversation.id = member.conversation_id
                WHERE conversation.type = 'thread' AND conversation.room_id = ?1
                  AND member.member_type = 'agent' AND member.member_id = ?2
                  AND member.left_at IS NULL
            )",
            params![room_id.to_string(), agent_id.to_string()],
            |row| row.get(0),
        )?;
        if blocked {
            return Err(StoreError::RoomRemovalBlocked { room_id, agent_id });
        }
        let changed = transaction.execute(
            "UPDATE room_members SET left_at = ?3
             WHERE room_id = ?1 AND agent_id = ?2 AND left_at IS NULL",
            params![room_id.to_string(), agent_id.to_string(), now],
        )? != 0;
        transaction.commit()?;
        Ok(changed)
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
        if room.status != "active" {
            return Err(StoreError::RoomInactive(room.id));
        }
        if members
            .iter()
            .any(|member| member.generation != 1 || member.left_at.is_some())
        {
            return Err(StoreError::MembershipTransitionRequired("room membership"));
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        insert_room(&transaction, room)?;
        for member in members {
            require_active_agent(&transaction, member.agent_id)?;
            insert_room_member(&transaction, member)?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn insert_conversation(&self, conversation: &Conversation) -> Result<(), StoreError> {
        if conversation.kind == ConversationKind::Thread {
            return Err(StoreError::ThreadAggregateRequired(conversation.id));
        }
        insert_conversation(&self.connection, conversation)
    }

    pub fn get_conversation(&self, id: ConversationId) -> Result<Option<Conversation>, StoreError> {
        get_conversation(&self.connection, id)
    }

    pub fn get_thread(&self, id: ConversationId) -> Result<Option<Conversation>, StoreError> {
        query_optional(
            &self.connection,
            "SELECT id, type, room_id, title, goal, parent_conversation_id,
                    origin_conversation_id, status, created_at, updated_at
             FROM conversations WHERE id = ?1 AND type = 'thread'",
            params![id.to_string()],
            records::conversation,
        )
    }

    pub fn list_threads(&self, room_id: RoomId) -> Result<Vec<Conversation>, StoreError> {
        query_all(
            &self.connection,
            "SELECT id, type, room_id, title, goal, parent_conversation_id,
                    origin_conversation_id, status, created_at, updated_at
             FROM conversations
             WHERE type = 'thread' AND room_id = ?1
             ORDER BY created_at, id",
            params![room_id.to_string()],
            records::conversation,
        )
    }

    pub fn list_conversation_members(
        &self,
        conversation_id: ConversationId,
    ) -> Result<Vec<ConversationMember>, StoreError> {
        query_all(
            &self.connection,
            "SELECT conversation_id, member_type, member_id, generation, joined_at, left_at
             FROM conversation_members WHERE conversation_id = ?1
             ORDER BY generation, member_type, member_id",
            params![conversation_id.to_string()],
            records::conversation_member,
        )
    }

    pub(crate) fn admit_thread_session(
        &mut self,
        thread_id: ConversationId,
        agent_id: AgentId,
        admitted_at: &str,
    ) -> Result<(Agent, Conversation, Option<SessionBinding>), StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let thread = require_open_thread(&transaction, thread_id)?;
        let room_id = thread.room_id.expect("validated thread has a room");
        require_active_room(&transaction, room_id)?;
        let agent = require_active_agent_record(&transaction, agent_id)?;
        require_active_room_membership(&transaction, room_id, agent_id)?;
        require_active_thread_membership(&transaction, thread_id, agent_id)?;
        let mut binding = query_optional(
            &transaction,
            "SELECT id, conversation_id, agent_id, transport_type, remote_session_id,
                    generation, status, created_at, last_used_at
             FROM session_bindings
             WHERE conversation_id = ?1 AND agent_id = ?2
             ORDER BY generation DESC LIMIT 1",
            params![thread_id.to_string(), agent_id.to_string()],
            records::session_binding,
        )?;
        if let Some(binding) = binding.as_mut()
            && matches!(
                binding.status,
                SessionBindingStatus::Active | SessionBindingStatus::Disconnected
            )
            && binding.remote_session_id.is_none()
            && session_recovery_by_id(&transaction, binding.id)?
                .is_none_or(|recovery| recovery.capsule_delivered_at.is_some())
        {
            transaction.execute(
                "UPDATE session_bindings SET status = 'lost', last_used_at = ?1 WHERE id = ?2",
                params![admitted_at, binding.id.to_string()],
            )?;
            binding.status = SessionBindingStatus::Lost;
            binding.last_used_at = admitted_at.into();
        }
        transaction.commit()?;
        Ok((agent, thread, binding))
    }

    pub fn add_thread_member(
        &mut self,
        conversation_id: ConversationId,
        agent_id: AgentId,
        now: &str,
    ) -> Result<bool, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let member_id = agent_id.to_string();
        let active: bool = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM conversation_members
                WHERE conversation_id = ?1 AND member_type = 'agent'
                  AND member_id = ?2 AND left_at IS NULL
            )",
            params![conversation_id.to_string(), member_id],
            |row| row.get(0),
        )?;
        if active {
            transaction.commit()?;
            return Ok(false);
        }
        let thread = require_open_thread(&transaction, conversation_id)?;
        let room_id = thread.room_id.expect("validated thread has a room");
        require_active_room(&transaction, room_id)?;
        require_active_agent(&transaction, agent_id)?;
        require_active_room_membership(&transaction, room_id, agent_id)?;
        let generation = next_thread_membership_generation(
            &transaction,
            conversation_id,
            MemberType::Agent,
            &member_id,
        )?;
        insert_conversation_member(
            &transaction,
            &ConversationMember {
                conversation_id,
                member_type: MemberType::Agent,
                member_id,
                generation,
                joined_at: now.into(),
                left_at: None,
            },
        )?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn persist_thread_mention(
        &mut self,
        message: &Message,
        source_agent_id: AgentId,
        target_agent_id: AgentId,
        capsule: &str,
    ) -> Result<Option<(bool, MessageDelivery)>, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        message.validate()?;
        let thread_id = message.conversation_id;
        let thread = require_open_thread(&transaction, thread_id)?;
        let room_id = thread.room_id.expect("validated thread has a room");
        require_active_room(&transaction, room_id)?;
        require_active_agent(&transaction, source_agent_id)?;
        require_active_agent(&transaction, target_agent_id)?;
        require_active_room_membership(&transaction, room_id, source_agent_id)?;
        require_active_room_membership(&transaction, room_id, target_agent_id)?;
        require_active_thread_membership(&transaction, thread_id, source_agent_id)?;
        if message.sender_type != MemberType::Agent
            || message.sender_id != source_agent_id.to_string()
        {
            return Err(StoreError::MessageSenderMismatch(source_agent_id));
        }
        if !insert_message(&transaction, message)? {
            transaction.commit()?;
            return Ok(None);
        }

        let target_member_id = target_agent_id.to_string();
        let active: bool = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM conversation_members
                WHERE conversation_id = ?1 AND member_type = 'agent'
                  AND member_id = ?2 AND left_at IS NULL
            )",
            params![thread_id.to_string(), target_member_id],
            |row| row.get(0),
        )?;
        if !active {
            let generation = next_thread_membership_generation(
                &transaction,
                thread_id,
                MemberType::Agent,
                &target_member_id,
            )?;
            insert_conversation_member(
                &transaction,
                &ConversationMember {
                    conversation_id: thread_id,
                    member_type: MemberType::Agent,
                    member_id: target_member_id,
                    generation,
                    joined_at: message.created_at.clone(),
                    left_at: None,
                },
            )?;
        }
        let delivery = MessageDelivery {
            message_id: message.id,
            target_agent_id,
            status: DeliveryStatus::Pending,
            capsule: (!active).then(|| capsule.to_owned()),
            capsule_delivered_at: None,
            created_at: message.created_at.clone(),
            updated_at: message.created_at.clone(),
            delivered_at: None,
        };
        delivery.validate()?;
        insert_message_delivery(&transaction, &delivery)?;
        transaction.commit()?;
        Ok(Some((!active, delivery)))
    }

    pub fn remove_thread_member(
        &mut self,
        conversation_id: ConversationId,
        agent_id: AgentId,
        now: &str,
    ) -> Result<bool, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_thread(&transaction, conversation_id)?;
        require_agent(&transaction, agent_id)?;
        let changed = transaction.execute(
            "UPDATE conversation_members SET left_at = ?3
             WHERE conversation_id = ?1 AND member_type = 'agent'
               AND member_id = ?2 AND left_at IS NULL",
            params![conversation_id.to_string(), agent_id.to_string(), now],
        )? != 0;
        transaction.commit()?;
        Ok(changed)
    }

    pub fn create_thread_with_primary_work(
        &mut self,
        thread: &Conversation,
        primary_work_id: WorkItemId,
        user_id: &str,
        initial_agents: &[AgentId],
    ) -> Result<WorkItem, StoreError> {
        thread.validate()?;
        if thread.kind != ConversationKind::Thread {
            return Err(StoreError::NotThread(thread.id));
        }
        if thread.status != "open" {
            return Err(StoreError::ThreadNotOpen(thread.id));
        }
        let room_id = thread.room_id.expect("validated thread has a room");
        let user = ConversationMember {
            conversation_id: thread.id,
            member_type: MemberType::User,
            member_id: user_id.into(),
            generation: 1,
            joined_at: thread.created_at.clone(),
            left_at: None,
        };
        user.validate()?;
        let work = WorkItem {
            id: primary_work_id,
            scope: WorkScope::Conversation(thread.id),
            title: thread.title.clone().expect("validated thread has a title"),
            goal: thread.goal.clone(),
            status: WorkStatus::Open,
            owner_agent_id: None,
            is_primary: true,
            created_at: thread.created_at.clone(),
            updated_at: thread.created_at.clone(),
            completed_at: None,
        };
        work.validate()?;
        let initial_agents: BTreeSet<_> = initial_agents.iter().copied().collect();

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let thread_exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM conversations WHERE id = ?1)",
            params![thread.id.to_string()],
            |row| row.get(0),
        )?;
        if thread_exists {
            return Err(StoreError::ThreadIdConflict(thread.id));
        }
        require_active_room(&transaction, room_id)?;
        for agent_id in &initial_agents {
            require_active_agent(&transaction, *agent_id)?;
            require_active_room_membership(&transaction, room_id, *agent_id)?;
        }

        insert_conversation(&transaction, thread)?;
        insert_conversation_member(&transaction, &user)?;
        for agent_id in initial_agents {
            insert_conversation_member(
                &transaction,
                &ConversationMember {
                    conversation_id: thread.id,
                    member_type: MemberType::Agent,
                    member_id: agent_id.to_string(),
                    generation: 1,
                    joined_at: thread.created_at.clone(),
                    left_at: None,
                },
            )?;
        }
        if let Err(error) = insert_work_item(&transaction, &work) {
            let work_exists: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM work_items WHERE id = ?1)",
                params![primary_work_id.to_string()],
                |row| row.get(0),
            )?;
            return if work_exists {
                Err(StoreError::PrimaryWorkIdConflict(primary_work_id))
            } else {
                Err(error)
            };
        }
        transaction.commit()?;
        Ok(work)
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
        if conversation.kind == ConversationKind::Thread {
            return Err(StoreError::ThreadAggregateRequired(conversation.id));
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
                generation: 1,
                joined_at: now.into(),
                left_at: None,
            },
            ConversationMember {
                conversation_id: conversation.id,
                member_type: MemberType::Agent,
                member_id: agent_id.to_string(),
                generation: 1,
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

    pub fn get_or_create_agent_dm(
        &mut self,
        source_agent_id: AgentId,
        target_agent_id: AgentId,
        now: &str,
    ) -> Result<Conversation, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let conversation =
            get_or_create_agent_dm(&transaction, source_agent_id, target_agent_id, now)?;
        transaction.commit()?;
        Ok(conversation)
    }

    pub fn persist_agent_direct_message(
        &mut self,
        message_id: MessageId,
        source_agent_id: AgentId,
        target_agent_id: AgentId,
        body: &str,
        sent_at: &str,
    ) -> Result<Option<(Message, MessageDelivery)>, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let conversation =
            get_or_create_agent_dm(&transaction, source_agent_id, target_agent_id, sent_at)?;
        let message = Message {
            id: message_id,
            conversation_id: conversation.id,
            sender_type: MemberType::Agent,
            sender_id: source_agent_id.to_string(),
            body: body.into(),
            reply_to: None,
            metadata: serde_json::json!({
                "july": {"schema": 1, "channel": "dm", "direction": "outbound"}
            }),
            created_at: sent_at.into(),
        };
        message.validate()?;
        let delivery = MessageDelivery {
            message_id,
            target_agent_id,
            status: DeliveryStatus::Pending,
            capsule: None,
            capsule_delivered_at: None,
            created_at: sent_at.into(),
            updated_at: sent_at.into(),
            delivered_at: None,
        };
        delivery.validate()?;
        if !insert_message(&transaction, &message)? {
            match get_message_delivery(&transaction, message_id, target_agent_id)? {
                Some(existing)
                    if existing.capsule.is_none() && existing.created_at == delivery.created_at =>
                {
                    transaction.commit()?;
                    return Ok(None);
                }
                _ => {
                    return Err(StoreError::DeliveryConflict {
                        message_id,
                        target_agent_id,
                    });
                }
            }
        }
        insert_message_delivery(&transaction, &delivery)?;
        transaction.commit()?;
        Ok(Some((message, delivery)))
    }

    pub fn insert_message(&self, message: &Message) -> Result<(), StoreError> {
        message.validate()?;
        insert_message(&self.connection, message).map(|_| ())
    }

    pub fn append_room_message(
        &mut self,
        message: &RoomMessage,
    ) -> Result<RoomMessage, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let message = append_room_message(&transaction, message)?;
        transaction.commit()?;
        Ok(message)
    }

    pub(crate) fn send_agent_room_message(
        &mut self,
        trigger: RoomMessageId,
        agent: AgentId,
        request: &SendRoomMessage,
        at: &str,
        publication_alive: &AtomicBool,
    ) -> Result<RoomMessage, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !publication_alive.load(Ordering::Acquire) {
            return Err(StoreError::RoomPublicationUnavailable);
        }
        let (_, incoming) = validate_room_activation(&transaction, trigger, agent)?;
        let active: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM room_message_activations a
             JOIN session_bindings b ON b.id = a.session_binding_id
             WHERE a.message_id = ?1 AND a.agent_id = ?2 AND a.status IN ('claimed', 'sent')
               AND b.agent_id = ?2 AND b.room_id = ?3 AND b.status = 'active'
               AND b.remote_session_id IS NOT NULL
               AND NOT EXISTS(SELECT 1 FROM session_bindings newer
                 WHERE newer.room_id = b.room_id AND newer.agent_id = b.agent_id AND newer.generation > b.generation))",
            params![trigger.to_string(), agent.to_string(), incoming.room_id.to_string()], |row| row.get(0))?;
        if !active {
            return Err(StoreError::RoomPublicationUnavailable);
        }
        if request
            .request_id
            .as_ref()
            .is_some_and(|id| id.trim().is_empty())
        {
            return Err(StoreError::InvalidRoomMessageRequest(
                "request id must not be blank",
            ));
        }
        let mut mentions = Vec::new();
        for name in &request.targets {
            let id = query_optional(
                &transaction,
                "SELECT id FROM agents WHERE name = ?1",
                params![name],
                |row| {
                    row.get::<_, String>(0)?
                        .parse::<AgentId>()
                        .map_err(StoreError::from)
                },
            )?
            .ok_or_else(|| StoreError::RoomTargetNotFound(name.clone()))?;
            require_active_agent(&transaction, id)?;
            require_active_room_membership(&transaction, incoming.room_id, id)?;
            if !mentions.contains(&id) {
                mentions.push(id);
            }
        }
        let existing_id = if let Some(key) = &request.request_id {
            query_optional(
                &transaction,
                "SELECT published_message_id FROM room_message_publications WHERE trigger_message_id = ?1 AND agent_id = ?2 AND request_id = ?3",
                params![trigger.to_string(), agent.to_string(), key],
                |row| row.get::<_, String>(0).map_err(StoreError::from),
            )?
        } else {
            None
        };
        let mut message = RoomMessage {
            id: RoomMessageId::new(),
            room_id: incoming.room_id,
            sender_type: MemberType::Agent,
            sender_id: agent.to_string(),
            body: request.body.clone(),
            mentions,
            reply_to: request.reply_to,
            created_at: at.into(),
        };
        if let Some(ref id) = existing_id {
            let previous = query_optional(&transaction,
                "SELECT id, room_id, sender_type, sender_id, body, mentions_json, reply_to, created_at FROM room_messages WHERE id = ?1",
                params![id], records::room_message)?.ok_or(StoreError::InvalidStoredValue("room publication message"))?;
            message.id = previous.id;
            message.created_at = previous.created_at.clone();
            if message != previous {
                return Err(StoreError::RoomPublicationConflict);
            }
        }
        let message = append_room_message(&transaction, &message)?;
        room_work::bind_publication(&transaction, &message, request, existing_id.is_some())?;
        if let Some(key) = &request.request_id {
            transaction.execute("INSERT OR IGNORE INTO room_message_publications(trigger_message_id, agent_id, request_id, published_message_id) VALUES (?1, ?2, ?3, ?4)",
                params![trigger.to_string(), agent.to_string(), key, message.id.to_string()])?;
        }
        // Recheck after validation so requests queued before revocation cannot publish afterward.
        if !publication_alive.load(Ordering::Acquire) {
            return Err(StoreError::RoomPublicationUnavailable);
        }
        transaction.commit()?;
        Ok(message)
    }

    pub fn list_recent_room_messages(
        &self,
        room_id: RoomId,
        limit: usize,
    ) -> Result<(Vec<RoomMessage>, bool), StoreError> {
        let query_limit =
            i64::try_from(limit.saturating_add(1)).map_err(|_| StoreError::IntegerOutOfRange {
                field: "room message limit",
                value: limit as i128,
            })?;
        let mut messages = query_all(
            &self.connection,
            "SELECT id, room_id, sender_type, sender_id, body, mentions_json, reply_to, created_at
             FROM room_messages WHERE room_id = ?1
             ORDER BY created_at DESC, id DESC LIMIT ?2",
            params![room_id.to_string(), query_limit],
            records::room_message,
        )?;
        let truncated = messages.len() > limit;
        messages.truncate(limit);
        messages.reverse();
        Ok((messages, truncated))
    }

    pub fn get_message(&self, id: MessageId) -> Result<Option<Message>, StoreError> {
        get_message(&self.connection, id)
    }

    pub fn insert_message_with_pending_delivery(
        &mut self,
        message: &Message,
        target_agent_id: AgentId,
        capsule: Option<&str>,
    ) -> Result<bool, StoreError> {
        message.validate()?;
        let delivery = MessageDelivery {
            message_id: message.id,
            target_agent_id,
            status: DeliveryStatus::Pending,
            capsule: capsule.map(str::to_owned),
            capsule_delivered_at: None,
            created_at: message.created_at.clone(),
            updated_at: message.created_at.clone(),
            delivered_at: None,
        };
        delivery.validate()?;

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let inserted = if insert_message(&transaction, message)? {
            insert_message_delivery(&transaction, &delivery)?;
            true
        } else {
            let existing = get_message_delivery(&transaction, message.id, target_agent_id)?;
            match existing {
                Some(existing)
                    if existing.capsule == delivery.capsule
                        && existing.created_at == delivery.created_at =>
                {
                    false
                }
                Some(_) => {
                    return Err(StoreError::DeliveryConflict {
                        message_id: message.id,
                        target_agent_id,
                    });
                }
                None => {
                    insert_message_delivery(&transaction, &delivery)?;
                    true
                }
            }
        };
        transaction.commit()?;
        Ok(inserted)
    }

    pub fn get_message_delivery(
        &self,
        message_id: MessageId,
        target_agent_id: AgentId,
    ) -> Result<Option<MessageDelivery>, StoreError> {
        get_message_delivery(&self.connection, message_id, target_agent_id)
    }

    pub fn list_failed_message_deliveries(&self) -> Result<Vec<FailedMessageDelivery>, StoreError> {
        query_all(
            &self.connection,
            "SELECT m.id, m.conversation_id, m.sender_type, m.sender_id, m.body, m.reply_to,
                    m.metadata_json, m.created_at,
                    d.message_id, d.target_agent_id, d.status, d.capsule,
                    d.capsule_delivered_at, d.created_at, d.updated_at, d.delivered_at,
                    c.type
             FROM message_deliveries d
             JOIN messages m ON m.id = d.message_id
             JOIN conversations c ON c.id = m.conversation_id
             WHERE d.status = 'failed'
             ORDER BY d.updated_at, d.message_id, d.target_agent_id",
            params![],
            failed_message_delivery,
        )
    }

    pub fn mark_delivery_capsule_delivered(
        &self,
        message_id: MessageId,
        target_agent_id: AgentId,
        delivered_at: &str,
    ) -> Result<bool, StoreError> {
        Ok(self.connection.execute(
            "UPDATE message_deliveries
             SET capsule_delivered_at = ?3, updated_at = ?3
             WHERE message_id = ?1 AND target_agent_id = ?2
               AND status = 'pending' AND capsule IS NOT NULL
               AND capsule_delivered_at IS NULL",
            params![
                message_id.to_string(),
                target_agent_id.to_string(),
                delivered_at
            ],
        )? == 1)
    }

    pub fn mark_delivery_delivered(
        &self,
        message_id: MessageId,
        target_agent_id: AgentId,
        delivered_at: &str,
    ) -> Result<bool, StoreError> {
        Ok(self.connection.execute(
            "UPDATE message_deliveries
             SET status = 'delivered', updated_at = ?3, delivered_at = ?3
             WHERE message_id = ?1 AND target_agent_id = ?2 AND status = 'pending'",
            params![
                message_id.to_string(),
                target_agent_id.to_string(),
                delivered_at
            ],
        )? == 1)
    }

    pub fn mark_delivery_failed(
        &self,
        message_id: MessageId,
        target_agent_id: AgentId,
        failed_at: &str,
    ) -> Result<bool, StoreError> {
        Ok(self.connection.execute(
            "UPDATE message_deliveries
             SET status = 'failed', updated_at = ?3
             WHERE message_id = ?1 AND target_agent_id = ?2 AND status = 'pending'",
            params![
                message_id.to_string(),
                target_agent_id.to_string(),
                failed_at
            ],
        )? == 1)
    }

    pub(crate) fn reconcile_pending_deliveries(
        &mut self,
        failed_at: &str,
    ) -> Result<(), StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "UPDATE message_deliveries
             SET status = 'failed', updated_at = ?1
             WHERE status = 'pending'",
            params![failed_at],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn claim_failed_delivery(
        &self,
        message_id: MessageId,
        target_agent_id: AgentId,
        claimed_at: &str,
    ) -> Result<bool, StoreError> {
        Ok(self.connection.execute(
            "UPDATE message_deliveries
             SET status = 'pending', updated_at = ?3
             WHERE message_id = ?1 AND target_agent_id = ?2 AND status = 'failed'",
            params![
                message_id.to_string(),
                target_agent_id.to_string(),
                claimed_at
            ],
        )? == 1)
    }

    pub fn claim_failed_thread_mention_delivery(
        &mut self,
        message_id: MessageId,
        target_agent_id: AgentId,
        claimed_at: &str,
    ) -> Result<Option<(Message, MessageDelivery)>, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(mut delivery) = get_message_delivery(&transaction, message_id, target_agent_id)?
        else {
            transaction.commit()?;
            return Ok(None);
        };
        if delivery.status != DeliveryStatus::Failed {
            transaction.commit()?;
            return Ok(None);
        }
        let message = get_message(&transaction, message_id)?.ok_or(
            StoreError::InvalidStoredValue("message_delivery.message_id"),
        )?;
        let thread = require_open_thread(&transaction, message.conversation_id)?;
        let room_id = thread.room_id.expect("validated thread has a room");
        require_active_room(&transaction, room_id)?;
        require_active_agent(&transaction, target_agent_id)?;
        require_active_room_membership(&transaction, room_id, target_agent_id)?;
        require_active_thread_membership(&transaction, message.conversation_id, target_agent_id)?;
        if transaction.execute(
            "UPDATE message_deliveries
             SET status = 'pending', updated_at = ?3
             WHERE message_id = ?1 AND target_agent_id = ?2 AND status = 'failed'",
            params![
                message_id.to_string(),
                target_agent_id.to_string(),
                claimed_at
            ],
        )? != 1
        {
            transaction.commit()?;
            return Ok(None);
        }
        delivery.status = DeliveryStatus::Pending;
        delivery.updated_at = claimed_at.into();
        delivery.validate()?;
        transaction.commit()?;
        Ok(Some((message, delivery)))
    }

    pub fn claim_failed_agent_direct_message_delivery(
        &mut self,
        message_id: MessageId,
        target_agent_id: AgentId,
        claimed_at: &str,
    ) -> Result<Option<(Message, MessageDelivery)>, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(mut delivery) = get_message_delivery(&transaction, message_id, target_agent_id)?
        else {
            transaction.commit()?;
            return Ok(None);
        };
        if delivery.status != DeliveryStatus::Failed {
            transaction.commit()?;
            return Ok(None);
        }
        let message = get_message(&transaction, message_id)?.ok_or(
            StoreError::InvalidStoredValue("message_delivery.message_id"),
        )?;
        require_agent_dm_scope(&transaction, &message, target_agent_id)?;
        if transaction.execute(
            "UPDATE message_deliveries
             SET status = 'pending', updated_at = ?3
             WHERE message_id = ?1 AND target_agent_id = ?2 AND status = 'failed'",
            params![
                message_id.to_string(),
                target_agent_id.to_string(),
                claimed_at
            ],
        )? != 1
        {
            transaction.commit()?;
            return Ok(None);
        }
        delivery.status = DeliveryStatus::Pending;
        delivery.updated_at = claimed_at.into();
        delivery.validate()?;
        transaction.commit()?;
        Ok(Some((message, delivery)))
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

    pub(crate) fn list_recent_messages_after(
        &self,
        conversation_id: ConversationId,
        anchor: Option<&Message>,
        limit: usize,
    ) -> Result<(Vec<Message>, bool), StoreError> {
        let query_limit =
            i64::try_from(limit.saturating_add(1)).map_err(|_| StoreError::IntegerOutOfRange {
                field: "recovery message limit",
                value: limit as i128,
            })?;
        let mut messages = query_all(
            &self.connection,
            "SELECT id, conversation_id, sender_type, sender_id, body, reply_to,
                    metadata_json, created_at
             FROM messages
             WHERE conversation_id = ?1
               AND (?2 IS NULL OR created_at > ?2 OR (created_at = ?2 AND id > ?3))
             ORDER BY created_at DESC, id DESC
             LIMIT ?4",
            params![
                conversation_id.to_string(),
                anchor.map(|message| message.created_at.as_str()),
                anchor.map(|message| message.id.to_string()),
                query_limit,
            ],
            records::message,
        )?;
        let truncated = messages.len() > limit;
        messages.truncate(limit);
        messages.reverse();
        Ok((messages, truncated))
    }

    pub fn insert_work_item(&self, work_item: &WorkItem) -> Result<(), StoreError> {
        if work_item.status != WorkStatus::Open
            || work_item.owner_agent_id.is_some()
            || work_item.is_primary
        {
            return Err(StoreError::InvalidStoredValue(
                "public work insert must be non-primary, open, and unowned",
            ));
        }
        insert_work_item(&self.connection, work_item)
    }

    pub fn get_work_item(&self, id: WorkItemId) -> Result<Option<WorkItem>, StoreError> {
        get_work_item(&self.connection, id)
    }

    pub fn list_work_items(
        &self,
        conversation_id: ConversationId,
    ) -> Result<Vec<WorkItem>, StoreError> {
        query_all(
            &self.connection,
            "SELECT id, conversation_id, title, goal, status, owner_agent_id,
                    is_primary, created_at, updated_at, completed_at, room_id
             FROM work_items WHERE conversation_id = ?1
             ORDER BY is_primary DESC, created_at, id",
            params![conversation_id.to_string()],
            records::work_item,
        )
    }

    /// Results produced by the work of one Conversation.
    pub fn list_work_results(
        &self,
        conversation_id: ConversationId,
    ) -> Result<Vec<WorkResult>, StoreError> {
        query_all(
            &self.connection,
            "SELECT r.id, r.work_id, r.status, r.summary, r.outputs_json, r.evidence_json,
                    r.supersedes_result_id, r.created_at
             FROM work_results r
             JOIN work_items w ON w.id = r.work_id
             WHERE w.conversation_id = ?1
             ORDER BY r.created_at, r.id",
            params![conversation_id.to_string()],
            records::work_result,
        )
    }

    /// Conversations that depend on this one's work; the deterministic
    /// `/publish` targets. Zero means no link, more than one means ambiguous.
    pub fn list_downstream_conversations(
        &self,
        conversation_id: ConversationId,
    ) -> Result<Vec<ConversationId>, StoreError> {
        query_all(
            &self.connection,
            "SELECT DISTINCT downstream.conversation_id
             FROM work_items upstream
             JOIN work_dependencies dependency
               ON dependency.upstream_work_id = upstream.id
             JOIN work_items downstream
               ON downstream.id = dependency.downstream_work_id
             WHERE upstream.conversation_id = ?1
               AND downstream.conversation_id <> ?1
             ORDER BY downstream.conversation_id",
            params![conversation_id.to_string()],
            records::conversation_id,
        )
    }

    pub fn assign_work_owner(
        &mut self,
        work_id: WorkItemId,
        owner_agent_id: AgentId,
        assigned_at: &str,
    ) -> Result<WorkItem, StoreError> {
        require_work_timestamp(assigned_at)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let work = assign_work_owner(&transaction, work_id, owner_agent_id, assigned_at)?;
        transaction.commit()?;
        Ok(work)
    }

    pub fn transition_work(
        &mut self,
        work_id: WorkItemId,
        target: WorkStatus,
        transitioned_at: &str,
    ) -> Result<WorkItem, StoreError> {
        require_work_timestamp(transitioned_at)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut work = require_work_item(&transaction, work_id)?;
        if work.status == target {
            transaction.commit()?;
            return Ok(work);
        }
        if target == WorkStatus::Ready || !work.status.can_transition_to(target) {
            return Err(StoreError::InvalidWorkTransition {
                work_id,
                from: work.status,
                to: target,
            });
        }
        work.status = target;
        work.updated_at = transitioned_at.into();
        work.completed_at = target.is_terminal().then(|| transitioned_at.into());
        work.validate()?;
        transaction.execute(
            "UPDATE work_items
             SET status = ?2, updated_at = ?3, completed_at = ?4
             WHERE id = ?1",
            params![
                work_id.to_string(),
                target.to_string(),
                transitioned_at,
                work.completed_at,
            ],
        )?;
        if target == WorkStatus::Failed {
            transaction.execute(
                "UPDATE work_dependencies
                 SET status = 'failed', result_id = NULL
                 WHERE upstream_work_id = ?1 AND status = 'waiting'",
                params![work_id.to_string()],
            )?;
        }
        transaction.commit()?;
        Ok(work)
    }

    pub fn add_work_dependency(
        &mut self,
        upstream_work_id: WorkItemId,
        downstream_work_id: WorkItemId,
        created_at: &str,
    ) -> Result<WorkDependency, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let dependency = add_work_dependency(
            &transaction,
            upstream_work_id,
            downstream_work_id,
            created_at,
        )?;
        transaction.commit()?;
        Ok(dependency)
    }

    pub fn get_work_dependency(
        &self,
        upstream_work_id: WorkItemId,
        downstream_work_id: WorkItemId,
    ) -> Result<Option<WorkDependency>, StoreError> {
        get_work_dependency(&self.connection, upstream_work_id, downstream_work_id)
    }

    pub fn list_work_dependency_outcomes_for_downstream(
        &self,
        downstream_work_id: WorkItemId,
    ) -> Result<Vec<(WorkDependency, Option<WorkResult>)>, StoreError> {
        require_work_item(&self.connection, downstream_work_id)?;
        query_all(
            &self.connection,
            "SELECT upstream_work_id, downstream_work_id, dependency_type, status, result_id,
                    created_at
             FROM work_dependencies
             WHERE downstream_work_id = ?1
             ORDER BY created_at, upstream_work_id",
            params![downstream_work_id.to_string()],
            records::work_dependency,
        )?
        .into_iter()
        .map(|dependency| {
            let result = match dependency.result_id {
                Some(result_id) => {
                    let result = get_work_result(&self.connection, result_id)?.ok_or(
                        StoreError::InvalidStoredValue("work dependency result reference"),
                    )?;
                    if result.work_id != dependency.upstream_work_id {
                        return Err(StoreError::InvalidStoredValue(
                            "work dependency result reference",
                        ));
                    }
                    Some(result)
                }
                None => None,
            };
            Ok((dependency, result))
        })
        .collect()
    }

    pub fn get_work_result(&self, id: ResultId) -> Result<Option<WorkResult>, StoreError> {
        get_work_result(&self.connection, id)
    }

    pub fn create_work_result(&mut self, result: &WorkResult) -> Result<WorkResult, StoreError> {
        result.validate()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(stored) = get_work_result(&transaction, result.id)? {
            if stored == *result {
                transaction.commit()?;
                return Ok(stored);
            }
            return Err(StoreError::WorkResultConflict(result.id));
        }

        let mut work = require_work_item(&transaction, result.work_id)?;
        if let Some(supersedes_result_id) = result.supersedes_result_id {
            let superseded = get_work_result(&transaction, supersedes_result_id)?.ok_or(
                StoreError::SupersededWorkResultNotFound(supersedes_result_id),
            )?;
            if superseded.work_id != result.work_id {
                return Err(StoreError::CrossWorkResultSupersede {
                    result_id: result.id,
                    supersedes_result_id,
                });
            }
        } else {
            if !work.status.can_transition_to(WorkStatus::Ready) {
                return Err(StoreError::InvalidWorkTransition {
                    work_id: work.id,
                    from: work.status,
                    to: WorkStatus::Ready,
                });
            }
            work.status = WorkStatus::Ready;
            work.updated_at.clone_from(&result.created_at);
            work.completed_at = None;
            work.validate()?;
            transaction.execute(
                "UPDATE work_items
                 SET status = ?2, updated_at = ?3, completed_at = NULL
                 WHERE id = ?1",
                params![
                    work.id.to_string(),
                    WorkStatus::Ready.to_string(),
                    result.created_at,
                ],
            )?;
        }

        insert_work_result(&transaction, result)?;
        if let Some(supersedes_result_id) = result.supersedes_result_id {
            transaction.execute(
                "UPDATE work_dependencies
                 SET status = 'superseded', result_id = ?2
                 WHERE upstream_work_id = ?1
                   AND status IN ('satisfied', 'superseded')
                   AND result_id = ?3",
                params![
                    result.work_id.to_string(),
                    result.id.to_string(),
                    supersedes_result_id.to_string(),
                ],
            )?;
        } else {
            transaction.execute(
                "UPDATE work_dependencies
                 SET status = 'satisfied', result_id = ?2
                 WHERE upstream_work_id = ?1 AND status = 'waiting'",
                params![result.work_id.to_string(), result.id.to_string()],
            )?;
        }
        transaction.commit()?;
        Ok(result.clone())
    }

    pub fn publish_result(
        &mut self,
        publish_id: PublishId,
        result_id: ResultId,
        target_conversation_id: ConversationId,
        published_at: &str,
    ) -> Result<(Publish, WorkResult), StoreError> {
        if published_at.trim().is_empty() {
            return Err(StoreError::InvalidPublishTimestamp);
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;

        if let Some(stored) = get_publish(&transaction, publish_id)? {
            if stored.result_id != result_id
                || stored.target_conversation_id != target_conversation_id
            {
                return Err(StoreError::PublishIdConflict(publish_id));
            }
            let result = get_work_result(&transaction, stored.result_id)?
                .ok_or(StoreError::PublishResultNotFound(stored.result_id))?;
            transaction.commit()?;
            return Ok((stored, result));
        }

        if let Some(stored) =
            get_publish_by_natural_key(&transaction, result_id, target_conversation_id)?
        {
            let result = get_work_result(&transaction, stored.result_id)?
                .ok_or(StoreError::PublishResultNotFound(stored.result_id))?;
            transaction.commit()?;
            return Ok((stored, result));
        }

        let result = get_work_result(&transaction, result_id)?
            .ok_or(StoreError::PublishResultNotFound(result_id))?;
        let work = require_work_item(&transaction, result.work_id)?;
        let WorkScope::Conversation(source_conversation_id) = work.scope else {
            return Err(StoreError::InvalidStoredValue(
                "Room work cannot publish to a conversation",
            ));
        };
        if get_conversation(&transaction, source_conversation_id)?.is_none() {
            return Err(StoreError::PublishSourceNotFound(source_conversation_id));
        }
        if get_conversation(&transaction, target_conversation_id)?.is_none() {
            return Err(StoreError::PublishTargetNotFound(target_conversation_id));
        }

        let publish = Publish {
            id: publish_id,
            result_id,
            source_conversation_id,
            target_conversation_id,
            created_at: published_at.into(),
        };
        insert_publish(&transaction, &publish)?;
        transaction.commit()?;
        Ok((publish, result))
    }

    pub fn get_publish(&self, id: PublishId) -> Result<Option<Publish>, StoreError> {
        get_publish(&self.connection, id)
    }

    pub fn list_published_results(
        &self,
        target_conversation_id: ConversationId,
    ) -> Result<Vec<(Publish, WorkResult)>, StoreError> {
        if get_conversation(&self.connection, target_conversation_id)?.is_none() {
            return Err(StoreError::PublishTargetNotFound(target_conversation_id));
        }
        query_all(
            &self.connection,
            "SELECT id, result_id, source_conversation_id, target_conversation_id, created_at
             FROM publishes
             WHERE target_conversation_id = ?1
             ORDER BY created_at, id",
            params![target_conversation_id.to_string()],
            records::publish,
        )?
        .into_iter()
        .map(|publish| {
            let result = get_work_result(&self.connection, publish.result_id)?
                .ok_or(StoreError::PublishResultNotFound(publish.result_id))?;
            Ok((publish, result))
        })
        .collect()
    }

    /// Open one ownership negotiation for a work item. The proposal itself
    /// never moves ownership; only an accepted handoff does.
    pub fn propose_handoff(&mut self, handoff: &Handoff) -> Result<Handoff, StoreError> {
        require_handoff_timestamp(&handoff.created_at)?;
        require_handoff_timestamp(&handoff.updated_at)?;
        if handoff.status != HandoffStatus::Proposed || handoff.round_count != 0 {
            return Err(StoreError::InvalidHandoffTransition {
                handoff_id: handoff.id,
                from: handoff.status,
                to: HandoffStatus::Proposed,
            });
        }
        handoff.validate()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(stored) = get_handoff(&transaction, handoff.id)? {
            transaction.commit()?;
            return if stored == *handoff {
                Ok(stored)
            } else {
                Err(StoreError::HandoffIdConflict(handoff.id))
            };
        }
        let work = require_work_item(&transaction, handoff.work_id)?;
        if work.scope != WorkScope::Conversation(handoff.thread_id) {
            return Err(StoreError::HandoffWorkOutOfThread {
                work_id: handoff.work_id,
                thread_id: handoff.thread_id,
            });
        }
        if work.status.is_terminal() {
            return Err(StoreError::TerminalWorkOwnerImmutable(handoff.work_id));
        }
        for agent_id in [handoff.from_agent_id, handoff.to_agent_id] {
            require_active_agent(&transaction, agent_id)?;
            require_active_conversation_membership(
                &transaction,
                handoff.thread_id,
                handoff.work_id,
                agent_id,
            )?;
        }
        if let Some(proposed_owner_id) = handoff.proposed_owner_id {
            require_active_agent(&transaction, proposed_owner_id)?;
        }
        if open_handoff_exists(&transaction, handoff.work_id)? {
            return Err(StoreError::HandoffAlreadyOpen(handoff.work_id));
        }
        insert_handoff(&transaction, handoff)?;
        transaction.commit()?;
        Ok(handoff.clone())
    }

    /// Record the target agent's structured answer. Accepting transfers
    /// ownership; rejecting never does.
    pub fn respond_to_handoff(
        &mut self,
        handoff_id: HandoffId,
        response: &HandoffResponse,
        responded_at: &str,
    ) -> Result<Handoff, StoreError> {
        require_handoff_timestamp(responded_at)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut handoff = require_handoff(&transaction, handoff_id)?;
        if response.agent_id != handoff.to_agent_id {
            return Err(StoreError::HandoffRespondentMismatch {
                handoff_id,
                expected: handoff.to_agent_id,
            });
        }
        let target = match response.decision {
            HandoffDecision::Accept => HandoffStatus::Accepted,
            HandoffDecision::Reject => HandoffStatus::Rejected,
            HandoffDecision::Partial => HandoffStatus::Partial,
        };
        if handoff.status != HandoffStatus::Proposed {
            return Err(StoreError::InvalidHandoffTransition {
                handoff_id,
                from: handoff.status,
                to: target,
            });
        }
        if let Some(proposed_owner_id) = response.proposed_owner_id {
            require_active_agent(&transaction, proposed_owner_id)?;
        }

        handoff.status = target;
        handoff.reason.clone_from(&response.reason);
        handoff.evidence.clone_from(&response.evidence);
        handoff.owned_scope.clone_from(&response.owned_scope);
        handoff.rejected_scope.clone_from(&response.rejected_scope);
        handoff.proposed_owner_id = response.proposed_owner_id;
        handoff.updated_at = responded_at.into();
        if target != HandoffStatus::Accepted {
            handoff.round_count += 1;
        }
        handoff.validate()?;
        if target == HandoffStatus::Accepted {
            assign_work_owner(
                &transaction,
                handoff.work_id,
                handoff.to_agent_id,
                responded_at,
            )?;
        }
        update_handoff(&transaction, &handoff)?;
        transaction.commit()?;
        Ok(handoff)
    }

    /// The source agent accepts the target's answer and closes the
    /// negotiation without further rounds.
    pub fn resolve_handoff(
        &mut self,
        handoff_id: HandoffId,
        agent_id: AgentId,
        resolved_at: &str,
    ) -> Result<Handoff, StoreError> {
        self.close_handoff(handoff_id, agent_id, HandoffStatus::Resolved, resolved_at)
    }

    /// The source agent withdraws a proposal nobody has answered yet.
    pub fn cancel_handoff(
        &mut self,
        handoff_id: HandoffId,
        agent_id: AgentId,
        cancelled_at: &str,
    ) -> Result<Handoff, StoreError> {
        self.close_handoff(handoff_id, agent_id, HandoffStatus::Cancelled, cancelled_at)
    }

    fn close_handoff(
        &mut self,
        handoff_id: HandoffId,
        agent_id: AgentId,
        target: HandoffStatus,
        closed_at: &str,
    ) -> Result<Handoff, StoreError> {
        require_handoff_timestamp(closed_at)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut handoff = require_handoff(&transaction, handoff_id)?;
        if agent_id != handoff.from_agent_id {
            return Err(StoreError::HandoffSourceMismatch {
                handoff_id,
                expected: handoff.from_agent_id,
            });
        }
        let allowed = match target {
            HandoffStatus::Resolved => {
                matches!(
                    handoff.status,
                    HandoffStatus::Rejected | HandoffStatus::Partial
                )
            }
            _ => handoff.status == HandoffStatus::Proposed,
        };
        if !allowed {
            return Err(StoreError::InvalidHandoffTransition {
                handoff_id,
                from: handoff.status,
                to: target,
            });
        }
        handoff.status = target;
        handoff.updated_at = closed_at.into();
        handoff.validate()?;
        update_handoff(&transaction, &handoff)?;
        transaction.commit()?;
        Ok(handoff)
    }

    /// The source agent challenges the target's answer with new evidence.
    /// Within the round budget this reopens the proposal for another answer;
    /// once the budget is spent the negotiation escalates to a decision
    /// instead of continuing to ping-pong.
    pub fn challenge_handoff(
        &mut self,
        handoff_id: HandoffId,
        challenge: &HandoffChallenge,
        challenged_at: &str,
    ) -> Result<(Handoff, Option<Decision>), StoreError> {
        require_handoff_timestamp(challenged_at)?;
        if challenge.evidence.is_empty() {
            return Err(StoreError::HandoffChallengeMissingEvidence(handoff_id));
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut handoff = require_handoff(&transaction, handoff_id)?;
        if challenge.agent_id != handoff.from_agent_id {
            return Err(StoreError::HandoffSourceMismatch {
                handoff_id,
                expected: handoff.from_agent_id,
            });
        }
        if !matches!(
            handoff.status,
            HandoffStatus::Rejected | HandoffStatus::Partial
        ) {
            return Err(StoreError::InvalidHandoffTransition {
                handoff_id,
                from: handoff.status,
                to: HandoffStatus::Disputed,
            });
        }

        for evidence in &challenge.evidence {
            if !handoff.evidence.contains(evidence) {
                handoff.evidence.push(evidence.clone());
            }
        }
        handoff.updated_at = challenged_at.into();
        let escalated = handoff.round_count >= Handoff::MAX_DISPUTE_ROUNDS;
        let decision = if escalated {
            let decision = Decision {
                id: challenge.decision_id,
                thread_id: handoff.thread_id,
                decision_type: DecisionType::Ownership,
                title: format!("Ownership of work {}", handoff.work_id),
                decision: None,
                reason: handoff.reason.clone(),
                selected_proposal_id: None,
                alternatives: Vec::new(),
                evidence: handoff.evidence.clone(),
                participants: vec![handoff.from_agent_id, handoff.to_agent_id],
                decision_owner: challenge.decision_owner,
                status: DecisionStatus::NeedsDecision,
                supersedes_decision_id: None,
                created_at: challenged_at.into(),
                updated_at: challenged_at.into(),
            };
            insert_decision(&transaction, &decision)?;
            handoff.status = HandoffStatus::Disputed;
            handoff.decision_id = Some(decision.id);
            Some(decision)
        } else {
            // Inside the budget the target owes one more structured answer.
            handoff.status = HandoffStatus::Proposed;
            handoff.owned_scope.clear();
            handoff.rejected_scope.clear();
            None
        };
        handoff.validate()?;
        update_handoff(&transaction, &handoff)?;
        transaction.commit()?;
        Ok((handoff, decision))
    }

    /// Offer one candidate solution to a thread. A revision supersedes the
    /// proposal it replaces instead of mutating it.
    pub fn create_proposal(&mut self, proposal: &Proposal) -> Result<Proposal, StoreError> {
        require_proposal_timestamp(&proposal.created_at)?;
        require_proposal_timestamp(&proposal.updated_at)?;
        proposal.validate()?;
        if !proposal.status.is_live() {
            return Err(StoreError::InvalidProposalTransition {
                proposal_id: proposal.id,
                from: proposal.status,
                to: ProposalStatus::Open,
            });
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(stored) = get_proposal(&transaction, proposal.id)? {
            transaction.commit()?;
            return if stored == *proposal {
                Ok(stored)
            } else {
                Err(StoreError::ProposalIdConflict(proposal.id))
            };
        }
        require_thread(&transaction, proposal.thread_id)?;
        require_active_agent(&transaction, proposal.author_agent_id)?;
        require_active_thread_membership(
            &transaction,
            proposal.thread_id,
            proposal.author_agent_id,
        )?;
        if let Some(superseded_id) = proposal.supersedes_proposal_id {
            let superseded = require_proposal(&transaction, superseded_id)?;
            if superseded.thread_id != proposal.thread_id {
                return Err(StoreError::ProposalSupersedeOutOfThread {
                    proposal_id: proposal.id,
                    superseded_id,
                });
            }
            set_proposal_status(
                &transaction,
                superseded_id,
                ProposalStatus::Superseded,
                &proposal.updated_at,
            )?;
        }
        insert_proposal(&transaction, proposal)?;
        transaction.commit()?;
        Ok(proposal.clone())
    }

    /// Record one agent's answer to a live proposal. An amendment request
    /// marks the proposal as amended so the author owes a revision.
    pub fn respond_to_proposal(
        &mut self,
        response: &ProposalResponse,
    ) -> Result<ProposalResponse, StoreError> {
        require_proposal_timestamp(&response.created_at)?;
        response.validate()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(stored) = get_proposal_response(&transaction, response.id)? {
            transaction.commit()?;
            return if stored == *response {
                Ok(stored)
            } else {
                Err(StoreError::ProposalResponseIdConflict(response.id))
            };
        }
        let proposal = require_proposal(&transaction, response.proposal_id)?;
        if !proposal.status.is_live() {
            return Err(StoreError::ProposalNotLive {
                proposal_id: proposal.id,
                status: proposal.status,
            });
        }
        require_active_agent(&transaction, response.agent_id)?;
        require_active_thread_membership(&transaction, proposal.thread_id, response.agent_id)?;
        insert_proposal_response(&transaction, response)?;
        if response.response_type == ProposalResponseType::Amend {
            set_proposal_status(
                &transaction,
                proposal.id,
                ProposalStatus::Amended,
                &response.created_at,
            )?;
        }
        transaction.commit()?;
        Ok(response.clone())
    }

    /// The author takes a live proposal off the table.
    pub fn withdraw_proposal(
        &mut self,
        proposal_id: ProposalId,
        agent_id: AgentId,
        withdrawn_at: &str,
    ) -> Result<Proposal, StoreError> {
        require_proposal_timestamp(withdrawn_at)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let proposal = require_proposal(&transaction, proposal_id)?;
        if proposal.author_agent_id != agent_id {
            return Err(StoreError::ProposalAuthorMismatch {
                proposal_id,
                expected: proposal.author_agent_id,
            });
        }
        let proposal = set_proposal_status(
            &transaction,
            proposal_id,
            ProposalStatus::Withdrawn,
            withdrawn_at,
        )?;
        transaction.commit()?;
        Ok(proposal)
    }

    pub fn get_proposal(&self, proposal_id: ProposalId) -> Result<Option<Proposal>, StoreError> {
        get_proposal(&self.connection, proposal_id)
    }

    pub fn list_proposals_for_thread(
        &self,
        thread_id: ConversationId,
    ) -> Result<Vec<Proposal>, StoreError> {
        query_all(
            &self.connection,
            &format!("{PROPOSAL_COLUMNS} WHERE thread_id = ?1 ORDER BY created_at, id"),
            params![thread_id.to_string()],
            records::proposal,
        )
    }

    pub fn list_proposal_responses(
        &self,
        proposal_id: ProposalId,
    ) -> Result<Vec<ProposalResponse>, StoreError> {
        query_all(
            &self.connection,
            &format!("{PROPOSAL_RESPONSE_COLUMNS} WHERE proposal_id = ?1 ORDER BY created_at, id"),
            params![proposal_id.to_string()],
            records::proposal_response,
        )
    }

    /// Open a decision without settling it, for example a technical question
    /// a thread wants recorded before proposals exist.
    pub fn record_decision(&mut self, decision: &Decision) -> Result<Decision, StoreError> {
        require_decision_timestamp(&decision.created_at)?;
        require_decision_timestamp(&decision.updated_at)?;
        decision.validate()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(stored) = get_decision(&transaction, decision.id)? {
            transaction.commit()?;
            return if stored == *decision {
                Ok(stored)
            } else {
                Err(StoreError::DecisionIdConflict(decision.id))
            };
        }
        require_thread(&transaction, decision.thread_id)?;
        for agent_id in &decision.participants {
            require_active_thread_membership(&transaction, decision.thread_id, *agent_id)?;
        }
        if let DecisionOwner::Agent(agent_id) = decision.decision_owner {
            require_active_agent(&transaction, agent_id)?;
        }
        if let Some(superseded_id) = decision.supersedes_decision_id {
            let superseded = require_decision(&transaction, superseded_id)?;
            if superseded.thread_id != decision.thread_id {
                return Err(StoreError::DecisionSupersedeOutOfThread {
                    decision_id: decision.id,
                    superseded_id,
                });
            }
            mark_decision_superseded(&transaction, superseded_id, &decision.updated_at)?;
        }
        insert_decision(&transaction, decision)?;
        transaction.commit()?;
        Ok(decision.clone())
    }

    /// Settle a decision. An ownership outcome may name the agent that ends up
    /// owning the disputed work, which also closes the escalated handoff.
    pub fn decide(
        &mut self,
        decision_id: DecisionId,
        outcome: &DecisionOutcome,
        decided_at: &str,
    ) -> Result<Decision, StoreError> {
        require_decision_timestamp(decided_at)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut decision = require_decision(&transaction, decision_id)?;
        if !matches!(
            decision.status,
            DecisionStatus::Pending | DecisionStatus::NeedsDecision
        ) {
            return Err(StoreError::InvalidDecisionTransition {
                decision_id,
                from: decision.status,
                to: DecisionStatus::Decided,
            });
        }
        if outcome.decided_by != decision.decision_owner {
            return Err(StoreError::DecisionOwnerMismatch {
                decision_id,
                expected: decision.decision_owner,
            });
        }
        decision.decision = Some(outcome.decision.clone());
        decision.reason.clone_from(&outcome.reason);
        decision
            .selected_proposal_id
            .clone_from(&outcome.selected_proposal_id);
        if !outcome.evidence.is_empty() {
            decision.evidence.clone_from(&outcome.evidence);
        }
        decision.status = DecisionStatus::Decided;
        decision.updated_at = decided_at.into();
        decision.validate()?;
        update_decision(&transaction, &decision)?;
        if let Some(proposal_id) = decision.selected_proposal_id {
            let proposal = require_proposal(&transaction, proposal_id)?;
            if proposal.thread_id != decision.thread_id {
                return Err(StoreError::ProposalOutOfThread {
                    proposal_id,
                    thread_id: decision.thread_id,
                });
            }
            set_proposal_status(
                &transaction,
                proposal_id,
                ProposalStatus::Accepted,
                decided_at,
            )?;
        }

        if let Some(mut handoff) = get_handoff_for_decision(&transaction, decision_id)? {
            if let Some(owner_agent_id) = outcome.assigned_owner_id {
                assign_work_owner(&transaction, handoff.work_id, owner_agent_id, decided_at)?;
            }
            handoff.status = HandoffStatus::Resolved;
            handoff.updated_at = decided_at.into();
            handoff.validate()?;
            update_handoff(&transaction, &handoff)?;
        } else if outcome.assigned_owner_id.is_some() {
            return Err(StoreError::DecisionHasNoHandoff(decision_id));
        }
        transaction.commit()?;
        Ok(decision)
    }

    pub fn cancel_decision(
        &mut self,
        decision_id: DecisionId,
        cancelled_by: DecisionOwner,
        reason: &str,
        cancelled_at: &str,
    ) -> Result<Decision, StoreError> {
        require_decision_timestamp(cancelled_at)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut decision = require_decision(&transaction, decision_id)?;
        if !matches!(
            decision.status,
            DecisionStatus::Pending | DecisionStatus::NeedsDecision
        ) {
            return Err(StoreError::InvalidDecisionTransition {
                decision_id,
                from: decision.status,
                to: DecisionStatus::Cancelled,
            });
        }
        if cancelled_by != decision.decision_owner {
            return Err(StoreError::DecisionOwnerMismatch {
                decision_id,
                expected: decision.decision_owner,
            });
        }
        if reason.trim().is_empty() {
            return Err(StoreError::Domain(crate::domain::DomainError::EmptyField(
                "decision.reason",
            )));
        }
        decision.status = DecisionStatus::Cancelled;
        decision.reason = Some(reason.into());
        decision.updated_at = cancelled_at.into();
        decision.validate()?;
        update_decision(&transaction, &decision)?;
        if let Some(mut handoff) = get_handoff_for_decision(&transaction, decision_id)? {
            handoff.status = HandoffStatus::Resolved;
            handoff.updated_at = cancelled_at.into();
            handoff.validate()?;
            update_handoff(&transaction, &handoff)?;
        }
        transaction.commit()?;
        Ok(decision)
    }

    /// Turn a settled decision into executable work. The conversion is
    /// explicit, links every generated work item back to the decision, and a
    /// replay of the same request creates nothing new.
    pub fn convert_decision_to_work(
        &mut self,
        decision_id: DecisionId,
        items: &[DecisionWork],
        created_at: &str,
    ) -> Result<Vec<WorkItem>, StoreError> {
        require_work_timestamp(created_at)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let decision = require_decision(&transaction, decision_id)?;
        if decision.status != DecisionStatus::Decided {
            return Err(StoreError::DecisionNotDecided {
                decision_id,
                status: decision.status,
            });
        }

        let mut created = Vec::with_capacity(items.len());
        for item in items {
            let work = WorkItem {
                id: item.work_id,
                scope: WorkScope::Conversation(decision.thread_id),
                title: item.title.clone(),
                goal: item.goal.clone(),
                status: WorkStatus::Open,
                owner_agent_id: item.owner_agent_id,
                is_primary: false,
                created_at: created_at.into(),
                updated_at: created_at.into(),
                completed_at: None,
            };
            work.validate()?;
            match get_work_item(&transaction, item.work_id)? {
                Some(stored) => {
                    // Only work this decision already generated may be reused.
                    if !decision_generated_work(&transaction, decision_id, item.work_id)?
                        || stored.scope != work.scope
                        || stored.title != work.title
                        || stored.goal != work.goal
                        || stored.owner_agent_id != work.owner_agent_id
                    {
                        return Err(StoreError::DecisionWorkConflict {
                            decision_id,
                            work_id: item.work_id,
                        });
                    }
                    created.push(stored);
                }
                None => {
                    if let Some(owner_agent_id) = item.owner_agent_id {
                        require_active_agent(&transaction, owner_agent_id)?;
                        require_active_conversation_membership(
                            &transaction,
                            decision.thread_id,
                            item.work_id,
                            owner_agent_id,
                        )?;
                    }
                    insert_work_item(&transaction, &work)?;
                    transaction.execute(
                        "INSERT INTO decision_work_items(decision_id, work_id, created_at)
                         VALUES (?1, ?2, ?3)",
                        params![
                            decision_id.to_string(),
                            item.work_id.to_string(),
                            created_at
                        ],
                    )?;
                    created.push(work);
                }
            }
        }

        for item in items {
            for upstream_work_id in &item.depends_on {
                add_work_dependency(&transaction, *upstream_work_id, item.work_id, created_at)?;
            }
        }
        transaction.commit()?;
        Ok(created)
    }

    pub fn list_decision_work(&self, decision_id: DecisionId) -> Result<Vec<WorkItem>, StoreError> {
        query_all(
            &self.connection,
            "SELECT work.id, work.conversation_id, work.title, work.goal, work.status,
                    work.owner_agent_id, work.is_primary, work.created_at, work.updated_at,
                    work.completed_at, work.room_id
             FROM decision_work_items AS link
             JOIN work_items AS work ON work.id = link.work_id
             WHERE link.decision_id = ?1
             ORDER BY link.created_at, work.id",
            params![decision_id.to_string()],
            records::work_item,
        )
    }

    pub fn get_decision(&self, decision_id: DecisionId) -> Result<Option<Decision>, StoreError> {
        get_decision(&self.connection, decision_id)
    }

    pub fn list_decisions_for_thread(
        &self,
        thread_id: ConversationId,
    ) -> Result<Vec<Decision>, StoreError> {
        query_all(
            &self.connection,
            &format!("{DECISION_COLUMNS} WHERE thread_id = ?1 ORDER BY created_at, id"),
            params![thread_id.to_string()],
            records::decision,
        )
    }

    pub fn list_pending_decisions(&self) -> Result<Vec<Decision>, StoreError> {
        query_all(
            &self.connection,
            &format!(
                "{DECISION_COLUMNS} WHERE status IN ('pending', 'needs_decision') ORDER BY created_at, id"
            ),
            [],
            records::decision,
        )
    }

    pub fn get_handoff(&self, handoff_id: HandoffId) -> Result<Option<Handoff>, StoreError> {
        get_handoff(&self.connection, handoff_id)
    }

    pub fn list_handoffs_for_work(&self, work_id: WorkItemId) -> Result<Vec<Handoff>, StoreError> {
        query_all(
            &self.connection,
            &format!("{HANDOFF_COLUMNS} WHERE work_id = ?1 ORDER BY created_at, id"),
            params![work_id.to_string()],
            records::handoff,
        )
    }

    /// Claim once before any runtime side effect. Exact message replay cannot resend.
    pub(crate) fn claim_room_activation(
        &mut self,
        message_id: RoomMessageId,
        agent_id: AgentId,
        activated_at: &str,
    ) -> Result<Option<RoomActivationClaim>, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (agent, message) = validate_room_activation(&transaction, message_id, agent_id)?;
        let claimed: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM room_message_activations WHERE message_id = ?1 AND agent_id = ?2)",
            params![message_id.to_string(), agent_id.to_string()], |row| row.get(0))?;
        if claimed {
            return Ok(None);
        }
        let binding = query_optional(&transaction,
            "SELECT id, room_id, agent_id, transport_type, remote_session_id, generation, status, created_at, last_used_at
             FROM session_bindings WHERE room_id = ?1 AND agent_id = ?2 ORDER BY generation DESC LIMIT 1",
            params![message.room_id.to_string(), agent_id.to_string()], records::room_session_binding)?;
        let binding = match binding {
            Some(binding)
                if matches!(
                    binding.status,
                    SessionBindingStatus::Lost | SessionBindingStatus::Closed
                ) =>
            {
                return Err(StoreError::RoomSessionUnavailable(binding.id));
            }
            Some(binding) => binding,
            None => {
                let binding = RoomSessionBinding {
                    id: SessionBindingId::new(),
                    room_id: message.room_id,
                    agent_id,
                    transport_type: agent.transport_type.clone(),
                    remote_session_id: None,
                    generation: 1,
                    status: SessionBindingStatus::Disconnected,
                    created_at: activated_at.into(),
                    last_used_at: activated_at.into(),
                };
                transaction.execute("INSERT INTO session_bindings(id, room_id, agent_id, transport_type, generation, status, created_at, last_used_at)
                    VALUES (?1, ?2, ?3, ?4, 1, 'disconnected', ?5, ?5)",
                    params![binding.id.to_string(), binding.room_id.to_string(), agent_id.to_string(), binding.transport_type, activated_at])?;
                binding
            }
        };
        let unfinished: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM room_message_activations WHERE session_binding_id = ?1 AND status IN ('claimed', 'sent'))",
            params![binding.id.to_string()], |row| row.get(0))?;
        if unfinished {
            return Err(StoreError::RoomSessionUnavailable(binding.id));
        }
        transaction.execute("INSERT INTO room_message_activations(message_id, agent_id, session_binding_id, status, updated_at)
            VALUES (?1, ?2, ?3, 'claimed', ?4)",
            params![message_id.to_string(), agent_id.to_string(), binding.id.to_string(), activated_at])?;
        // Snapshot only preceding unseen shared messages, never messages after the trigger.
        let mut context = query_all(&transaction,
            "SELECT m.id, m.room_id, m.sender_type, m.sender_id, m.body, m.mentions_json, m.reply_to, m.created_at
             FROM room_messages m JOIN room_message_order o ON o.message_id = m.id
             WHERE m.room_id = ?1
               AND o.sequence < (SELECT sequence FROM room_message_order WHERE message_id = ?2)
               AND o.sequence > COALESCE((SELECT seen.sequence FROM agent_room_cursors c
                   JOIN room_message_order seen ON seen.message_id = c.last_seen_message_id
                   WHERE c.agent_id = ?3 AND c.room_id = ?1), 0)
             ORDER BY o.sequence DESC LIMIT 51",
            params![message.room_id.to_string(), message_id.to_string(), agent_id.to_string()], records::room_message)?;
        let truncated = context.len() > 50;
        context.truncate(50);
        context.reverse();
        transaction.commit()?;
        Ok(Some(RoomActivationClaim {
            agent,
            message,
            binding,
            context,
            truncated,
        }))
    }

    pub(crate) fn validate_room_activation(
        &self,
        message_id: RoomMessageId,
        agent_id: AgentId,
    ) -> Result<(), StoreError> {
        validate_room_activation(&self.connection, message_id, agent_id).map(|_| ())
    }

    pub(crate) fn load_room_recipient_message(
        &mut self,
        message_id: RoomMessageId,
        agent_id: AgentId,
    ) -> Result<RoomMessage, StoreError> {
        let transaction = self.connection.transaction()?;
        let (_, message) = validate_room_activation(&transaction, message_id, agent_id)?;
        transaction.commit()?;
        Ok(message)
    }

    pub fn get_room_session_binding(
        &self,
        room_id: RoomId,
        agent_id: AgentId,
    ) -> Result<Option<RoomSessionBinding>, StoreError> {
        query_optional(&self.connection,
            "SELECT id, room_id, agent_id, transport_type, remote_session_id, generation, status, created_at, last_used_at
             FROM session_bindings WHERE room_id = ?1 AND agent_id = ?2 ORDER BY generation DESC LIMIT 1",
            params![room_id.to_string(), agent_id.to_string()], records::room_session_binding)
    }

    pub(crate) fn attach_room_remote_session(
        &self,
        binding_id: SessionBindingId,
        remote: &str,
        at: &str,
    ) -> Result<(), StoreError> {
        if self.connection.execute("UPDATE session_bindings SET remote_session_id = ?1, status = 'active', last_used_at = ?2
            WHERE id = ?3 AND room_id IS NOT NULL AND status IN ('active', 'disconnected')",
            params![remote, at, binding_id.to_string()])? == 0 {
            return Err(StoreError::RoomSessionUnavailable(binding_id));
        }
        Ok(())
    }

    pub(crate) fn set_room_activation_status(
        &self,
        message_id: RoomMessageId,
        agent_id: AgentId,
        status: &str,
        at: &str,
    ) -> Result<(), StoreError> {
        let transaction = self.connection.unchecked_transaction()?;
        let changed = transaction.execute(
            "UPDATE room_message_activations SET status = ?1, updated_at = ?2
            WHERE message_id = ?3 AND agent_id = ?4 AND status IN ('claimed', 'sent')",
            params![status, at, message_id.to_string(), agent_id.to_string()],
        )?;
        // send_message only queues ACP work. Successful completion acknowledges the
        // shared context; failed/cancelled/uncertain turns cannot advance it.
        if changed == 1 && status == "completed" {
            transaction.execute(
                "INSERT INTO agent_room_cursors(agent_id, room_id, last_seen_message_id)
                 SELECT ?1, room_id, id FROM room_messages WHERE id = ?2
                 ON CONFLICT(agent_id, room_id) DO UPDATE SET last_seen_message_id = excluded.last_seen_message_id
                 WHERE (SELECT sequence FROM room_message_order WHERE message_id = excluded.last_seen_message_id)
                     > (SELECT sequence FROM room_message_order WHERE message_id = agent_room_cursors.last_seen_message_id)",
                params![agent_id.to_string(), message_id.to_string()],
            )?;
        }
        transaction.commit()?;
        Ok(())
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
             FROM session_bindings WHERE id = ?1 AND conversation_id IS NOT NULL",
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
             WHERE agent_id = ?1 AND conversation_id IS NOT NULL AND status IN ('active', 'disconnected')
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

    pub fn mark_binding_disconnected(
        &self,
        binding_id: SessionBindingId,
        last_used_at: &str,
    ) -> Result<bool, StoreError> {
        if self.connection.execute(
            "UPDATE session_bindings
             SET status = 'disconnected', last_used_at = ?1
             WHERE id = ?2 AND status IN ('active', 'disconnected')",
            params![last_used_at, binding_id.to_string()],
        )? != 0
        {
            return Ok(true);
        }
        Ok(self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM session_bindings WHERE id = ?1)",
            params![binding_id.to_string()],
            |row| row.get(0),
        )?)
    }

    pub fn begin_session_replacement(
        &mut self,
        source_binding_id: SessionBindingId,
        replacement_binding_id: SessionBindingId,
        capsule: &str,
        replaced_at: &str,
    ) -> Result<(SessionBinding, SessionRecovery), StoreError> {
        let requested_recovery = SessionRecovery {
            session_binding_id: replacement_binding_id,
            source_binding_id,
            capsule: capsule.into(),
            capsule_delivered_at: None,
            created_at: replaced_at.into(),
        };
        requested_recovery.validate()?;

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let source = session_binding_by_id(&transaction, source_binding_id)?.ok_or(
            StoreError::SessionReplacementSourceNotFound(source_binding_id),
        )?;

        if let Some(existing_recovery) =
            session_recovery_by_source(&transaction, source_binding_id)?
        {
            if existing_recovery.session_binding_id == replacement_binding_id
                && existing_recovery.capsule == capsule
                && existing_recovery.created_at == replaced_at
            {
                let replacement = session_binding_by_id(&transaction, replacement_binding_id)?
                    .ok_or(StoreError::SessionRecoveryNotFound(replacement_binding_id))?;
                transaction.commit()?;
                return Ok((replacement, existing_recovery));
            }
            return Err(StoreError::SessionReplacementConflict {
                source_binding_id,
                replacement_binding_id,
            });
        }

        if session_binding_by_id(&transaction, replacement_binding_id)?.is_some() {
            return Err(StoreError::SessionReplacementConflict {
                source_binding_id,
                replacement_binding_id,
            });
        }
        let latest = query_optional(
            &transaction,
            "SELECT id, conversation_id, agent_id, transport_type, remote_session_id,
                    generation, status, created_at, last_used_at
             FROM session_bindings
             WHERE conversation_id = ?1 AND agent_id = ?2
             ORDER BY generation DESC LIMIT 1",
            params![
                source.conversation_id.to_string(),
                source.agent_id.to_string()
            ],
            records::session_binding,
        )?
        .ok_or(StoreError::SessionReplacementSourceNotFound(
            source_binding_id,
        ))?;
        if latest.id != source_binding_id {
            return Err(StoreError::SessionReplacementSourceStale {
                source_binding_id,
                latest_binding_id: latest.id,
            });
        }
        if source.status == SessionBindingStatus::Closed {
            return Err(StoreError::SessionReplacementSourceUnavailable {
                source_binding_id,
                status: source.status,
            });
        }
        let generation = u32::try_from(source.generation)
            .ok()
            .and_then(|generation| generation.checked_add(1))
            .ok_or(StoreError::SessionReplacementGenerationExhausted(
                source_binding_id,
            ))?;
        let replacement = SessionBinding {
            id: replacement_binding_id,
            conversation_id: source.conversation_id,
            agent_id: source.agent_id,
            transport_type: source.transport_type,
            remote_session_id: None,
            generation: u64::from(generation),
            status: SessionBindingStatus::Disconnected,
            created_at: replaced_at.into(),
            last_used_at: replaced_at.into(),
        };
        replacement.validate()?;

        transaction.execute(
            "UPDATE session_bindings
             SET status = 'lost', last_used_at = ?1
             WHERE id = ?2",
            params![replaced_at, source_binding_id.to_string()],
        )?;
        transaction.execute(
            "INSERT INTO session_bindings(
                id, conversation_id, agent_id, transport_type, remote_session_id,
                generation, status, created_at, last_used_at
             ) VALUES (?1, ?2, ?3, ?4, NULL, ?5, 'disconnected', ?6, ?6)",
            params![
                replacement.id.to_string(),
                replacement.conversation_id.to_string(),
                replacement.agent_id.to_string(),
                replacement.transport_type,
                i64::from(generation),
                replaced_at,
            ],
        )?;
        transaction.execute(
            "INSERT INTO session_recoveries(
                session_binding_id, source_binding_id, capsule, created_at
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                replacement_binding_id.to_string(),
                source_binding_id.to_string(),
                capsule,
                replaced_at,
            ],
        )?;
        transaction.commit()?;
        Ok((replacement, requested_recovery))
    }

    pub fn get_session_recovery(
        &self,
        session_binding_id: SessionBindingId,
    ) -> Result<Option<SessionRecovery>, StoreError> {
        session_recovery_by_id(&self.connection, session_binding_id)
    }

    pub fn attach_replacement_remote_session(
        &mut self,
        session_binding_id: SessionBindingId,
        remote_session_id: &str,
        attached_at: &str,
    ) -> Result<SessionBinding, StoreError> {
        require_store_text(remote_session_id, "session_binding.remote_session_id")?;
        require_store_text(attached_at, "session_binding.last_used_at")?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let recovery = session_recovery_by_id(&transaction, session_binding_id)?
            .ok_or(StoreError::SessionRecoveryNotFound(session_binding_id))?;
        let mut binding = session_binding_by_id(&transaction, session_binding_id)?
            .ok_or(StoreError::SessionRecoveryNotFound(session_binding_id))?;

        if let Some(existing_remote_session_id) = binding.remote_session_id.as_deref() {
            if existing_remote_session_id == remote_session_id {
                transaction.commit()?;
                return Ok(binding);
            }
            return Err(StoreError::SessionRecoveryRemoteAttachmentConflict(
                session_binding_id,
            ));
        }
        if recovery.capsule_delivered_at.is_some()
            || binding.status != SessionBindingStatus::Disconnected
        {
            return Err(StoreError::SessionRecoveryRemoteAttachmentConflict(
                session_binding_id,
            ));
        }
        transaction.execute(
            "UPDATE session_bindings
             SET remote_session_id = ?1, status = 'active', last_used_at = ?2
             WHERE id = ?3 AND remote_session_id IS NULL AND status = 'disconnected'",
            params![
                remote_session_id,
                attached_at,
                session_binding_id.to_string()
            ],
        )?;
        transaction.commit()?;
        binding.remote_session_id = Some(remote_session_id.into());
        binding.status = SessionBindingStatus::Active;
        binding.last_used_at = attached_at.into();
        Ok(binding)
    }

    pub fn mark_session_recovery_capsule_delivered(
        &mut self,
        session_binding_id: SessionBindingId,
        delivered_at: &str,
    ) -> Result<bool, StoreError> {
        require_store_text(delivered_at, "session_recovery.capsule_delivered_at")?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let recovery = session_recovery_by_id(&transaction, session_binding_id)?
            .ok_or(StoreError::SessionRecoveryNotFound(session_binding_id))?;
        if recovery.capsule_delivered_at.is_some() {
            transaction.commit()?;
            return Ok(false);
        }
        let binding = session_binding_by_id(&transaction, session_binding_id)?
            .ok_or(StoreError::SessionRecoveryNotFound(session_binding_id))?;
        if binding.remote_session_id.is_none() || binding.status != SessionBindingStatus::Active {
            return Err(StoreError::SessionRecoveryNotAttached(session_binding_id));
        }
        transaction.execute(
            "UPDATE session_recoveries
             SET capsule_delivered_at = ?1
             WHERE session_binding_id = ?2 AND capsule_delivered_at IS NULL",
            params![delivered_at, session_binding_id.to_string()],
        )?;
        transaction.commit()?;
        Ok(true)
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
        let transaction = self.connection.unchecked_transaction()?;
        if let Some(last_message_id) = checkpoint.last_message_id {
            let belongs_to_conversation: bool = transaction.query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM messages WHERE id = ?1 AND conversation_id = ?2
                 )",
                params![
                    last_message_id.to_string(),
                    checkpoint.conversation_id.to_string()
                ],
                |row| row.get(0),
            )?;
            if !belongs_to_conversation {
                return Err(StoreError::InvalidStoredValue(
                    "checkpoint.last_message_id conversation",
                ));
            }
        }
        transaction.execute(
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
        transaction.commit()?;
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

    pub fn get_latest_checkpoint(
        &self,
        conversation_id: ConversationId,
        agent_id: AgentId,
    ) -> Result<Option<Checkpoint>, StoreError> {
        query_optional(
            &self.connection,
            "SELECT id, conversation_id, agent_id, goal, current_state, decisions_json,
                    open_items_json, references_json, last_message_id, created_at
             FROM checkpoints
             WHERE conversation_id = ?1 AND agent_id = ?2
             ORDER BY created_at DESC, id DESC
             LIMIT 1",
            params![conversation_id.to_string(), agent_id.to_string()],
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

    pub fn promote_memory(&self, memory: &Memory) -> Result<(), StoreError> {
        memory.validate()?;
        let evidence = serde_json::to_string(&memory.evidence)?;
        let transaction = self.connection.unchecked_transaction()?;
        let source_conversation_id =
            memory
                .source_conversation_id
                .ok_or(StoreError::InvalidStoredValue(
                    "memory.source_conversation_id",
                ))?;
        let source_exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM conversations WHERE id = ?1)",
            params![source_conversation_id.to_string()],
            |row| row.get(0),
        )?;
        if !source_exists {
            return Err(StoreError::InvalidStoredValue(
                "memory.source_conversation_id",
            ));
        }
        if let Some(supersedes_memory_id) = memory.supersedes_memory_id {
            if supersedes_memory_id == memory.id {
                return Err(StoreError::InvalidStoredValue(
                    "memory.supersedes_memory_id",
                ));
            }
            let predecessor_scope = query_optional(
                &transaction,
                "SELECT scope_type, scope_id FROM memories WHERE id = ?1",
                params![supersedes_memory_id.to_string()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )?;
            if predecessor_scope != Some((memory.scope_type.to_string(), memory.scope_id.clone())) {
                return Err(StoreError::InvalidStoredValue(
                    "memory.supersedes_memory_id scope",
                ));
            }
        }
        transaction.execute(
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
                source_conversation_id.to_string(),
                evidence,
                memory.supersedes_memory_id.map(|id| id.to_string()),
                memory.created_at,
            ],
        )?;
        transaction.commit()?;
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

    pub fn list_memories(
        &self,
        scope_type: MemoryScopeType,
        scope_id: &str,
        kind: Option<MemoryKind>,
    ) -> Result<Vec<Memory>, StoreError> {
        query_all(
            &self.connection,
            "SELECT id, scope_type, scope_id, kind, content, source_conversation_id,
                    evidence_json, supersedes_memory_id, created_at
             FROM memories
             WHERE scope_type = ?1 AND scope_id = ?2 AND (?3 IS NULL OR kind = ?3)
             ORDER BY created_at ASC, id ASC",
            params![
                scope_type.to_string(),
                scope_id,
                kind.map(|kind| kind.to_string()),
            ],
            records::memory,
        )
    }

    pub(crate) fn list_current_recovery_memories(
        &self,
        project_root: &str,
        room_id: Option<RoomId>,
    ) -> Result<Vec<Memory>, StoreError> {
        query_all(
            &self.connection,
            "SELECT memory.id, memory.scope_type, memory.scope_id, memory.kind, memory.content,
                    memory.source_conversation_id, memory.evidence_json,
                    memory.supersedes_memory_id, memory.created_at
             FROM memories memory
             WHERE ((memory.scope_type = 'project' AND memory.scope_id = ?1)
                    OR (?2 IS NOT NULL AND memory.scope_type = 'room' AND memory.scope_id = ?2))
               AND NOT EXISTS (
                    SELECT 1 FROM memories successor
                    WHERE successor.supersedes_memory_id = memory.id
               )
             ORDER BY memory.scope_type, memory.kind, memory.created_at, memory.id",
            params![project_root, room_id.map(|id| id.to_string())],
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
        "INSERT INTO room_members(room_id, agent_id, role, generation, joined_at, left_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            member.room_id.to_string(),
            member.agent_id.to_string(),
            member.role,
            member.generation,
            member.joined_at,
            member.left_at,
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
            conversation_id, member_type, member_id, generation, joined_at, left_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            member.conversation_id.to_string(),
            member.member_type.to_string(),
            member.member_id,
            member.generation,
            member.joined_at,
            member.left_at,
        ],
    )?;
    Ok(())
}

fn get_or_create_agent_dm(
    connection: &Connection,
    source_agent_id: AgentId,
    target_agent_id: AgentId,
    now: &str,
) -> Result<Conversation, StoreError> {
    if source_agent_id == target_agent_id {
        return Err(StoreError::InvalidStoredValue(
            "agent DM requires distinct agent IDs",
        ));
    }
    require_active_agent(connection, source_agent_id)?;
    require_active_agent(connection, target_agent_id)?;
    let existing = query_optional(
        connection,
        "SELECT c.id, c.type, c.room_id, c.title, c.goal, c.parent_conversation_id,
                c.origin_conversation_id, c.status, c.created_at, c.updated_at
         FROM conversations c
         WHERE c.type = 'dm' AND c.status = 'open'
           AND 2 = (
               SELECT COUNT(*) FROM conversation_members m
               WHERE m.conversation_id = c.id AND m.left_at IS NULL
           )
           AND 2 = (
               SELECT COUNT(*) FROM conversation_members m
               WHERE m.conversation_id = c.id AND m.member_type = 'agent'
                 AND m.member_id IN (?1, ?2) AND m.left_at IS NULL
           )
         ORDER BY c.created_at, c.id
         LIMIT 1",
        params![source_agent_id.to_string(), target_agent_id.to_string()],
        records::conversation,
    )?;
    if let Some(existing) = existing {
        return Ok(existing);
    }

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
    conversation.validate()?;
    insert_conversation(connection, &conversation)?;
    for agent_id in [source_agent_id, target_agent_id] {
        insert_conversation_member(
            connection,
            &ConversationMember {
                conversation_id: conversation.id,
                member_type: MemberType::Agent,
                member_id: agent_id.to_string(),
                generation: 1,
                joined_at: now.into(),
                left_at: None,
            },
        )?;
    }
    Ok(conversation)
}

fn require_agent_dm_scope(
    connection: &Connection,
    message: &Message,
    target_agent_id: AgentId,
) -> Result<(), StoreError> {
    if message.sender_type != MemberType::Agent {
        return Err(StoreError::InvalidStoredValue("message.sender_type"));
    }
    let source_agent_id: AgentId = message.sender_id.parse()?;
    if source_agent_id == target_agent_id {
        return Err(StoreError::InvalidStoredValue(
            "agent DM requires distinct agent IDs",
        ));
    }
    require_active_agent(connection, source_agent_id)?;
    require_active_agent(connection, target_agent_id)?;
    let valid: bool = connection.query_row(
        "SELECT EXISTS (
             SELECT 1 FROM conversations c
             WHERE c.id = ?1 AND c.type = 'dm' AND c.status = 'open'
               AND 2 = (
                   SELECT COUNT(*) FROM conversation_members m
                   WHERE m.conversation_id = c.id AND m.left_at IS NULL
               )
               AND 2 = (
                   SELECT COUNT(*) FROM conversation_members m
                   WHERE m.conversation_id = c.id AND m.member_type = 'agent'
                     AND m.member_id IN (?2, ?3) AND m.left_at IS NULL
               )
         )",
        params![
            message.conversation_id.to_string(),
            source_agent_id.to_string(),
            target_agent_id.to_string()
        ],
        |row| row.get(0),
    )?;
    if !valid {
        return Err(StoreError::InvalidStoredValue(
            "message_delivery.agent_dm_scope",
        ));
    }
    Ok(())
}

fn insert_message(connection: &Connection, message: &Message) -> Result<bool, StoreError> {
    let metadata = if message.metadata.is_null() {
        None
    } else {
        Some(serde_json::to_string(&message.metadata)?)
    };
    let inserted = connection.execute(
        "INSERT INTO messages(
            id, conversation_id, sender_type, sender_id, body, reply_to,
            metadata_json, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(id) DO NOTHING",
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
    if inserted == 1 {
        return Ok(true);
    }
    let existing = query_optional(
        connection,
        "SELECT id, conversation_id, sender_type, sender_id, body, reply_to,
                metadata_json, created_at
         FROM messages WHERE id = ?1",
        params![message.id.to_string()],
        records::message,
    )?;
    if existing.as_ref() == Some(message) {
        Ok(false)
    } else {
        Err(StoreError::MessageConflict { id: message.id })
    }
}

fn insert_room_message(connection: &Connection, message: &RoomMessage) -> Result<(), StoreError> {
    let mentions = serde_json::to_string(
        &message
            .mentions
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
    )?;
    let inserted = connection.execute(
        "INSERT INTO room_messages(
            id, room_id, sender_type, sender_id, body, mentions_json, reply_to, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            message.id.to_string(),
            message.room_id.to_string(),
            message.sender_type.to_string(),
            message.sender_id,
            message.body,
            mentions,
            message.reply_to.map(|id| id.to_string()),
            message.created_at,
        ],
    )?;
    debug_assert_eq!(inserted, 1);
    Ok(())
}

fn insert_message_delivery(
    connection: &Connection,
    delivery: &MessageDelivery,
) -> Result<(), StoreError> {
    connection.execute(
        "INSERT INTO message_deliveries(
            message_id, target_agent_id, status, capsule, capsule_delivered_at,
            created_at, updated_at, delivered_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            delivery.message_id.to_string(),
            delivery.target_agent_id.to_string(),
            delivery.status.to_string(),
            delivery.capsule,
            delivery.capsule_delivered_at,
            delivery.created_at,
            delivery.updated_at,
            delivery.delivered_at,
        ],
    )?;
    Ok(())
}

fn get_message(
    connection: &Connection,
    message_id: MessageId,
) -> Result<Option<Message>, StoreError> {
    query_optional(
        connection,
        "SELECT id, conversation_id, sender_type, sender_id, body, reply_to,
                metadata_json, created_at
         FROM messages WHERE id = ?1",
        params![message_id.to_string()],
        records::message,
    )
}

fn failed_message_delivery(row: &Row<'_>) -> Result<FailedMessageDelivery, StoreError> {
    Ok(FailedMessageDelivery {
        message: records::message(row)?,
        delivery: MessageDelivery {
            message_id: row.get::<_, String>(8)?.parse()?,
            target_agent_id: row.get::<_, String>(9)?.parse()?,
            status: row.get::<_, String>(10)?.parse()?,
            capsule: row.get(11)?,
            capsule_delivered_at: row.get(12)?,
            created_at: row.get(13)?,
            updated_at: row.get(14)?,
            delivered_at: row.get(15)?,
        },
        conversation_kind: row.get::<_, String>(16)?.parse()?,
    })
}

fn get_message_delivery(
    connection: &Connection,
    message_id: MessageId,
    target_agent_id: AgentId,
) -> Result<Option<MessageDelivery>, StoreError> {
    query_optional(
        connection,
        "SELECT message_id, target_agent_id, status, capsule, capsule_delivered_at,
                created_at, updated_at, delivered_at
         FROM message_deliveries WHERE message_id = ?1 AND target_agent_id = ?2",
        params![message_id.to_string(), target_agent_id.to_string()],
        records::message_delivery,
    )
}

fn get_work_item(
    connection: &Connection,
    work_id: WorkItemId,
) -> Result<Option<WorkItem>, StoreError> {
    query_optional(
        connection,
        "SELECT id, conversation_id, title, goal, status, owner_agent_id,
                is_primary, created_at, updated_at, completed_at, room_id
         FROM work_items WHERE id = ?1",
        params![work_id.to_string()],
        records::work_item,
    )
}

fn get_work_dependency(
    connection: &Connection,
    upstream_work_id: WorkItemId,
    downstream_work_id: WorkItemId,
) -> Result<Option<WorkDependency>, StoreError> {
    query_optional(
        connection,
        "SELECT upstream_work_id, downstream_work_id, dependency_type, status, result_id,
                created_at
         FROM work_dependencies
         WHERE upstream_work_id = ?1 AND downstream_work_id = ?2",
        params![upstream_work_id.to_string(), downstream_work_id.to_string()],
        records::work_dependency,
    )
}

fn get_conversation(
    connection: &Connection,
    conversation_id: ConversationId,
) -> Result<Option<Conversation>, StoreError> {
    query_optional(
        connection,
        "SELECT id, type, room_id, title, goal, parent_conversation_id,
                origin_conversation_id, status, created_at, updated_at
         FROM conversations WHERE id = ?1",
        params![conversation_id.to_string()],
        records::conversation,
    )
}

fn get_work_result(
    connection: &Connection,
    result_id: ResultId,
) -> Result<Option<WorkResult>, StoreError> {
    query_optional(
        connection,
        "SELECT id, work_id, status, summary, outputs_json, evidence_json,
                supersedes_result_id, created_at
         FROM work_results WHERE id = ?1",
        params![result_id.to_string()],
        records::work_result,
    )
}

fn get_publish(
    connection: &Connection,
    publish_id: PublishId,
) -> Result<Option<Publish>, StoreError> {
    query_optional(
        connection,
        "SELECT id, result_id, source_conversation_id, target_conversation_id, created_at
         FROM publishes WHERE id = ?1",
        params![publish_id.to_string()],
        records::publish,
    )
}

fn get_publish_by_natural_key(
    connection: &Connection,
    result_id: ResultId,
    target_conversation_id: ConversationId,
) -> Result<Option<Publish>, StoreError> {
    query_optional(
        connection,
        "SELECT id, result_id, source_conversation_id, target_conversation_id, created_at
         FROM publishes
         WHERE result_id = ?1 AND target_conversation_id = ?2",
        params![result_id.to_string(), target_conversation_id.to_string()],
        records::publish,
    )
}

fn add_work_dependency(
    connection: &Connection,
    upstream_work_id: WorkItemId,
    downstream_work_id: WorkItemId,
    created_at: &str,
) -> Result<WorkDependency, StoreError> {
    if upstream_work_id == downstream_work_id {
        return Err(StoreError::WorkDependencySelf(upstream_work_id));
    }
    let dependency = WorkDependency {
        upstream_work_id,
        downstream_work_id,
        dependency_type: crate::domain::DependencyType::Requires,
        status: crate::domain::DependencyStatus::Waiting,
        result_id: None,
        created_at: created_at.into(),
    };
    dependency.validate()?;
    require_work_item(connection, upstream_work_id)?;
    require_work_item(connection, downstream_work_id)?;
    if let Some(stored) = get_work_dependency(connection, upstream_work_id, downstream_work_id)? {
        if stored.created_at == dependency.created_at {
            return Ok(stored);
        }
        return Err(StoreError::WorkDependencyConflict {
            upstream_work_id,
            downstream_work_id,
        });
    }
    let cyclic: bool = connection.query_row(
        "WITH RECURSIVE reachable(work_id) AS (
             SELECT downstream_work_id
             FROM work_dependencies
             WHERE upstream_work_id = ?1
             UNION
             SELECT dependency.downstream_work_id
             FROM work_dependencies AS dependency
             JOIN reachable ON dependency.upstream_work_id = reachable.work_id
         )
         SELECT EXISTS(SELECT 1 FROM reachable WHERE work_id = ?2)",
        params![downstream_work_id.to_string(), upstream_work_id.to_string()],
        |row| row.get(0),
    )?;
    if cyclic {
        return Err(StoreError::WorkDependencyCycle {
            upstream_work_id,
            downstream_work_id,
        });
    }
    connection.execute(
        "INSERT INTO work_dependencies(
            upstream_work_id, downstream_work_id, dependency_type, status, result_id, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            dependency.upstream_work_id.to_string(),
            dependency.downstream_work_id.to_string(),
            dependency.dependency_type.to_string(),
            dependency.status.to_string(),
            dependency.result_id.map(|id| id.to_string()),
            dependency.created_at,
        ],
    )?;
    Ok(dependency)
}

fn assign_work_owner(
    connection: &Connection,
    work_id: WorkItemId,
    owner_agent_id: AgentId,
    assigned_at: &str,
) -> Result<WorkItem, StoreError> {
    let mut work = require_work_item(connection, work_id)?;
    if work.owner_agent_id == Some(owner_agent_id) {
        return Ok(work);
    }
    if work.status.is_terminal() {
        return Err(StoreError::TerminalWorkOwnerImmutable(work_id));
    }
    require_active_agent(connection, owner_agent_id)?;
    match work.scope {
        WorkScope::Conversation(conversation_id) => require_active_conversation_membership(
            connection,
            conversation_id,
            work_id,
            owner_agent_id,
        )?,
        WorkScope::Room(room_id) => {
            require_active_room(connection, room_id)?;
            require_active_room_membership(connection, room_id, owner_agent_id)?;
        }
    }
    work.owner_agent_id = Some(owner_agent_id);
    work.updated_at = assigned_at.into();
    work.validate()?;
    connection.execute(
        "UPDATE work_items SET owner_agent_id = ?2, updated_at = ?3 WHERE id = ?1",
        params![work_id.to_string(), owner_agent_id.to_string(), assigned_at],
    )?;
    Ok(work)
}

fn require_handoff_timestamp(timestamp: &str) -> Result<(), StoreError> {
    if timestamp.trim().is_empty() {
        Err(StoreError::InvalidHandoffTimestamp)
    } else {
        Ok(())
    }
}

fn require_handoff(connection: &Connection, handoff_id: HandoffId) -> Result<Handoff, StoreError> {
    get_handoff(connection, handoff_id)?.ok_or(StoreError::HandoffNotFound(handoff_id))
}

fn get_handoff(
    connection: &Connection,
    handoff_id: HandoffId,
) -> Result<Option<Handoff>, StoreError> {
    query_optional(
        connection,
        &format!("{HANDOFF_COLUMNS} WHERE id = ?1"),
        params![handoff_id.to_string()],
        records::handoff,
    )
}

fn open_handoff_exists(connection: &Connection, work_id: WorkItemId) -> Result<bool, StoreError> {
    Ok(connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM handoffs
            WHERE work_id = ?1
              AND status IN ('proposed', 'rejected', 'partial', 'disputed')
        )",
        params![work_id.to_string()],
        |row| row.get(0),
    )?)
}

fn insert_handoff(connection: &Connection, handoff: &Handoff) -> Result<(), StoreError> {
    connection.execute(
        "INSERT INTO handoffs(
            id, thread_id, work_id, from_agent_id, to_agent_id, status, reason,
            evidence_json, owned_scope_json, rejected_scope_json, proposed_owner_id,
            round_count, decision_id, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            handoff.id.to_string(),
            handoff.thread_id.to_string(),
            handoff.work_id.to_string(),
            handoff.from_agent_id.to_string(),
            handoff.to_agent_id.to_string(),
            handoff.status.to_string(),
            handoff.reason,
            serde_json::to_string(&handoff.evidence)?,
            serde_json::to_string(&handoff.owned_scope)?,
            serde_json::to_string(&handoff.rejected_scope)?,
            handoff.proposed_owner_id.map(|id| id.to_string()),
            handoff.round_count,
            handoff.decision_id.map(|id| id.to_string()),
            handoff.created_at,
            handoff.updated_at,
        ],
    )?;
    Ok(())
}

fn update_handoff(connection: &Connection, handoff: &Handoff) -> Result<(), StoreError> {
    connection.execute(
        "UPDATE handoffs
         SET status = ?2, reason = ?3, evidence_json = ?4, owned_scope_json = ?5,
             rejected_scope_json = ?6, proposed_owner_id = ?7, round_count = ?8,
             decision_id = ?9, updated_at = ?10
         WHERE id = ?1",
        params![
            handoff.id.to_string(),
            handoff.status.to_string(),
            handoff.reason,
            serde_json::to_string(&handoff.evidence)?,
            serde_json::to_string(&handoff.owned_scope)?,
            serde_json::to_string(&handoff.rejected_scope)?,
            handoff.proposed_owner_id.map(|id| id.to_string()),
            handoff.round_count,
            handoff.decision_id.map(|id| id.to_string()),
            handoff.updated_at,
        ],
    )?;
    Ok(())
}

fn require_proposal_timestamp(timestamp: &str) -> Result<(), StoreError> {
    if timestamp.trim().is_empty() {
        Err(StoreError::InvalidProposalTimestamp)
    } else {
        Ok(())
    }
}

fn require_proposal(
    connection: &Connection,
    proposal_id: ProposalId,
) -> Result<Proposal, StoreError> {
    get_proposal(connection, proposal_id)?.ok_or(StoreError::ProposalNotFound(proposal_id))
}

fn get_proposal(
    connection: &Connection,
    proposal_id: ProposalId,
) -> Result<Option<Proposal>, StoreError> {
    query_optional(
        connection,
        &format!("{PROPOSAL_COLUMNS} WHERE id = ?1"),
        params![proposal_id.to_string()],
        records::proposal,
    )
}

fn get_proposal_response(
    connection: &Connection,
    response_id: ProposalResponseId,
) -> Result<Option<ProposalResponse>, StoreError> {
    query_optional(
        connection,
        &format!("{PROPOSAL_RESPONSE_COLUMNS} WHERE id = ?1"),
        params![response_id.to_string()],
        records::proposal_response,
    )
}

fn set_proposal_status(
    connection: &Connection,
    proposal_id: ProposalId,
    target: ProposalStatus,
    changed_at: &str,
) -> Result<Proposal, StoreError> {
    let mut proposal = require_proposal(connection, proposal_id)?;
    if proposal.status == target {
        return Ok(proposal);
    }
    if !proposal.status.is_live() {
        return Err(StoreError::InvalidProposalTransition {
            proposal_id,
            from: proposal.status,
            to: target,
        });
    }
    proposal.status = target;
    proposal.updated_at = changed_at.into();
    proposal.validate()?;
    connection.execute(
        "UPDATE proposals SET status = ?2, updated_at = ?3 WHERE id = ?1",
        params![proposal_id.to_string(), target.to_string(), changed_at],
    )?;
    Ok(proposal)
}

fn insert_proposal(connection: &Connection, proposal: &Proposal) -> Result<(), StoreError> {
    connection.execute(
        "INSERT INTO proposals(
            id, thread_id, author_agent_id, title, problem_statement, approach,
            benefits_json, costs_json, risks_json, assumptions_json, evidence_json,
            status, supersedes_proposal_id, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            proposal.id.to_string(),
            proposal.thread_id.to_string(),
            proposal.author_agent_id.to_string(),
            proposal.title,
            proposal.problem_statement,
            proposal.approach,
            serde_json::to_string(&proposal.benefits)?,
            serde_json::to_string(&proposal.costs)?,
            serde_json::to_string(&proposal.risks)?,
            serde_json::to_string(&proposal.assumptions)?,
            serde_json::to_string(&proposal.evidence)?,
            proposal.status.to_string(),
            proposal.supersedes_proposal_id.map(|id| id.to_string()),
            proposal.created_at,
            proposal.updated_at,
        ],
    )?;
    Ok(())
}

fn insert_proposal_response(
    connection: &Connection,
    response: &ProposalResponse,
) -> Result<(), StoreError> {
    connection.execute(
        "INSERT INTO proposal_responses(
            id, proposal_id, agent_id, response_type, reason, evidence_json, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            response.id.to_string(),
            response.proposal_id.to_string(),
            response.agent_id.to_string(),
            response.response_type.to_string(),
            response.reason,
            serde_json::to_string(&response.evidence)?,
            response.created_at,
        ],
    )?;
    Ok(())
}

fn decision_generated_work(
    connection: &Connection,
    decision_id: DecisionId,
    work_id: WorkItemId,
) -> Result<bool, StoreError> {
    Ok(connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM decision_work_items WHERE decision_id = ?1 AND work_id = ?2
        )",
        params![decision_id.to_string(), work_id.to_string()],
        |row| row.get(0),
    )?)
}

fn require_decision_timestamp(timestamp: &str) -> Result<(), StoreError> {
    if timestamp.trim().is_empty() {
        Err(StoreError::InvalidDecisionTimestamp)
    } else {
        Ok(())
    }
}

fn require_decision(
    connection: &Connection,
    decision_id: DecisionId,
) -> Result<Decision, StoreError> {
    get_decision(connection, decision_id)?.ok_or(StoreError::DecisionNotFound(decision_id))
}

fn get_decision(
    connection: &Connection,
    decision_id: DecisionId,
) -> Result<Option<Decision>, StoreError> {
    query_optional(
        connection,
        &format!("{DECISION_COLUMNS} WHERE id = ?1"),
        params![decision_id.to_string()],
        records::decision,
    )
}

fn get_handoff_for_decision(
    connection: &Connection,
    decision_id: DecisionId,
) -> Result<Option<Handoff>, StoreError> {
    query_optional(
        connection,
        &format!("{HANDOFF_COLUMNS} WHERE decision_id = ?1"),
        params![decision_id.to_string()],
        records::handoff,
    )
}

fn decision_participants(decision: &Decision) -> Result<String, StoreError> {
    let participants: Vec<String> = decision
        .participants
        .iter()
        .map(ToString::to_string)
        .collect();
    Ok(serde_json::to_string(&participants)?)
}

fn insert_decision(connection: &Connection, decision: &Decision) -> Result<(), StoreError> {
    decision.validate()?;
    connection.execute(
        "INSERT INTO decisions(
            id, thread_id, decision_type, title, decision, reason, selected_proposal_id,
            alternatives_json, evidence_json, decision_owner, participants_json, status,
            supersedes_decision_id, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            decision.id.to_string(),
            decision.thread_id.to_string(),
            decision.decision_type.to_string(),
            decision.title,
            decision.decision,
            decision.reason,
            decision.selected_proposal_id.map(|id| id.to_string()),
            serde_json::to_string(&decision.alternatives)?,
            serde_json::to_string(&decision.evidence)?,
            decision.decision_owner.to_string(),
            decision_participants(decision)?,
            decision.status.to_string(),
            decision.supersedes_decision_id.map(|id| id.to_string()),
            decision.created_at,
            decision.updated_at,
        ],
    )?;
    Ok(())
}

fn update_decision(connection: &Connection, decision: &Decision) -> Result<(), StoreError> {
    decision.validate()?;
    connection.execute(
        "UPDATE decisions
         SET decision = ?2, reason = ?3, selected_proposal_id = ?4, alternatives_json = ?5,
             evidence_json = ?6, participants_json = ?7, status = ?8, updated_at = ?9
         WHERE id = ?1",
        params![
            decision.id.to_string(),
            decision.decision,
            decision.reason,
            decision.selected_proposal_id.map(|id| id.to_string()),
            serde_json::to_string(&decision.alternatives)?,
            serde_json::to_string(&decision.evidence)?,
            decision_participants(decision)?,
            decision.status.to_string(),
            decision.updated_at,
        ],
    )?;
    Ok(())
}

fn mark_decision_superseded(
    connection: &Connection,
    decision_id: DecisionId,
    superseded_at: &str,
) -> Result<(), StoreError> {
    let mut decision = require_decision(connection, decision_id)?;
    if decision.status == DecisionStatus::Superseded {
        return Ok(());
    }
    if decision.status != DecisionStatus::Decided {
        return Err(StoreError::InvalidDecisionTransition {
            decision_id,
            from: decision.status,
            to: DecisionStatus::Superseded,
        });
    }
    // A superseded decision keeps the outcome it once stated: the audit trail
    // is the point of recording it.
    decision.status = DecisionStatus::Superseded;
    decision.updated_at = superseded_at.into();
    decision.validate()?;
    update_decision(connection, &decision)?;
    Ok(())
}

fn require_work_item(connection: &Connection, work_id: WorkItemId) -> Result<WorkItem, StoreError> {
    get_work_item(connection, work_id)?.ok_or(StoreError::WorkItemNotFound(work_id))
}

fn require_work_timestamp(timestamp: &str) -> Result<(), StoreError> {
    if timestamp.trim().is_empty() {
        Err(StoreError::InvalidWorkTimestamp)
    } else {
        Ok(())
    }
}

fn insert_work_item(connection: &Connection, work_item: &WorkItem) -> Result<(), StoreError> {
    work_item.validate()?;
    let (conversation_id, room_id) = match work_item.scope {
        WorkScope::Conversation(id) => (Some(id.to_string()), None),
        WorkScope::Room(id) => {
            require_active_room(connection, id)?;
            (None, Some(id.to_string()))
        }
    };
    connection.execute(
        "INSERT INTO work_items(
            id, conversation_id, title, goal, status, owner_agent_id,
            is_primary, created_at, updated_at, completed_at, room_id
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            work_item.id.to_string(),
            conversation_id,
            work_item.title,
            work_item.goal,
            work_item.status.to_string(),
            work_item.owner_agent_id.map(|id| id.to_string()),
            work_item.is_primary,
            work_item.created_at,
            work_item.updated_at,
            work_item.completed_at,
            room_id,
        ],
    )?;
    Ok(())
}

fn insert_work_result(connection: &Connection, result: &WorkResult) -> Result<(), StoreError> {
    result.validate()?;
    connection.execute(
        "INSERT INTO work_results(
            id, work_id, status, summary, outputs_json, evidence_json,
            supersedes_result_id, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            result.id.to_string(),
            result.work_id.to_string(),
            result.status,
            result.summary,
            serde_json::to_string(&result.outputs)?,
            serde_json::to_string(&result.evidence)?,
            result.supersedes_result_id.map(|id| id.to_string()),
            result.created_at,
        ],
    )?;
    Ok(())
}

fn insert_publish(connection: &Connection, publish: &Publish) -> Result<(), StoreError> {
    publish.validate()?;
    connection.execute(
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

fn require_room(connection: &Connection, room_id: RoomId) -> Result<String, StoreError> {
    connection
        .query_row(
            "SELECT status FROM rooms WHERE id = ?1",
            params![room_id.to_string()],
            |row| row.get(0),
        )
        .map_err(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => StoreError::RoomNotFound(room_id),
            error => error.into(),
        })
}

fn require_active_room(connection: &Connection, room_id: RoomId) -> Result<(), StoreError> {
    if require_room(connection, room_id)? == "active" {
        Ok(())
    } else {
        Err(StoreError::RoomInactive(room_id))
    }
}

fn require_agent(connection: &Connection, agent_id: AgentId) -> Result<String, StoreError> {
    connection
        .query_row(
            "SELECT status FROM agents WHERE id = ?1",
            params![agent_id.to_string()],
            |row| row.get(0),
        )
        .map_err(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => StoreError::AgentNotFound(agent_id),
            error => error.into(),
        })
}

fn require_active_agent(connection: &Connection, agent_id: AgentId) -> Result<(), StoreError> {
    if require_agent(connection, agent_id)? == "active" {
        Ok(())
    } else {
        Err(StoreError::AgentInactive(agent_id))
    }
}

fn require_active_agent_record(
    connection: &Connection,
    agent_id: AgentId,
) -> Result<Agent, StoreError> {
    let agent = query_optional(
        connection,
        "SELECT id, name, project_root, transport_type, transport_config_json,
                status, metadata_json, created_at, updated_at
         FROM agents WHERE id = ?1",
        params![agent_id.to_string()],
        records::agent,
    )?
    .ok_or(StoreError::AgentNotFound(agent_id))?;
    if agent.status == "active" {
        Ok(agent)
    } else {
        Err(StoreError::AgentInactive(agent_id))
    }
}

fn require_thread(
    connection: &Connection,
    conversation_id: ConversationId,
) -> Result<Conversation, StoreError> {
    let conversation = query_optional(
        connection,
        "SELECT id, type, room_id, title, goal, parent_conversation_id,
                origin_conversation_id, status, created_at, updated_at
         FROM conversations WHERE id = ?1",
        params![conversation_id.to_string()],
        records::conversation,
    )?
    .ok_or(StoreError::ThreadNotFound(conversation_id))?;
    if conversation.kind == ConversationKind::Thread {
        Ok(conversation)
    } else {
        Err(StoreError::NotThread(conversation_id))
    }
}

fn require_open_thread(
    connection: &Connection,
    conversation_id: ConversationId,
) -> Result<Conversation, StoreError> {
    let conversation = require_thread(connection, conversation_id)?;
    if conversation.status == "open" {
        Ok(conversation)
    } else {
        Err(StoreError::ThreadNotOpen(conversation_id))
    }
}

fn require_active_room_membership(
    connection: &Connection,
    room_id: RoomId,
    agent_id: AgentId,
) -> Result<(), StoreError> {
    let active: bool = connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM room_members
            WHERE room_id = ?1 AND agent_id = ?2 AND left_at IS NULL
        )",
        params![room_id.to_string(), agent_id.to_string()],
        |row| row.get(0),
    )?;
    if active {
        Ok(())
    } else {
        Err(StoreError::RoomMembershipRequired { room_id, agent_id })
    }
}

fn require_active_thread_membership(
    connection: &Connection,
    thread_id: ConversationId,
    agent_id: AgentId,
) -> Result<(), StoreError> {
    let active: bool = connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM conversation_members
            WHERE conversation_id = ?1 AND member_type = 'agent'
              AND member_id = ?2 AND left_at IS NULL
        )",
        params![thread_id.to_string(), agent_id.to_string()],
        |row| row.get(0),
    )?;
    if active {
        Ok(())
    } else {
        Err(StoreError::ThreadMembershipRequired {
            thread_id,
            agent_id,
        })
    }
}

fn require_active_conversation_membership(
    connection: &Connection,
    conversation_id: ConversationId,
    work_id: WorkItemId,
    owner_agent_id: AgentId,
) -> Result<(), StoreError> {
    let active: bool = connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM conversation_members
            WHERE conversation_id = ?1 AND member_type = 'agent'
              AND member_id = ?2 AND left_at IS NULL
        )",
        params![conversation_id.to_string(), owner_agent_id.to_string()],
        |row| row.get(0),
    )?;
    if active {
        Ok(())
    } else {
        Err(StoreError::WorkOwnerScopeRequired {
            work_id,
            owner_agent_id,
        })
    }
}

fn next_room_membership_generation(
    connection: &Connection,
    room_id: RoomId,
    agent_id: AgentId,
) -> Result<u32, StoreError> {
    let generation: i64 = connection.query_row(
        "SELECT COALESCE(MAX(generation), 0) + 1
         FROM room_members WHERE room_id = ?1 AND agent_id = ?2",
        params![room_id.to_string(), agent_id.to_string()],
        |row| row.get(0),
    )?;
    generation
        .try_into()
        .map_err(|_| StoreError::IntegerOutOfRange {
            field: "room_members.generation",
            value: i128::from(generation),
        })
}

fn next_thread_membership_generation(
    connection: &Connection,
    conversation_id: ConversationId,
    member_type: MemberType,
    member_id: &str,
) -> Result<u32, StoreError> {
    let generation: i64 = connection.query_row(
        "SELECT COALESCE(MAX(generation), 0) + 1
         FROM conversation_members
         WHERE conversation_id = ?1 AND member_type = ?2 AND member_id = ?3",
        params![
            conversation_id.to_string(),
            member_type.to_string(),
            member_id
        ],
        |row| row.get(0),
    )?;
    generation
        .try_into()
        .map_err(|_| StoreError::IntegerOutOfRange {
            field: "conversation_members.generation",
            value: i128::from(generation),
        })
}

fn require_store_text(value: &str, field: &'static str) -> Result<(), StoreError> {
    if value.trim().is_empty() {
        Err(crate::domain::DomainError::EmptyField(field).into())
    } else {
        Ok(())
    }
}

fn append_room_message(
    connection: &Connection,
    message: &RoomMessage,
) -> Result<RoomMessage, StoreError> {
    let mut canonical = message.clone();
    if canonical.sender_type == MemberType::Agent {
        canonical.sender_id = canonical
            .sender_id
            .parse::<AgentId>()
            .map_err(|_| StoreError::InvalidStoredValue("room message agent sender"))?
            .to_string();
    }
    let message = &canonical;
    if let Some(existing) = query_optional(
        connection,
        "SELECT id, room_id, sender_type, sender_id, body, mentions_json, reply_to, created_at
             FROM room_messages WHERE id = ?1",
        params![message.id.to_string()],
        records::room_message,
    )? {
        if existing == *message {
            return Ok(existing);
        }
        return Err(StoreError::RoomMessageIdConflict(message.id));
    }
    message.validate()?;
    require_active_room(connection, message.room_id)?;
    if message.sender_type == MemberType::Agent {
        let agent_id = message
            .sender_id
            .parse()
            .expect("canonical agent sender id parses");
        require_active_agent(connection, agent_id)?;
        require_active_room_membership(connection, message.room_id, agent_id)?;
    }
    for target in &message.mentions {
        require_active_agent(connection, *target)?;
        require_active_room_membership(connection, message.room_id, *target)?;
    }
    if let Some(reply_to) = message.reply_to {
        let reply_room = query_optional(
            connection,
            "SELECT room_id FROM room_messages WHERE id = ?1",
            params![reply_to.to_string()],
            |row| row.get::<_, String>(0).map_err(StoreError::from),
        )?
        .ok_or(StoreError::RoomMessageReplyNotFound(reply_to))?;
        if reply_room != message.room_id.to_string() {
            return Err(StoreError::RoomMessageReplyNotInRoom {
                room_id: message.room_id,
                reply_to,
            });
        }
    }
    insert_room_message(connection, message)?;
    Ok(message.clone())
}

fn validate_room_activation(
    connection: &Connection,
    message_id: RoomMessageId,
    agent_id: AgentId,
) -> Result<(Agent, RoomMessage), StoreError> {
    let message = query_optional(connection,
        "SELECT id, room_id, sender_type, sender_id, body, mentions_json, reply_to, created_at FROM room_messages WHERE id = ?1",
        params![message_id.to_string()], records::room_message)?.ok_or(StoreError::RoomMessageNotFound(message_id))?;
    if !message.mentions.contains(&agent_id) {
        return Err(StoreError::RoomMessageTargetRequired {
            message_id,
            agent_id,
        });
    }
    require_active_room(connection, message.room_id)?;
    let agent = require_active_agent_record(connection, agent_id)?;
    require_active_room_membership(connection, message.room_id, agent_id)?;
    if message.sender_type == MemberType::Agent {
        let sender = message
            .sender_id
            .parse()
            .map_err(|_| StoreError::InvalidStoredValue("room sender"))?;
        require_active_agent(connection, sender)?;
        require_active_room_membership(connection, message.room_id, sender)?;
    }
    Ok((agent, message))
}

fn session_binding_by_id(
    connection: &Connection,
    id: SessionBindingId,
) -> Result<Option<SessionBinding>, StoreError> {
    query_optional(
        connection,
        "SELECT id, conversation_id, agent_id, transport_type, remote_session_id,
                generation, status, created_at, last_used_at
         FROM session_bindings WHERE id = ?1 AND conversation_id IS NOT NULL",
        params![id.to_string()],
        records::session_binding,
    )
}

fn session_recovery_by_id(
    connection: &Connection,
    session_binding_id: SessionBindingId,
) -> Result<Option<SessionRecovery>, StoreError> {
    query_optional(
        connection,
        "SELECT session_binding_id, source_binding_id, capsule, capsule_delivered_at, created_at
         FROM session_recoveries WHERE session_binding_id = ?1",
        params![session_binding_id.to_string()],
        records::session_recovery,
    )
}

fn session_recovery_by_source(
    connection: &Connection,
    source_binding_id: SessionBindingId,
) -> Result<Option<SessionRecovery>, StoreError> {
    query_optional(
        connection,
        "SELECT session_binding_id, source_binding_id, capsule, capsule_delivered_at, created_at
         FROM session_recoveries WHERE source_binding_id = ?1",
        params![source_binding_id.to_string()],
        records::session_recovery,
    )
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
    use crate::domain::{ConversationId, DomainError, PublishId, ResultId, WorkItemId};
    use crate::storage::StoreError;
    use rusqlite::{Connection, params};
    use std::env;
    use std::fs;
    use std::path::{Path, PathBuf};
    use ulid::Ulid;

    #[test]
    fn pre_room_work_database_is_rejected_without_conversion() {
        let db = TestDatabase::new();
        let connection = Connection::open(db.path()).unwrap();
        connection.execute_batch("CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY); INSERT INTO schema_migrations VALUES(19); CREATE TABLE work_items(id TEXT PRIMARY KEY, conversation_id TEXT NOT NULL); INSERT INTO work_items VALUES('old','conversation');").unwrap();
        assert!(matches!(
            SqliteStore::open(db.path()),
            Err(StoreError::InvalidStoredValue(
                "pre-Phase9 Work schema; use a fresh workspace database"
            ))
        ));
        assert_eq!(
            connection
                .query_row(
                    "SELECT conversation_id FROM work_items WHERE id='old'",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
            "conversation"
        );
        assert_eq!(super::current_schema_version(&connection).unwrap(), 19);
    }

    #[test]
    fn room_work_has_one_canonical_scope() {
        let db = TestDatabase::new();
        let store = SqliteStore::open(&db.path).expect("open store");
        store.connection.execute_batch("INSERT INTO rooms(id,name,status,created_at,updated_at) VALUES ('room-work','work','active','now','now');
            INSERT INTO work_items(id,room_id,title,status,created_at,updated_at) VALUES ('work-room','room-work','shared task','open','now','now');").expect("insert Room work without conversation");
        assert!(store.connection.execute("INSERT INTO work_items(id,title,status,created_at,updated_at) VALUES ('no-scope','invalid','open','now','now')", []).is_err());
    }

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
    fn migration_nineteen_preserves_room_history_bindings_and_cursor() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .unwrap();
        apply_migrations(&mut connection, &MIGRATIONS[..18]).unwrap();
        seed_session_parent_rows(&connection);
        connection.execute_batch("INSERT INTO rooms VALUES ('room', 'room', NULL, 'active', 'now', 'now');
            INSERT INTO room_messages(id, room_id, sender_type, sender_id, body, mentions_json, created_at) VALUES ('trigger', 'room', 'user', 'local-user', 'body', '[]', 'now');
            INSERT INTO session_bindings(id, room_id, agent_id, transport_type, remote_session_id, generation, status, created_at, last_used_at) VALUES ('room-binding', 'room', 'agent-1', 'acp', 'remote', 1, 'active', 'now', 'now');
            INSERT INTO room_message_activations VALUES ('trigger', 'agent-1', 'room-binding', 'completed', 'now');
            INSERT INTO agent_room_cursors VALUES ('agent-1', 'room', 'trigger');").unwrap();
        let tables = [
            "agents",
            "conversations",
            "rooms",
            "room_messages",
            "room_message_order",
            "session_bindings",
            "room_message_activations",
            "agent_room_cursors",
        ];
        let snapshot = |connection: &Connection| {
            tables
                .iter()
                .map(|table| {
                    let mut statement = connection
                        .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
                        .unwrap();
                    let count = statement.column_count();
                    statement
                        .query_map([], |row| {
                            (0..count)
                                .map(|column| row.get::<_, rusqlite::types::Value>(column))
                                .collect::<Result<Vec<_>, _>>()
                        })
                        .unwrap()
                        .collect::<Result<Vec<_>, _>>()
                        .unwrap()
                })
                .collect::<Vec<_>>()
        };
        let before = snapshot(&connection);
        apply_migrations(&mut connection, &MIGRATIONS).unwrap();
        assert_eq!(snapshot(&connection), before);
        assert_eq!(super::current_schema_version(&connection).unwrap(), 20);
        assert!(
            !connection
                .prepare("PRAGMA foreign_key_check")
                .unwrap()
                .query([])
                .unwrap()
                .next()
                .unwrap()
                .is_some()
        );
        connection.execute("INSERT INTO room_message_publications VALUES ('trigger', 'agent-1', 'key', 'trigger')", []).unwrap();
        assert!(
            connection
                .execute(
                    "UPDATE room_message_publications SET request_id = 'different'",
                    []
                )
                .is_err()
        );
        assert!(
            connection
                .execute("DELETE FROM room_message_publications", [])
                .is_err()
        );
    }

    #[test]
    fn agent_room_publication_is_scoped_idempotent_and_revocable() {
        use crate::domain::{
            Agent, AgentId, MemberType, Room, RoomId, RoomMessage, RoomMessageId, SendRoomMessage,
        };
        use std::sync::atomic::{AtomicBool, Ordering};
        let database = TestDatabase::new();
        let mut store = SqliteStore::open(database.path()).unwrap();
        let room = Room {
            id: RoomId::new(),
            name: "room".into(),
            description: None,
            status: "active".into(),
            created_at: "now".into(),
            updated_at: "now".into(),
        };
        store.create_room(&room).unwrap();
        let mut agents = Vec::new();
        for name in ["sender", "target", "outsider"] {
            let agent = Agent {
                id: AgentId::new(),
                name: name.into(),
                project_root: "/tmp".into(),
                transport_type: "acp".into(),
                transport_config: serde_json::json!({}),
                status: "active".into(),
                metadata: serde_json::json!({}),
                created_at: "now".into(),
                updated_at: "now".into(),
            };
            store.insert_agent(&agent).unwrap();
            if name != "outsider" {
                store
                    .add_room_member(room.id, agent.id, None, "now")
                    .unwrap();
            }
            agents.push(agent);
        }
        let trigger = RoomMessage {
            id: RoomMessageId::new(),
            room_id: room.id,
            sender_type: MemberType::User,
            sender_id: "local-user".into(),
            body: "question".into(),
            mentions: vec![agents[0].id],
            reply_to: None,
            created_at: "now".into(),
        };
        store.append_room_message(&trigger).unwrap();
        let claim = store
            .claim_room_activation(trigger.id, agents[0].id, "now")
            .unwrap()
            .unwrap();
        store
            .attach_room_remote_session(claim.binding.id, "remote", "now")
            .unwrap();
        let alive = AtomicBool::new(true);
        let mut request = SendRoomMessage {
            work: None,
            targets: vec!["target".into(), "target".into()],
            body: "hello".into(),
            reply_to: Some(trigger.id),
            request_id: Some("one".into()),
        };
        let sent = store
            .send_agent_room_message(trigger.id, agents[0].id, &request, "now", &alive)
            .unwrap();
        assert_eq!(sent.sender_id, agents[0].id.to_string());
        assert_eq!(sent.room_id, room.id);
        assert_eq!(sent.mentions, vec![agents[1].id]);
        let shared_request = SendRoomMessage {
            work: None,
            targets: vec![],
            request_id: Some("shared-answer".into()),
            ..request.clone()
        };
        let shared = store
            .send_agent_room_message(trigger.id, agents[0].id, &shared_request, "now", &alive)
            .unwrap();
        assert_eq!(shared.sender_type, MemberType::Agent);
        assert_eq!(shared.sender_id, agents[0].id.to_string());
        assert_eq!(shared.room_id, room.id);
        assert_eq!(shared.reply_to, Some(trigger.id));
        assert!(shared.mentions.is_empty());
        assert_eq!(
            store
                .send_agent_room_message(trigger.id, agents[0].id, &shared_request, "later", &alive)
                .unwrap(),
            shared
        );
        assert!(matches!(
            store.send_agent_room_message(
                trigger.id,
                agents[0].id,
                &SendRoomMessage {
                    body: "conflicting".into(),
                    ..shared_request.clone()
                },
                "later",
                &alive
            ),
            Err(StoreError::RoomPublicationConflict)
        ));
        assert_eq!(
            store
                .send_agent_room_message(trigger.id, agents[0].id, &request, "later", &alive)
                .unwrap(),
            sent
        );
        request.body = "changed".into();
        assert!(
            store
                .send_agent_room_message(trigger.id, agents[0].id, &request, "later", &alive)
                .is_err()
        );
        request.request_id = None;
        request.targets = vec!["outsider".into()];
        assert!(
            store
                .send_agent_room_message(trigger.id, agents[0].id, &request, "later", &alive)
                .is_err()
        );
        request.targets = vec!["target".into()];
        request.body = sent.body.clone();
        request.request_id = Some("one".into());
        store
            .remove_room_member(room.id, agents[1].id, "later")
            .unwrap();
        assert!(
            store
                .send_agent_room_message(trigger.id, agents[0].id, &request, "later", &alive)
                .is_err()
        );
        store
            .add_room_member(room.id, agents[1].id, None, "later")
            .unwrap();
        store
            .remove_room_member(room.id, agents[0].id, "later")
            .unwrap();
        request = shared_request;
        assert!(
            store
                .send_agent_room_message(trigger.id, agents[0].id, &request, "later", &alive)
                .is_err()
        );
        store
            .add_room_member(room.id, agents[0].id, None, "later")
            .unwrap();
        alive.store(false, Ordering::Release);
        assert!(
            store
                .send_agent_room_message(trigger.id, agents[0].id, &request, "later", &alive)
                .is_err()
        );
        alive.store(true, Ordering::Release);
        store
            .set_room_activation_status(trigger.id, agents[0].id, "completed", "later")
            .unwrap();
        assert!(
            store
                .send_agent_room_message(trigger.id, agents[0].id, &request, "later", &alive)
                .is_err()
        );
        assert_eq!(
            store
                .list_recent_room_messages(room.id, 50)
                .unwrap()
                .0
                .len(),
            3
        );
    }

    #[test]
    fn fresh_database_has_schema_version_sixteen() {
        let database = TestDatabase::new();
        let store = SqliteStore::open(database.path()).expect("open fresh database");

        assert_eq!(store.schema_version().unwrap(), 20);
    }

    #[test]
    fn fresh_database_contains_canonical_and_search_tables() {
        let database = TestDatabase::new();
        let store = SqliteStore::open(database.path()).expect("open fresh database");
        let expected = [
            "agents",
            "rooms",
            "room_members",
            "room_messages",
            "conversations",
            "conversation_members",
            "messages",
            "message_deliveries",
            "work_items",
            "work_dependencies",
            "work_results",
            "publishes",
            "session_bindings",
            "session_recoveries",
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
            "room_messages",
            "room_message_activations",
            "conversations",
            "conversation_members",
            "messages",
            "message_deliveries",
            "work_items",
            "room_a2a_task_bindings",
            "room_message_work",
            "work_dependencies",
            "work_results",
            "publishes",
            "session_bindings",
            "session_recoveries",
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

        assert_eq!(foreign_key_count, 43);
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
            "uq_room_members_active",
            "uq_conversation_members_active",
            "uq_work_items_primary_conversation",
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
    fn work_result_fts_tracks_insert_and_immutable_rows_reject_mutation() {
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

        assert!(
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
                .is_err()
        );
        assert_eq!(
            fts_count(&store.connection, "work_results_fts", "initialsummary"),
            1
        );
        assert_eq!(
            fts_count(
                &store.connection,
                "work_results_fts",
                "revisedsummary revisedoutput revisedevidence"
            ),
            0
        );

        assert!(
            store
                .connection
                .execute("DELETE FROM work_results WHERE id = 'result-1'", [])
                .is_err()
        );
        assert_eq!(
            fts_count(&store.connection, "work_results_fts", "initialsummary"),
            1
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
            20
        );
        assert_eq!(
            SqliteStore::open(database.path())
                .unwrap()
                .schema_version()
                .unwrap(),
            20
        );
    }

    #[test]
    fn migration_three_preserves_v2_membership_and_work_rows() {
        let database = TestDatabase::new();
        let mut connection = Connection::open(database.path()).unwrap();
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .unwrap();
        apply_migrations(&mut connection, &MIGRATIONS[..2]).unwrap();
        seed_session_parent_rows(&connection);
        connection
            .execute_batch(
                "INSERT INTO rooms(id, name, status, created_at, updated_at)
                 VALUES ('room-1', 'room-one', 'active', 'now', 'now');
                 INSERT INTO room_members(room_id, agent_id, role, joined_at)
                 VALUES ('room-1', 'agent-1', 'reviewer', 'joined');
                 INSERT INTO conversation_members(
                     conversation_id, member_type, member_id, joined_at, left_at
                 ) VALUES ('conversation-1', 'agent', 'agent-1', 'joined', 'left');
                 INSERT INTO work_items(
                     id, conversation_id, title, status, created_at, updated_at
                 ) VALUES ('work-1', 'conversation-1', 'legacy work', 'open', 'now', 'now');
                 INSERT INTO work_results(
                     id, work_id, status, summary, created_at
                 ) VALUES ('result-1', 'work-1', 'done', 'kept', 'now');",
            )
            .unwrap();

        apply_migrations(&mut connection, &MIGRATIONS[..3]).unwrap();

        assert_eq!(super::current_schema_version(&connection).unwrap(), 3);
        assert_eq!(
            connection
                .query_row(
                    "SELECT role, generation, joined_at, left_at
                     FROM room_members WHERE room_id = 'room-1'",
                    [],
                    |row| {
                        Ok((
                            row.get::<_, Option<String>>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, Option<String>>(3)?,
                        ))
                    },
                )
                .unwrap(),
            (Some("reviewer".into()), 1, "joined".into(), None)
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT member_type, member_id, generation, joined_at, left_at
                     FROM conversation_members
                     WHERE conversation_id = 'conversation-1'",
                    [],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, Option<String>>(4)?,
                        ))
                    },
                )
                .unwrap(),
            (
                "agent".into(),
                "agent-1".into(),
                1,
                "joined".into(),
                Some("left".into())
            )
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT is_primary FROM work_items WHERE id = 'work-1'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT work_id FROM work_results WHERE id = 'result-1'",
                    [],
                    |row| { row.get::<_, String>(0) }
                )
                .unwrap(),
            "work-1"
        );
    }

    #[test]
    fn migration_four_preserves_legacy_messages_without_deliveries() {
        let database = TestDatabase::new();
        let mut connection = Connection::open(database.path()).unwrap();
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .unwrap();
        apply_migrations(&mut connection, &MIGRATIONS[..3]).unwrap();
        seed_session_parent_rows(&connection);
        connection
            .execute(
                "INSERT INTO messages(
                    id, conversation_id, sender_type, sender_id, body, created_at
                 ) VALUES (
                    'message-legacy', 'conversation-1', 'agent', 'agent-1', 'kept', 'now'
                 )",
                [],
            )
            .unwrap();

        apply_migrations(&mut connection, &MIGRATIONS[..4]).unwrap();

        assert_eq!(super::current_schema_version(&connection).unwrap(), 4);
        assert_eq!(
            connection
                .query_row(
                    "SELECT body FROM messages WHERE id = 'message-legacy'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "kept"
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM message_deliveries", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
    }

    #[test]
    fn migration_five_backfills_and_constrains_dependency_status() {
        let database = TestDatabase::new();
        let mut connection = Connection::open(database.path()).unwrap();
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .unwrap();
        apply_migrations(&mut connection, &MIGRATIONS[..4]).unwrap();
        seed_conversation(&connection);
        connection
            .execute_batch(
                "INSERT INTO work_items(
                    id, conversation_id, title, status, created_at, updated_at
                 ) VALUES
                    ('work-upstream', 'conversation-1', 'prerequisite', 'ready', 'now', 'now'),
                    ('work-downstream', 'conversation-1', 'consumer', 'blocked', 'now', 'now');
                 INSERT INTO work_dependencies(
                    upstream_work_id, downstream_work_id, dependency_type, created_at
                 ) VALUES ('work-upstream', 'work-downstream', 'requires', 'now');",
            )
            .unwrap();

        apply_migrations(&mut connection, &MIGRATIONS[..5]).unwrap();

        assert_eq!(super::current_schema_version(&connection).unwrap(), 5);
        let status: String = connection
            .query_row("SELECT status FROM work_dependencies", [], |row| row.get(0))
            .unwrap();
        assert_eq!(status, "waiting");
        for status in ["waiting", "satisfied", "failed", "superseded"] {
            connection
                .execute("UPDATE work_dependencies SET status = ?1", [status])
                .unwrap();
        }
        assert!(
            connection
                .execute("UPDATE work_dependencies SET status = 'unknown'", [])
                .is_err()
        );
    }

    #[test]
    fn migration_six_repairs_legacy_work_completion_and_guards_raw_writes() {
        let database = TestDatabase::new();
        let mut connection = Connection::open(database.path()).unwrap();
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .unwrap();
        apply_migrations(&mut connection, &MIGRATIONS[..5]).unwrap();
        seed_conversation(&connection);
        connection
            .execute_batch(
                "INSERT INTO work_items(
                    id, conversation_id, title, status, created_at, updated_at, completed_at
                 ) VALUES
                    ('terminal-null', 'conversation-1', 'terminal null', 'done',
                     'created', 'terminal-null-updated', NULL),
                    ('terminal-blank', 'conversation-1', 'terminal blank', 'failed',
                     'created', 'terminal-blank-updated', '  '),
                    ('nonterminal-set', 'conversation-1', 'nonterminal set', 'working',
                     'created', 'nonterminal-updated', 'legacy-completed'),
                    ('valid-terminal', 'conversation-1', 'valid terminal', 'cancelled',
                     'created', 'valid-terminal-updated', 'valid-completed'),
                    ('valid-open', 'conversation-1', 'valid open', 'open',
                     'created', 'valid-open-updated', NULL);",
            )
            .unwrap();

        apply_migrations(&mut connection, &MIGRATIONS[..6]).unwrap();

        assert_eq!(super::current_schema_version(&connection).unwrap(), 6);
        for (id, expected) in [
            ("terminal-null", Some("terminal-null-updated")),
            ("terminal-blank", Some("terminal-blank-updated")),
            ("nonterminal-set", None),
            ("valid-terminal", Some("valid-completed")),
            ("valid-open", None),
        ] {
            let completed_at: Option<String> = connection
                .query_row(
                    "SELECT completed_at FROM work_items WHERE id = ?1",
                    [id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(completed_at.as_deref(), expected, "wrong repair for {id}");
        }

        for (id, status, completed_at) in [
            ("raw-terminal-null", "done", None),
            ("raw-terminal-blank", "failed", Some("  ")),
            ("raw-nonterminal-set", "working", Some("completed")),
        ] {
            assert!(
                connection
                    .execute(
                        "INSERT INTO work_items(
                            id, conversation_id, title, status,
                            created_at, updated_at, completed_at
                         ) VALUES (?1, 'conversation-1', 'raw', ?2, 'created', 'updated', ?3)",
                        params![id, status, completed_at],
                    )
                    .is_err(),
                "accepted invalid raw insert: {id}"
            );
        }
        assert!(
            connection
                .execute(
                    "UPDATE work_items
                     SET status = 'done', completed_at = NULL
                     WHERE id = 'valid-open'",
                    [],
                )
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "UPDATE work_items
                     SET status = 'working'
                     WHERE id = 'valid-terminal'",
                    [],
                )
                .is_err()
        );
    }

    #[test]
    fn work_hydration_rejects_invalid_completion_state() {
        let database = TestDatabase::new();
        let mut connection = Connection::open(database.path()).unwrap();
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .unwrap();
        apply_migrations(&mut connection, &MIGRATIONS[..5]).unwrap();
        let conversation_id = ConversationId::new();
        let work_id = WorkItemId::new();
        connection
            .execute(
                "INSERT INTO conversations(id, type, status, created_at, updated_at)
                 VALUES (?1, 'dm', 'open', 'created', 'updated')",
                [conversation_id.to_string()],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO work_items(
                    id, conversation_id, title, status, created_at, updated_at, completed_at
                 ) VALUES (?1, ?2, 'invalid', 'done', 'created', 'updated', NULL)",
                params![work_id.to_string(), conversation_id.to_string()],
            )
            .unwrap();
        let store = SqliteStore { connection };

        assert!(matches!(
            store.get_work_item(work_id),
            Err(StoreError::Domain(
                DomainError::WorkCompletionTimestampMismatch
            ))
        ));
    }

    #[test]
    fn migration_seven_repairs_and_rejects_rust_whitespace_completion() {
        let database = TestDatabase::new();
        let mut connection = Connection::open(database.path()).unwrap();
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .unwrap();
        apply_migrations(&mut connection, &MIGRATIONS[..6]).unwrap();
        seed_conversation(&connection);
        let rust_whitespace = "\u{0009}\u{000a}\u{000b}\u{000c}\u{000d}\u{0020}\u{0085}\u{00a0}\u{1680}\u{2000}\u{2001}\u{2002}\u{2003}\u{2004}\u{2005}\u{2006}\u{2007}\u{2008}\u{2009}\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}";
        for (id, updated_at, completed_at) in [
            ("terminal-tab", "tab-updated", "\t"),
            ("terminal-newline", "newline-updated", "\n"),
            (
                "terminal-ascii-whitespace",
                "ascii-whitespace-updated",
                "\t\n\u{000b}\u{000c}\r ",
            ),
            (
                "terminal-rust-whitespace",
                "rust-whitespace-updated",
                rust_whitespace,
            ),
            ("valid-surrounded", "valid-updated", "\tcompleted\n"),
        ] {
            connection
                .execute(
                    "INSERT INTO work_items(
                        id, conversation_id, title, status,
                        created_at, updated_at, completed_at
                     ) VALUES (?1, 'conversation-1', 'legacy', 'done', 'created', ?2, ?3)",
                    params![id, updated_at, completed_at],
                )
                .unwrap();
        }

        apply_migrations(&mut connection, &MIGRATIONS[..7]).unwrap();

        assert_eq!(super::current_schema_version(&connection).unwrap(), 7);
        for (id, expected) in [
            ("terminal-tab", "tab-updated"),
            ("terminal-newline", "newline-updated"),
            ("terminal-ascii-whitespace", "ascii-whitespace-updated"),
            ("terminal-rust-whitespace", "rust-whitespace-updated"),
            ("valid-surrounded", "\tcompleted\n"),
        ] {
            let completed_at: String = connection
                .query_row(
                    "SELECT completed_at FROM work_items WHERE id = ?1",
                    [id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(completed_at, expected, "wrong migration value for {id}");
        }

        for (id, completed_at) in [
            ("raw-tab", "\t"),
            ("raw-newline", "\n"),
            ("raw-ascii-whitespace", "\t\n\u{000b}\u{000c}\r "),
            ("raw-rust-whitespace", rust_whitespace),
        ] {
            assert!(
                connection
                    .execute(
                        "INSERT INTO work_items(
                            id, conversation_id, title, status,
                            created_at, updated_at, completed_at
                         ) VALUES (?1, 'conversation-1', 'raw', 'done',
                                   'created', 'updated', ?2)",
                        params![id, completed_at],
                    )
                    .is_err(),
                "accepted whitespace-only completed_at for {id}"
            );
        }
        assert!(
            connection
                .execute(
                    "UPDATE work_items SET completed_at = ?1 WHERE id = 'valid-surrounded'",
                    ["\t\n"],
                )
                .is_err()
        );
    }

    #[test]
    fn migration_eight_preserves_edges_and_adds_result_reference_foreign_key() {
        let database = TestDatabase::new();
        let mut connection = Connection::open(database.path()).unwrap();
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .unwrap();
        apply_migrations(&mut connection, &MIGRATIONS[..7]).unwrap();
        seed_conversation(&connection);
        connection
            .execute_batch(
                "INSERT INTO work_items(
                    id, conversation_id, title, status, created_at, updated_at
                 ) VALUES
                    ('work-upstream', 'conversation-1', 'prerequisite', 'working', 'now', 'now'),
                    ('work-downstream', 'conversation-1', 'consumer', 'blocked', 'now', 'now');
                 INSERT INTO work_dependencies(
                    upstream_work_id, downstream_work_id, dependency_type, status, created_at
                 ) VALUES (
                    'work-upstream', 'work-downstream', 'requires', 'waiting', 'created'
                 );",
            )
            .unwrap();

        apply_migrations(&mut connection, &MIGRATIONS[..8]).unwrap();

        assert_eq!(super::current_schema_version(&connection).unwrap(), 8);
        assert_eq!(
            connection
                .query_row(
                    "SELECT status, result_id, created_at FROM work_dependencies",
                    [],
                    |row| Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?
                    )),
                )
                .unwrap(),
            ("waiting".into(), None, "created".into())
        );
        assert!(
            connection
                .execute(
                    "UPDATE work_dependencies SET result_id = 'missing-result'",
                    [],
                )
                .is_err()
        );
    }

    #[test]
    fn migration_nine_reconciles_dependency_results_and_guards_raw_writes() {
        let database = TestDatabase::new();
        let mut connection = Connection::open(database.path()).unwrap();
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .unwrap();
        apply_migrations(&mut connection, &MIGRATIONS[..8]).unwrap();
        seed_conversation(&connection);
        connection
            .execute_batch(
                "INSERT INTO work_items(
                    id, conversation_id, title, status, created_at, updated_at
                 ) VALUES
                    ('upstream-1', 'conversation-1', 'upstream 1', 'working', 'now', 'now'),
                    ('upstream-2', 'conversation-1', 'upstream 2', 'working', 'now', 'now'),
                    ('upstream-3', 'conversation-1', 'upstream 3', 'working', 'now', 'now'),
                    ('downstream-1', 'conversation-1', 'downstream 1', 'blocked', 'now', 'now'),
                    ('downstream-2', 'conversation-1', 'downstream 2', 'blocked', 'now', 'now'),
                    ('downstream-3', 'conversation-1', 'downstream 3', 'blocked', 'now', 'now');
                 INSERT INTO work_results(id, work_id, status, summary, created_at) VALUES
                    ('result-1', 'upstream-1', 'accepted', 'result 1', 'now'),
                    ('result-3', 'upstream-3', 'accepted', 'result 3', 'now');
                 INSERT INTO work_dependencies(
                    upstream_work_id, downstream_work_id, dependency_type,
                    status, result_id, created_at
                 ) VALUES
                    ('upstream-1', 'downstream-1', 'requires', 'satisfied', 'result-1', 'now'),
                    ('upstream-1', 'downstream-2', 'requires', 'superseded', 'result-1', 'now'),
                    ('upstream-2', 'downstream-1', 'requires', 'satisfied', NULL, 'now'),
                    ('upstream-2', 'downstream-2', 'requires', 'superseded', 'result-1', 'now'),
                    ('upstream-3', 'downstream-1', 'requires', 'waiting', 'result-3', 'now'),
                    ('upstream-3', 'downstream-2', 'requires', 'failed', 'result-3', 'now');",
            )
            .unwrap();

        apply_migrations(&mut connection, &MIGRATIONS[..9]).unwrap();

        assert_eq!(super::current_schema_version(&connection).unwrap(), 9);
        let read = |upstream: &str, downstream: &str| {
            connection
                .query_row(
                    "SELECT status, result_id
                     FROM work_dependencies
                     WHERE upstream_work_id = ?1 AND downstream_work_id = ?2",
                    params![upstream, downstream],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
                )
                .unwrap()
        };
        assert_eq!(
            read("upstream-1", "downstream-1"),
            ("satisfied".into(), Some("result-1".into()))
        );
        assert_eq!(
            read("upstream-1", "downstream-2"),
            ("superseded".into(), Some("result-1".into()))
        );
        for (upstream, downstream) in [
            ("upstream-2", "downstream-1"),
            ("upstream-2", "downstream-2"),
            ("upstream-3", "downstream-1"),
            ("upstream-3", "downstream-2"),
        ] {
            assert_eq!(
                read(upstream, downstream),
                ("waiting".into(), None),
                "mismatch was not normalized: {upstream} -> {downstream}"
            );
        }

        for values in [
            "'satisfied', NULL",
            "'superseded', NULL",
            "'waiting', 'result-1'",
            "'failed', 'result-1'",
            "'satisfied', 'result-3'",
            "'satisfied', 'result-1'",
        ] {
            assert!(
                connection
                    .execute(
                        &format!(
                            "INSERT INTO work_dependencies(
                                upstream_work_id, downstream_work_id, dependency_type,
                                status, result_id, created_at
                             ) VALUES (
                                'upstream-1', 'downstream-3', 'requires', {values}, 'now'
                             )"
                        ),
                        [],
                    )
                    .is_err(),
                "accepted invalid dependency insert: {values}"
            );
        }
        connection
            .execute(
                "INSERT INTO work_dependencies(
                    upstream_work_id, downstream_work_id, dependency_type,
                    status, result_id, created_at
                 ) VALUES (
                    'upstream-1', 'downstream-3', 'requires', 'waiting', NULL, 'now'
                 )",
                [],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE work_dependencies
                 SET status = 'satisfied', result_id = 'result-1'
                 WHERE upstream_work_id = 'upstream-1'
                   AND downstream_work_id = 'downstream-3'",
                [],
            )
            .unwrap();
        for update in [
            "SET result_id = NULL",
            "SET status = 'waiting'",
            "SET status = 'failed'",
            "SET status = 'superseded', result_id = NULL",
            "SET result_id = 'result-3'",
            "SET upstream_work_id = 'upstream-2'",
        ] {
            assert!(
                connection
                    .execute(
                        &format!(
                            "UPDATE work_dependencies {update}
                             WHERE upstream_work_id = 'upstream-1'
                               AND downstream_work_id = 'downstream-3'"
                        ),
                        [],
                    )
                    .is_err(),
                "accepted invalid dependency update: {update}"
            );
        }
        assert_eq!(
            read("upstream-1", "downstream-3"),
            ("satisfied".into(), Some("result-1".into()))
        );
        assert!(
            connection
                .execute(
                    "UPDATE work_results SET work_id = 'upstream-3' WHERE id = 'result-1'",
                    [],
                )
                .is_err()
        );
    }

    #[test]
    fn migration_ten_reconciles_phase_six_references_and_preserves_valid_rows() {
        let database = TestDatabase::new();
        let mut connection = Connection::open(database.path()).unwrap();
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .unwrap();
        apply_migrations(&mut connection, &MIGRATIONS[..9]).unwrap();
        seed_conversation(&connection);
        connection
            .execute_batch(
                "INSERT INTO conversations(id, type, status, created_at, updated_at)
                 VALUES ('conversation-2', 'dm', 'open', 'now', 'now');
                 INSERT INTO work_items(
                    id, conversation_id, title, status, created_at, updated_at
                 ) VALUES
                    ('ready-work', 'conversation-1', 'ready', 'ready', 'now', 'now'),
                    ('working-work', 'conversation-1', 'working', 'working', 'now', 'now'),
                    ('other-work', 'conversation-2', 'other', 'ready', 'now', 'now'),
                    ('downstream-1', 'conversation-2', 'downstream 1', 'blocked', 'now', 'now'),
                    ('downstream-2', 'conversation-2', 'downstream 2', 'blocked', 'now', 'now'),
                    ('downstream-3', 'conversation-2', 'downstream 3', 'blocked', 'now', 'now');
                 INSERT INTO work_results(
                    id, work_id, status, summary, supersedes_result_id, created_at
                 ) VALUES
                    ('prior-result', 'ready-work', 'accepted', 'prior', NULL, 'now'),
                    ('valid-result', 'ready-work', 'accepted', 'valid', 'prior-result', 'now'),
                    ('working-result', 'working-work', 'accepted', 'working', NULL, 'now'),
                    ('self-result', 'ready-work', 'accepted', 'self', 'self-result', 'now'),
                    ('cross-result', 'other-work', 'accepted', 'cross', 'valid-result', 'now');
                 INSERT INTO publishes(
                    id, result_id, source_conversation_id, target_conversation_id, created_at
                 ) VALUES
                    ('valid-publish', 'valid-result', 'conversation-1', 'conversation-2', 'now'),
                    ('false-publish', 'working-result', 'conversation-2', 'conversation-1', 'now');
                 INSERT INTO work_dependencies(
                    upstream_work_id, downstream_work_id, dependency_type,
                    status, result_id, created_at
                 ) VALUES
                    ('ready-work', 'downstream-1', 'requires', 'waiting', NULL, 'now'),
                    ('working-work', 'downstream-2', 'requires', 'waiting', NULL, 'now'),
                    ('other-work', 'downstream-3', 'requires', 'waiting', NULL, 'now');
                 UPDATE work_dependencies
                 SET status = 'satisfied', result_id = 'valid-result'
                 WHERE upstream_work_id = 'ready-work';
                 UPDATE work_dependencies
                 SET status = 'superseded', result_id = 'working-result'
                 WHERE upstream_work_id = 'working-work';
                 DROP TRIGGER work_dependency_result_update_guard;
                 UPDATE work_dependencies
                 SET status = 'satisfied', result_id = 'valid-result'
                 WHERE upstream_work_id = 'other-work';
                 CREATE TRIGGER work_dependency_result_update_guard
                 BEFORE UPDATE OF upstream_work_id, status, result_id ON work_dependencies
                 WHEN NOT (
                    (NEW.status IN ('waiting', 'failed') AND NEW.result_id IS NULL)
                    OR (
                        NEW.status IN ('satisfied', 'superseded')
                        AND NEW.result_id IS NOT NULL
                        AND EXISTS (
                            SELECT 1 FROM work_results
                            WHERE work_results.id = NEW.result_id
                              AND work_results.work_id = NEW.upstream_work_id
                        )
                    )
                 )
                 BEGIN
                    SELECT RAISE(ABORT, 'work dependency result does not match status or upstream work');
                 END;",
            )
            .unwrap();

        apply_migrations(&mut connection, &MIGRATIONS).unwrap();

        assert_eq!(super::current_schema_version(&connection).unwrap(), 20);
        for (id, expected) in [
            ("valid-result", Some("prior-result")),
            ("self-result", None),
            ("cross-result", None),
        ] {
            assert_eq!(
                connection
                    .query_row(
                        "SELECT supersedes_result_id FROM work_results WHERE id = ?1",
                        [id],
                        |row| row.get::<_, Option<String>>(0),
                    )
                    .unwrap()
                    .as_deref(),
                expected
            );
        }
        assert_eq!(
            connection
                .query_row(
                    "SELECT source_conversation_id FROM publishes WHERE id = 'valid-publish'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "conversation-1"
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT source_conversation_id FROM publishes WHERE id = 'false-publish'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "conversation-1"
        );
        let dependency = |upstream: &str| {
            connection
                .query_row(
                    "SELECT status, result_id FROM work_dependencies WHERE upstream_work_id = ?1",
                    [upstream],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
                )
                .unwrap()
        };
        assert_eq!(
            dependency("ready-work"),
            ("satisfied".into(), Some("valid-result".into()))
        );
        for upstream in ["working-work", "other-work"] {
            assert_eq!(dependency(upstream), ("waiting".into(), None));
        }
    }

    #[test]
    fn migration_ten_guards_raw_phase_six_mutations() {
        let database = TestDatabase::new();
        let store = SqliteStore::open(database.path()).unwrap();
        seed_conversation(&store.connection);
        store
            .connection
            .execute_batch(
                "INSERT INTO conversations(id, type, status, created_at, updated_at)
                 VALUES ('conversation-2', 'dm', 'open', 'now', 'now');
                 INSERT INTO work_items(
                    id, conversation_id, title, status, created_at, updated_at
                 ) VALUES
                    ('ready-work', 'conversation-1', 'ready', 'ready', 'now', 'now'),
                    ('working-work', 'conversation-1', 'working', 'working', 'now', 'now'),
                    ('other-work', 'conversation-2', 'other', 'ready', 'now', 'now'),
                    ('downstream-1', 'conversation-2', 'downstream 1', 'blocked', 'now', 'now'),
                    ('downstream-2', 'conversation-2', 'downstream 2', 'blocked', 'now', 'now');
                 INSERT INTO work_results(id, work_id, status, summary, created_at) VALUES
                    ('prior-result', 'ready-work', 'accepted', 'prior', 'now'),
                    ('delete-result', 'ready-work', 'accepted', 'delete', 'now'),
                    ('working-result', 'working-work', 'accepted', 'working', 'now');",
            )
            .unwrap();

        assert!(
            store
                .connection
                .execute(
                    "UPDATE work_results SET summary = 'changed' WHERE id = 'prior-result'",
                    [],
                )
                .is_err()
        );
        assert!(
            store
                .connection
                .execute("DELETE FROM work_results WHERE id = 'delete-result'", [])
                .is_err()
        );
        for (id, work_id, supersedes) in [
            ("self-result", "ready-work", "self-result"),
            ("cross-result", "other-work", "prior-result"),
            ("missing-result", "ready-work", "absent-result"),
        ] {
            assert!(
                store
                    .connection
                    .execute(
                        "INSERT INTO work_results(
                            id, work_id, status, summary, supersedes_result_id, created_at
                         ) VALUES (?1, ?2, 'accepted', 'invalid', ?3, 'now')",
                        params![id, work_id, supersedes],
                    )
                    .is_err(),
                "accepted invalid supersede {id}"
            );
        }
        store
            .connection
            .execute(
                "INSERT INTO work_results(
                    id, work_id, status, summary, supersedes_result_id, created_at
                 ) VALUES ('valid-result', 'ready-work', 'accepted', 'valid', 'prior-result', 'now')",
                [],
            )
            .unwrap();

        assert!(
            store
                .connection
                .execute(
                    "INSERT INTO publishes(
                        id, result_id, source_conversation_id, target_conversation_id, created_at
                     ) VALUES (
                        'false-publish', 'valid-result', 'conversation-2', 'conversation-1', 'now'
                     )",
                    [],
                )
                .is_err()
        );
        store
            .connection
            .execute(
                "INSERT INTO publishes(
                    id, result_id, source_conversation_id, target_conversation_id, created_at
                 ) VALUES (
                    'valid-publish', 'valid-result', 'conversation-1', 'conversation-2', 'now'
                 )",
                [],
            )
            .unwrap();
        assert!(
            store
                .connection
                .execute(
                    "UPDATE publishes SET source_conversation_id = 'conversation-2'
                     WHERE id = 'valid-publish'",
                    [],
                )
                .is_err()
        );

        store
            .connection
            .execute_batch(
                "INSERT INTO work_dependencies(
                    upstream_work_id, downstream_work_id, dependency_type,
                    status, result_id, created_at
                 ) VALUES
                    ('ready-work', 'downstream-1', 'requires', 'waiting', NULL, 'now'),
                    ('working-work', 'downstream-2', 'requires', 'waiting', NULL, 'now');
                 UPDATE work_dependencies
                 SET status = 'satisfied', result_id = 'valid-result'
                 WHERE upstream_work_id = 'ready-work';",
            )
            .unwrap();
        assert!(
            store
                .connection
                .execute(
                    "UPDATE work_dependencies
                     SET status = 'satisfied', result_id = 'working-result'
                     WHERE upstream_work_id = 'working-work'",
                    [],
                )
                .is_err()
        );
        assert!(
            store
                .connection
                .execute(
                    "UPDATE work_items SET status = 'working' WHERE id = 'ready-work'",
                    [],
                )
                .is_err()
        );
    }

    #[test]
    fn failed_migration_ten_leaves_version_nine_without_partial_guards_or_repairs() {
        let database = TestDatabase::new();
        let mut connection = Connection::open(database.path()).unwrap();
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .unwrap();
        apply_migrations(&mut connection, &MIGRATIONS[..9]).unwrap();
        seed_conversation(&connection);
        connection
            .execute_batch(
                "INSERT INTO work_items(id, conversation_id, title, status, created_at, updated_at)
                 VALUES ('work-1', 'conversation-1', 'work', 'ready', 'now', 'now');
                 INSERT INTO work_results(
                    id, work_id, status, summary, supersedes_result_id, created_at
                 ) VALUES ('self-result', 'work-1', 'accepted', 'self', 'self-result', 'now');
                 CREATE TRIGGER publishes_source_insert_guard
                 BEFORE INSERT ON publishes BEGIN SELECT 1; END;",
            )
            .unwrap();

        assert!(apply_migrations(&mut connection, &MIGRATIONS).is_err());

        assert_eq!(super::current_schema_version(&connection).unwrap(), 9);
        assert_eq!(
            connection
                .query_row(
                    "SELECT supersedes_result_id FROM work_results WHERE id = 'self-result'",
                    [],
                    |row| row.get::<_, Option<String>>(0),
                )
                .unwrap()
                .as_deref(),
            Some("self-result")
        );
        for trigger in ["work_results_no_update", "work_results_no_delete"] {
            assert_eq!(
                connection
                    .query_row(
                        "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'trigger' AND name = ?1",
                        [trigger],
                        |row| row.get::<_, i64>(0),
                    )
                    .unwrap(),
                0,
                "migration leaked trigger {trigger}"
            );
        }
    }

    #[test]
    fn work_result_and_publish_hydration_validate_domain_records() {
        let database = TestDatabase::new();
        let mut connection = Connection::open(database.path()).unwrap();
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .unwrap();
        apply_migrations(&mut connection, &MIGRATIONS[..9]).unwrap();
        let conversation_id = ConversationId::new();
        let target_id = ConversationId::new();
        let work_id = WorkItemId::new();
        let invalid_result_id = ResultId::new();
        let valid_result_id = ResultId::new();
        let publish_id = PublishId::new();
        connection
            .execute_batch(&format!(
                "INSERT INTO conversations(id, type, status, created_at, updated_at) VALUES
                    ('{conversation_id}', 'dm', 'open', 'now', 'now'),
                    ('{target_id}', 'dm', 'open', 'now', 'now');
                 INSERT INTO work_items(id, conversation_id, title, status, created_at, updated_at)
                 VALUES ('{work_id}', '{conversation_id}', 'work', 'ready', 'now', 'now');
                 INSERT INTO work_results(id, work_id, status, summary, created_at) VALUES
                    ('{invalid_result_id}', '{work_id}', char(160), 'invalid', 'now'),
                    ('{valid_result_id}', '{work_id}', 'accepted', 'valid', 'now');
                 INSERT INTO publishes(
                    id, result_id, source_conversation_id, target_conversation_id, created_at
                 ) VALUES (
                    '{publish_id}', '{valid_result_id}', '{conversation_id}', '{target_id}', char(160)
                 );"
            ))
            .unwrap();
        let store = SqliteStore { connection };

        assert!(matches!(
            store.get_work_result(invalid_result_id),
            Err(StoreError::Domain(DomainError::EmptyField(
                "work_result.status"
            )))
        ));
        assert!(matches!(
            store.get_publish(publish_id),
            Err(StoreError::Domain(DomainError::EmptyField(
                "publish.created_at"
            )))
        ));
    }

    #[test]
    fn dependency_hydration_rejects_invalid_status_or_cross_work_result() {
        let database = TestDatabase::new();
        let mut connection = Connection::open(database.path()).unwrap();
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .unwrap();
        apply_migrations(&mut connection, &MIGRATIONS[..8]).unwrap();
        let conversation_id = ConversationId::new();
        let upstream_id = WorkItemId::new();
        let other_work_id = WorkItemId::new();
        let downstream_id = WorkItemId::new();
        let other_result_id = ResultId::new();
        connection
            .execute(
                "INSERT INTO conversations(id, type, status, created_at, updated_at)
                 VALUES (?1, 'dm', 'open', 'now', 'now')",
                [conversation_id.to_string()],
            )
            .unwrap();
        for (id, title, status) in [
            (upstream_id, "upstream", "working"),
            (other_work_id, "other", "working"),
            (downstream_id, "downstream", "blocked"),
        ] {
            connection
                .execute(
                    "INSERT INTO work_items(
                        id, conversation_id, title, status, created_at, updated_at
                     ) VALUES (?1, ?2, ?3, ?4, 'now', 'now')",
                    params![id.to_string(), conversation_id.to_string(), title, status],
                )
                .unwrap();
        }
        connection
            .execute(
                "INSERT INTO work_results(id, work_id, status, summary, created_at)
                 VALUES (?1, ?2, 'accepted', 'other result', 'now')",
                params![other_result_id.to_string(), other_work_id.to_string()],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO work_dependencies(
                    upstream_work_id, downstream_work_id, dependency_type,
                    status, result_id, created_at
                 ) VALUES (?1, ?2, 'requires', 'satisfied', NULL, 'now')",
                params![upstream_id.to_string(), downstream_id.to_string()],
            )
            .unwrap();
        let store = SqliteStore { connection };

        assert!(matches!(
            store.get_work_dependency(upstream_id, downstream_id),
            Err(StoreError::Domain(
                DomainError::DependencyResultStatusMismatch
            ))
        ));
        store
            .connection
            .execute(
                "UPDATE work_dependencies SET result_id = ?3
                 WHERE upstream_work_id = ?1 AND downstream_work_id = ?2",
                params![
                    upstream_id.to_string(),
                    downstream_id.to_string(),
                    other_result_id.to_string(),
                ],
            )
            .unwrap();
        assert!(matches!(
            store.list_work_dependency_outcomes_for_downstream(downstream_id),
            Err(StoreError::InvalidStoredValue(
                "work dependency result reference"
            ))
        ));
    }

    #[test]
    fn delivery_schema_rejects_invalid_state_and_progress() {
        let database = TestDatabase::new();
        let store = SqliteStore::open(database.path()).unwrap();
        seed_session_parent_rows(&store.connection);
        store
            .connection
            .execute(
                "INSERT INTO messages(
                    id, conversation_id, sender_type, sender_id, body, created_at
                 ) VALUES (
                    'message-1', 'conversation-1', 'agent', 'agent-1', 'hello', 'now'
                 )",
                [],
            )
            .unwrap();

        for values in [
            "'unknown', NULL, NULL, 'now', 'now', NULL",
            "'pending', '', NULL, 'now', 'now', NULL",
            "'pending', NULL, 'capsule-sent', 'now', 'now', NULL",
            "'pending', NULL, NULL, '', 'now', NULL",
            "'pending', NULL, NULL, 'now', '', NULL",
            "'pending', NULL, NULL, 'now', 'now', 'delivered'",
            "'delivered', NULL, NULL, 'now', 'now', NULL",
            "'failed', 'capsule', '', 'now', 'now', NULL",
        ] {
            assert!(
                store
                    .connection
                    .execute(
                        &format!(
                            "INSERT INTO message_deliveries(
                                message_id, target_agent_id, status, capsule,
                                capsule_delivered_at, created_at, updated_at, delivered_at
                             ) VALUES ('message-1', 'agent-1', {values})"
                        ),
                        [],
                    )
                    .is_err(),
                "accepted invalid delivery values: {values}"
            );
        }
    }

    #[test]
    fn phase_four_membership_and_primary_work_constraints_are_enforced() {
        let database = TestDatabase::new();
        let store = SqliteStore::open(database.path()).unwrap();
        seed_session_parent_rows(&store.connection);
        store
            .connection
            .execute_batch(
                "INSERT INTO rooms(id, name, status, created_at, updated_at)
                 VALUES ('room-1', 'room-one', 'active', 'now', 'now');
                 INSERT INTO room_members(
                     room_id, agent_id, role, generation, joined_at, left_at
                 ) VALUES ('room-1', 'agent-1', NULL, 1, 'joined-1', 'left-1');
                 INSERT INTO room_members(
                     room_id, agent_id, role, generation, joined_at, left_at
                 ) VALUES ('room-1', 'agent-1', NULL, 2, 'joined-2', NULL);
                 INSERT INTO conversation_members(
                     conversation_id, member_type, member_id, generation, joined_at, left_at
                 ) VALUES ('conversation-1', 'agent', 'agent-1', 1, 'joined-1', 'left-1');
                 INSERT INTO conversation_members(
                     conversation_id, member_type, member_id, generation, joined_at, left_at
                 ) VALUES ('conversation-1', 'agent', 'agent-1', 2, 'joined-2', NULL);
                 INSERT INTO work_items(
                     id, conversation_id, title, status, is_primary, created_at, updated_at
                 ) VALUES ('work-1', 'conversation-1', 'primary', 'open', 1, 'now', 'now');",
            )
            .unwrap();

        for statement in [
            "INSERT INTO room_members(room_id, agent_id, generation, joined_at)
             VALUES ('room-1', 'agent-1', 3, 'joined-3')",
            "INSERT INTO conversation_members(
                 conversation_id, member_type, member_id, generation, joined_at
             ) VALUES ('conversation-1', 'agent', 'agent-1', 3, 'joined-3')",
            "INSERT INTO work_items(
                 id, conversation_id, title, status, is_primary, created_at, updated_at
             ) VALUES ('work-2', 'conversation-1', 'second', 'open', 1, 'now', 'now')",
        ] {
            assert!(store.connection.execute(statement, []).is_err());
        }
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
                 INSERT INTO schema_migrations(version) VALUES (21);",
            )
            .unwrap();
        drop(connection);

        match SqliteStore::open(database.path()) {
            Err(StoreError::DatabaseTooNew {
                found: 21,
                supported: 20,
            }) => {}
            Err(error) => panic!("unexpected error: {error}"),
            Ok(_) => panic!("newer database was accepted"),
        }
    }

    #[test]
    fn session_recovery_constraints_and_one_way_progress_are_enforced() {
        let database = TestDatabase::new();
        let store = SqliteStore::open(database.path()).unwrap();
        seed_session_parent_rows(&store.connection);
        insert_raw_binding(&store.connection, "source-1", 1, "lost");
        insert_raw_binding(&store.connection, "replacement-1", 2, "disconnected");
        insert_raw_binding(&store.connection, "source-2", 3, "lost");
        insert_raw_binding(&store.connection, "replacement-2", 4, "lost");

        assert!(
            store
                .connection
                .execute(
                    "INSERT INTO session_recoveries(
                        session_binding_id, source_binding_id, capsule, created_at
                     ) VALUES (NULL, 'source-2', 'capsule', 'created')",
                    [],
                )
                .is_err()
        );
        store
            .connection
            .execute(
                "INSERT INTO session_recoveries(
                    session_binding_id, source_binding_id, capsule, created_at
                 ) VALUES ('replacement-1', 'source-1', 'capsule', 'created')",
                [],
            )
            .unwrap();
        assert!(
            store
                .connection
                .execute(
                    "INSERT INTO session_recoveries(
                        session_binding_id, source_binding_id, capsule, created_at
                     ) VALUES ('source-2', 'source-2', 'capsule', 'created')",
                    [],
                )
                .is_err()
        );
        assert!(
            store
                .connection
                .execute(
                    "INSERT INTO session_recoveries(
                        session_binding_id, source_binding_id, capsule, created_at
                     ) VALUES ('replacement-2', 'source-2', ' ', 'created')",
                    [],
                )
                .is_err()
        );
        for statement in [
            "UPDATE session_recoveries SET source_binding_id = 'source-2'",
            "UPDATE session_recoveries SET capsule = 'changed'",
            "UPDATE session_recoveries SET created_at = 'changed'",
            "UPDATE session_recoveries SET capsule_delivered_at = ' '",
        ] {
            assert!(
                store.connection.execute(statement, []).is_err(),
                "{statement}"
            );
        }
        store
            .connection
            .execute(
                "UPDATE session_recoveries SET capsule_delivered_at = 'delivered'",
                [],
            )
            .unwrap();
        assert!(
            store
                .connection
                .execute(
                    "UPDATE session_recoveries SET capsule_delivered_at = 'later'",
                    [],
                )
                .is_err()
        );
        assert!(
            store
                .connection
                .execute(
                    "UPDATE session_recoveries SET capsule_delivered_at = NULL",
                    [],
                )
                .is_err()
        );

        let index_count: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_index_list('session_recoveries')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(index_count, 2);
    }

    #[test]
    fn failed_migration_eleven_rolls_back_table_and_version() {
        let database = TestDatabase::new();
        let mut connection = Connection::open(database.path()).unwrap();
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .unwrap();
        apply_migrations(&mut connection, &MIGRATIONS[..10]).unwrap();
        connection
            .execute_batch(
                "CREATE TRIGGER session_recoveries_update_guard
                 BEFORE UPDATE ON agents BEGIN SELECT 1; END;",
            )
            .unwrap();

        assert!(apply_migrations(&mut connection, &MIGRATIONS).is_err());
        assert_eq!(super::current_schema_version(&connection).unwrap(), 10);
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema
                     WHERE type = 'table' AND name = 'session_recoveries'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
    }

    #[test]
    fn migration_seventeen_preserves_bindings_permissions_and_recoveries() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .unwrap();
        apply_migrations(&mut connection, &MIGRATIONS[..16]).unwrap();
        seed_session_parent_rows(&connection);
        insert_raw_binding(&connection, "source", 1, "lost");
        insert_raw_binding(&connection, "replacement", 2, "active");
        connection.execute_batch("INSERT INTO permission_decisions VALUES ('decision', 'source', 'request', '[]', 'cancelled', NULL, 'now');
            INSERT INTO session_recoveries VALUES ('replacement', 'source', 'private capsule', 'delivered', 'now');").unwrap();
        apply_migrations(&mut connection, &MIGRATIONS).unwrap();
        assert_eq!(connection.query_row("SELECT count(*) FROM session_bindings WHERE conversation_id = 'conversation-1' AND room_id IS NULL", [], |row| row.get::<_,i64>(0)).unwrap(), 2);
        assert_eq!(
            connection
                .query_row("SELECT capsule FROM session_recoveries", [], |row| row
                    .get::<_, String>(
                    0
                ))
                .unwrap(),
            "private capsule"
        );
        assert_eq!(
            connection
                .query_row("SELECT outcome FROM permission_decisions", [], |row| row
                    .get::<_, String>(
                    0
                ))
                .unwrap(),
            "cancelled"
        );
        assert!(
            !connection
                .prepare("PRAGMA foreign_key_check")
                .unwrap()
                .query([])
                .unwrap()
                .next()
                .unwrap()
                .is_some()
        );
        for sql in [
            "UPDATE permission_decisions SET outcome = 'selected'",
            "DELETE FROM permission_decisions",
            "UPDATE session_recoveries SET capsule = 'changed'",
            "UPDATE session_bindings SET conversation_id = NULL WHERE id = 'source'",
        ] {
            assert!(connection.execute(sql, []).is_err(), "{sql}");
        }
        connection.execute_batch("INSERT INTO rooms VALUES ('room', 'VNA', NULL, 'active', 'now', 'now');
            INSERT INTO session_bindings(id, room_id, agent_id, transport_type, generation, status, created_at, last_used_at)
            VALUES ('room-binding', 'room', 'agent-1', 'acp', 1, 'active', 'now', 'now');").unwrap();
        assert!(connection.execute("UPDATE session_bindings SET conversation_id = 'conversation-1' WHERE id = 'room-binding'", []).is_err());
        assert!(connection.execute("INSERT INTO session_bindings(id, room_id, agent_id, transport_type, generation, status, created_at, last_used_at) VALUES ('duplicate', 'room', 'agent-1', 'acp', 2, 'disconnected', 'now', 'now')", []).is_err());
    }

    #[test]
    fn failed_migration_seventeen_restores_original_tables_and_triggers() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .unwrap();
        apply_migrations(&mut connection, &MIGRATIONS[..16]).unwrap();
        seed_session_parent_rows(&connection);
        insert_raw_binding(&connection, "source", 1, "active");
        connection.execute_batch("INSERT INTO permission_decisions VALUES ('decision', 'source', 'request', '[]', 'cancelled', NULL, 'now');
            CREATE TABLE room_message_activations (existing TEXT);").unwrap();
        assert!(apply_migrations(&mut connection, &MIGRATIONS).is_err());
        assert_eq!(super::current_schema_version(&connection).unwrap(), 16);
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM session_bindings WHERE id = 'source'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        assert!(
            connection
                .execute("DELETE FROM permission_decisions", [])
                .is_err()
        );
        assert!(
            !connection
                .prepare("PRAGMA foreign_key_check")
                .unwrap()
                .query([])
                .unwrap()
                .next()
                .unwrap()
                .is_some()
        );
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
