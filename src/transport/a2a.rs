//! Internal A2A 0.3 Message profile, not an HTTP binding or Task implementation.

use crate::domain::{AgentId, MemberType, RoomMessage};
use serde_json::{Value, json};

#[derive(Debug, thiserror::Error)]
#[error("invalid internal A2A Room message")]
pub struct A2aMessageError;

/// Project a canonical shared message for one logical recipient. Membership is
/// checked by the storage-backed bridge, not by this pure mapping.
pub fn encode_room_message(
    message: &RoomMessage,
    target: AgentId,
) -> Result<Value, A2aMessageError> {
    message.validate().map_err(|_| A2aMessageError)?;
    if message.sender_type != MemberType::Agent || !message.mentions.contains(&target) {
        return Err(A2aMessageError);
    }
    let sender: AgentId = message.sender_id.parse().map_err(|_| A2aMessageError)?;
    if sender.to_string() != message.sender_id {
        return Err(A2aMessageError);
    }
    Ok(json!({
        "kind": "message",
        // The initiating A2A client has role user, even when July's sender is an agent.
        "role": "user",
        "messageId": format!("{}:{}", message.id, target),
        "contextId": message.room_id.to_string(),
        "parts": [{"kind": "text", "text": message.body}],
        "metadata": {
            "july.room_id": message.room_id.to_string(),
            "july.room_message_id": message.id.to_string(),
            "july.sender_agent_id": sender.to_string(),
            "july.target_agent_id": target.to_string(),
            "july.reply_to": message.reply_to.map(|id| id.to_string()),
            "july.created_at": message.created_at,
            "july.mentions": message.mentions.iter().map(ToString::to_string).collect::<Vec<_>>()
        }
    }))
}

/// Accept only this exact profile of the persisted message. Extra private data
/// and Task fields are rejected; wire content never replaces canonical state.
pub fn validate_room_message(
    value: &Value,
    message: &RoomMessage,
    target: AgentId,
) -> Result<(), A2aMessageError> {
    if *value != encode_room_message(message, target)? {
        return Err(A2aMessageError);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{RoomId, RoomMessageId};

    fn message() -> RoomMessage {
        RoomMessage {
            id: RoomMessageId::new(),
            room_id: RoomId::new(),
            sender_type: MemberType::Agent,
            sender_id: AgentId::new().to_string(),
            body: "Shared answer\nwith exact whitespace.  ".into(),
            mentions: vec![AgentId::new(), AgentId::new()],
            reply_to: Some(RoomMessageId::new()),
            created_at: "2026-09-08T06:00:00Z".into(),
        }
    }

    #[test]
    fn preserves_shared_content_and_stable_per_recipient_identity() {
        let message = message();
        let first = encode_room_message(&message, message.mentions[0]).unwrap();
        let second = encode_room_message(&message, message.mentions[1]).unwrap();
        assert_eq!(
            first,
            encode_room_message(&message, message.mentions[0]).unwrap()
        );
        assert_ne!(first["messageId"], second["messageId"]);
        assert_eq!(first["role"], "user");
        assert_eq!(first["kind"], "message");
        assert_eq!(first["contextId"], message.room_id.to_string());
        assert_eq!(
            first["parts"],
            json!([{"kind":"text", "text":message.body}])
        );
        assert_eq!(
            first["metadata"]["july.reply_to"],
            message.reply_to.unwrap().to_string()
        );
        assert_eq!(first["metadata"]["july.sender_agent_id"], message.sender_id);
        validate_room_message(&first, &message, message.mentions[0]).unwrap();
        assert!(validate_room_message(&first, &message, message.mentions[1]).is_err());
        let mut without_reply = message;
        without_reply.reply_to = None;
        assert!(encode_room_message(&without_reply, without_reply.mentions[0]).unwrap()["metadata"]["july.reply_to"].is_null());
    }

    #[test]
    fn rejects_forged_private_and_task_fields() {
        let message = message();
        let target = message.mentions[0];
        let encoded = encode_room_message(&message, target).unwrap();
        for pointer in [
            "/messageId",
            "/contextId",
            "/role",
            "/kind",
            "/parts/0/text",
            "/metadata/july.room_id",
            "/metadata/july.room_message_id",
            "/metadata/july.sender_agent_id",
            "/metadata/july.target_agent_id",
            "/metadata/july.reply_to",
            "/metadata/july.created_at",
            "/metadata/july.mentions",
        ] {
            let mut forged = encoded.clone();
            *forged.pointer_mut(pointer).unwrap() = json!("forged");
            assert!(
                validate_room_message(&forged, &message, target).is_err(),
                "{pointer}"
            );
        }
        for field in [
            "taskId",
            "status",
            "history",
            "artifacts",
            "private_transcript",
        ] {
            let mut forged = encoded.clone();
            forged[field] = json!("private");
            assert!(validate_room_message(&forged, &message, target).is_err());
        }
        let mut private = encoded.clone();
        private["metadata"]["private_trace"] = json!("secret");
        assert!(validate_room_message(&private, &message, target).is_err());
        let mut system = encoded.clone();
        system["role"] = json!("system");
        assert!(validate_room_message(&system, &message, target).is_err());
        let mut data = encoded;
        data["parts"] = json!([{"kind":"data", "data":{"secret":"trace"}}]);
        assert!(validate_room_message(&data, &message, target).is_err());
        for malformed in [Value::Null, json!([]), json!({})] {
            assert!(validate_room_message(&malformed, &message, target).is_err());
        }
    }

    #[test]
    fn rejects_non_agent_sender_and_unmentioned_target() {
        let mut message = message();
        assert!(encode_room_message(&message, AgentId::new()).is_err());
        message.sender_type = MemberType::User;
        assert!(encode_room_message(&message, message.mentions[0]).is_err());
        message.sender_type = MemberType::Agent;
        message.sender_id = "forged".into();
        assert!(encode_room_message(&message, message.mentions[0]).is_err());
    }
}
