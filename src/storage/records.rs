use super::StoreError;
use crate::domain::{
    Agent, Checkpoint, Conversation, ConversationMember, DomainError, Memory, Message,
    MessageDelivery, PermissionDecision, PermissionOption, PermissionOutcome, Publish, Room,
    RoomMember, SessionBinding, WorkDependency, WorkItem, WorkResult,
};
use rusqlite::Row;
use serde_json::Value;
use std::str::FromStr;
use ulid::DecodeError;

fn id<T>(value: String) -> Result<T, StoreError>
where
    T: FromStr<Err = DecodeError>,
{
    Ok(value.parse()?)
}

fn optional_id<T>(value: Option<String>) -> Result<Option<T>, StoreError>
where
    T: FromStr<Err = DecodeError>,
{
    value.map(id).transpose()
}

fn domain_enum<T>(value: String) -> Result<T, StoreError>
where
    T: FromStr<Err = DomainError>,
{
    Ok(value.parse()?)
}

fn json_value(value: String) -> Result<Value, StoreError> {
    Ok(serde_json::from_str(&value)?)
}

fn string_vec(value: String) -> Result<Vec<String>, StoreError> {
    Ok(serde_json::from_str(&value)?)
}

pub(super) fn agent(row: &Row<'_>) -> Result<Agent, StoreError> {
    Ok(Agent {
        id: id(row.get(0)?)?,
        name: row.get(1)?,
        project_root: row.get(2)?,
        transport_type: row.get(3)?,
        transport_config: json_value(row.get(4)?)?,
        status: row.get(5)?,
        metadata: json_value(row.get(6)?)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

pub(super) fn room(row: &Row<'_>) -> Result<Room, StoreError> {
    Ok(Room {
        id: id(row.get(0)?)?,
        name: row.get(1)?,
        description: row.get(2)?,
        status: row.get(3)?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
    })
}

pub(super) fn room_member(row: &Row<'_>) -> Result<RoomMember, StoreError> {
    let generation: i64 = row.get(3)?;
    Ok(RoomMember {
        room_id: id(row.get(0)?)?,
        agent_id: id(row.get(1)?)?,
        role: row.get(2)?,
        generation: generation
            .try_into()
            .map_err(|_| StoreError::IntegerOutOfRange {
                field: "room_members.generation",
                value: i128::from(generation),
            })?,
        joined_at: row.get(4)?,
        left_at: row.get(5)?,
    })
}

pub(super) fn conversation(row: &Row<'_>) -> Result<Conversation, StoreError> {
    Ok(Conversation {
        id: id(row.get(0)?)?,
        kind: domain_enum(row.get(1)?)?,
        room_id: optional_id(row.get(2)?)?,
        title: row.get(3)?,
        goal: row.get(4)?,
        parent_conversation_id: optional_id(row.get(5)?)?,
        origin_conversation_id: optional_id(row.get(6)?)?,
        status: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

pub(super) fn conversation_member(row: &Row<'_>) -> Result<ConversationMember, StoreError> {
    let generation: i64 = row.get(3)?;
    Ok(ConversationMember {
        conversation_id: id(row.get(0)?)?,
        member_type: domain_enum(row.get(1)?)?,
        member_id: row.get(2)?,
        generation: generation
            .try_into()
            .map_err(|_| StoreError::IntegerOutOfRange {
                field: "conversation_members.generation",
                value: i128::from(generation),
            })?,
        joined_at: row.get(4)?,
        left_at: row.get(5)?,
    })
}

pub(super) fn message(row: &Row<'_>) -> Result<Message, StoreError> {
    let metadata: Option<String> = row.get(6)?;
    Ok(Message {
        id: id(row.get(0)?)?,
        conversation_id: id(row.get(1)?)?,
        sender_type: domain_enum(row.get(2)?)?,
        sender_id: row.get(3)?,
        body: row.get(4)?,
        reply_to: optional_id(row.get(5)?)?,
        metadata: metadata.map(json_value).transpose()?.unwrap_or(Value::Null),
        created_at: row.get(7)?,
    })
}

pub(super) fn message_delivery(row: &Row<'_>) -> Result<MessageDelivery, StoreError> {
    Ok(MessageDelivery {
        message_id: id(row.get(0)?)?,
        target_agent_id: id(row.get(1)?)?,
        status: domain_enum(row.get(2)?)?,
        capsule: row.get(3)?,
        capsule_delivered_at: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
        delivered_at: row.get(7)?,
    })
}

pub(super) fn work_item(row: &Row<'_>) -> Result<WorkItem, StoreError> {
    Ok(WorkItem {
        id: id(row.get(0)?)?,
        conversation_id: id(row.get(1)?)?,
        title: row.get(2)?,
        goal: row.get(3)?,
        status: domain_enum(row.get(4)?)?,
        owner_agent_id: optional_id(row.get(5)?)?,
        is_primary: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
        completed_at: row.get(9)?,
    })
}

pub(super) fn work_dependency(row: &Row<'_>) -> Result<WorkDependency, StoreError> {
    Ok(WorkDependency {
        upstream_work_id: id(row.get(0)?)?,
        downstream_work_id: id(row.get(1)?)?,
        dependency_type: domain_enum(row.get(2)?)?,
        status: domain_enum(row.get(3)?)?,
        created_at: row.get(4)?,
    })
}

pub(super) fn work_result(row: &Row<'_>) -> Result<WorkResult, StoreError> {
    Ok(WorkResult {
        id: id(row.get(0)?)?,
        work_id: id(row.get(1)?)?,
        status: row.get(2)?,
        summary: row.get(3)?,
        outputs: string_vec(row.get(4)?)?,
        evidence: string_vec(row.get(5)?)?,
        supersedes_result_id: optional_id(row.get(6)?)?,
        created_at: row.get(7)?,
    })
}

pub(super) fn publish(row: &Row<'_>) -> Result<Publish, StoreError> {
    Ok(Publish {
        id: id(row.get(0)?)?,
        result_id: id(row.get(1)?)?,
        source_conversation_id: id(row.get(2)?)?,
        target_conversation_id: id(row.get(3)?)?,
        created_at: row.get(4)?,
    })
}

pub(super) fn session_binding(row: &Row<'_>) -> Result<SessionBinding, StoreError> {
    let generation: i64 = row.get(5)?;
    Ok(SessionBinding {
        id: id(row.get(0)?)?,
        conversation_id: id(row.get(1)?)?,
        agent_id: id(row.get(2)?)?,
        transport_type: row.get(3)?,
        remote_session_id: row.get(4)?,
        generation: generation
            .try_into()
            .map_err(|_| StoreError::IntegerOutOfRange {
                field: "session_bindings.generation",
                value: i128::from(generation),
            })?,
        status: domain_enum(row.get(6)?)?,
        created_at: row.get(7)?,
        last_used_at: row.get(8)?,
    })
}

pub(super) fn permission_decision(row: &Row<'_>) -> Result<PermissionDecision, StoreError> {
    let options = match json_value(row.get(3)?)? {
        Value::Array(options) => options
            .into_iter()
            .map(|option| {
                let Value::Object(mut option) = option else {
                    return Err(StoreError::InvalidStoredValue(
                        "permission_decisions.options_json",
                    ));
                };
                let Some(Value::String(id)) = option.remove("id") else {
                    return Err(StoreError::InvalidStoredValue(
                        "permission_decisions.options_json.id",
                    ));
                };
                let Some(Value::String(label)) = option.remove("label") else {
                    return Err(StoreError::InvalidStoredValue(
                        "permission_decisions.options_json.label",
                    ));
                };
                Ok(PermissionOption { id, label })
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => {
            return Err(StoreError::InvalidStoredValue(
                "permission_decisions.options_json",
            ));
        }
    };
    let outcome = match (row.get::<_, String>(4)?.as_str(), row.get(5)?) {
        ("selected", Some(selected)) => PermissionOutcome::Selected(selected),
        ("cancelled", None) => PermissionOutcome::Cancelled,
        _ => {
            return Err(StoreError::InvalidStoredValue(
                "permission_decisions.outcome",
            ));
        }
    };
    let decision = PermissionDecision {
        id: row.get(0)?,
        session_binding_id: id(row.get(1)?)?,
        correlation_id: row.get(2)?,
        options,
        outcome,
        decided_at: row.get(6)?,
    };
    decision.validate()?;
    Ok(decision)
}

pub(super) fn checkpoint(row: &Row<'_>) -> Result<Checkpoint, StoreError> {
    Ok(Checkpoint {
        id: id(row.get(0)?)?,
        conversation_id: id(row.get(1)?)?,
        agent_id: id(row.get(2)?)?,
        goal: row.get(3)?,
        current_state: row.get(4)?,
        decisions: string_vec(row.get(5)?)?,
        open_items: string_vec(row.get(6)?)?,
        references: string_vec(row.get(7)?)?,
        last_message_id: optional_id(row.get(8)?)?,
        created_at: row.get(9)?,
    })
}

pub(super) fn memory(row: &Row<'_>) -> Result<Memory, StoreError> {
    Ok(Memory {
        id: id(row.get(0)?)?,
        scope_type: domain_enum(row.get(1)?)?,
        scope_id: row.get(2)?,
        kind: domain_enum(row.get(3)?)?,
        content: row.get(4)?,
        source_conversation_id: optional_id(row.get(5)?)?,
        evidence: string_vec(row.get(6)?)?,
        supersedes_memory_id: optional_id(row.get(7)?)?,
        created_at: row.get(8)?,
    })
}
