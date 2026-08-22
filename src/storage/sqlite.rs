use super::{StoreError, records};
use crate::domain::{
    Agent, AgentId, Checkpoint, CheckpointId, Conversation, ConversationId, ConversationKind,
    ConversationMember, DeliveryStatus, MemberType, Memory, MemoryId, Message, MessageDelivery,
    MessageId, PermissionDecision, PermissionOutcome, Publish, PublishId, ResultId, Room, RoomId,
    RoomMember, SessionBinding, SessionBindingId, SessionBindingStatus, WorkDependency, WorkItem,
    WorkItemId, WorkResult, WorkStatus,
};
use rusqlite::{Connection, Params, Row, TransactionBehavior, params};
use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

const BUSY_TIMEOUT_MS: u64 = 5_000;
const MIGRATIONS: [Migration; 5] = [
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
        query_optional(
            &self.connection,
            "SELECT id, type, room_id, title, goal, parent_conversation_id,
                    origin_conversation_id, status, created_at, updated_at
             FROM conversations WHERE id = ?1",
            params![id.to_string()],
            records::conversation,
        )
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
            conversation_id: thread.id,
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

    pub fn insert_work_item(&self, work_item: &WorkItem) -> Result<(), StoreError> {
        insert_work_item(&self.connection, work_item)
    }

    pub fn get_work_item(&self, id: WorkItemId) -> Result<Option<WorkItem>, StoreError> {
        get_work_item(&self.connection, id)
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
        let mut work = require_work_item(&transaction, work_id)?;
        if work.owner_agent_id == Some(owner_agent_id) {
            transaction.commit()?;
            return Ok(work);
        }
        if work.status.is_terminal() {
            return Err(StoreError::TerminalWorkOwnerImmutable(work_id));
        }
        require_active_agent(&transaction, owner_agent_id)?;
        require_active_conversation_membership(
            &transaction,
            work.conversation_id,
            work_id,
            owner_agent_id,
        )?;
        work.owner_agent_id = Some(owner_agent_id);
        work.updated_at = assigned_at.into();
        work.validate()?;
        transaction.execute(
            "UPDATE work_items SET owner_agent_id = ?2, updated_at = ?3 WHERE id = ?1",
            params![work_id.to_string(), owner_agent_id.to_string(), assigned_at],
        )?;
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
        if !work.status.can_transition_to(target) {
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
        transaction.commit()?;
        Ok(work)
    }

    pub fn insert_work_dependency(&self, dependency: &WorkDependency) -> Result<(), StoreError> {
        dependency.validate()?;
        self.connection.execute(
            "INSERT INTO work_dependencies(
                upstream_work_id, downstream_work_id, dependency_type, status, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                dependency.upstream_work_id.to_string(),
                dependency.downstream_work_id.to_string(),
                dependency.dependency_type.to_string(),
                dependency.status.to_string(),
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
            "SELECT upstream_work_id, downstream_work_id, dependency_type, status, created_at
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
        Ok(self.get_session_binding(binding_id)?.is_some())
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
                is_primary, created_at, updated_at, completed_at
         FROM work_items WHERE id = ?1",
        params![work_id.to_string()],
        records::work_item,
    )
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
    connection.execute(
        "INSERT INTO work_items(
            id, conversation_id, title, goal, status, owner_agent_id,
            is_primary, created_at, updated_at, completed_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            work_item.id.to_string(),
            work_item.conversation_id.to_string(),
            work_item.title,
            work_item.goal,
            work_item.status.to_string(),
            work_item.owner_agent_id.map(|id| id.to_string()),
            work_item.is_primary,
            work_item.created_at,
            work_item.updated_at,
            work_item.completed_at,
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
    fn fresh_database_has_schema_version_five() {
        let database = TestDatabase::new();
        let store = SqliteStore::open(database.path()).expect("open fresh database");

        assert_eq!(store.schema_version().unwrap(), 5);
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
            "message_deliveries",
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
            "message_deliveries",
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

        assert_eq!(foreign_key_count, 27);
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
            5
        );
        assert_eq!(
            SqliteStore::open(database.path())
                .unwrap()
                .schema_version()
                .unwrap(),
            5
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

        apply_migrations(&mut connection, &MIGRATIONS).unwrap();

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
                 INSERT INTO schema_migrations(version) VALUES (6);",
            )
            .unwrap();
        drop(connection);

        match SqliteStore::open(database.path()) {
            Err(StoreError::DatabaseTooNew {
                found: 6,
                supported: 5,
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
