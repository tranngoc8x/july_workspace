use july_workspace::domain::{
    Agent, AgentId, MemberType, Room, RoomId, RoomMessage, RoomMessageId,
};
use july_workspace::runtime::StorageWorker;
use july_workspace::storage::SqliteStore;
use serde_json::{Value, json};
use std::path::PathBuf;

const NOW: &str = "2026-09-08T00:00:00Z";

struct Fixture {
    path: PathBuf,
    agents: Vec<Agent>,
    message: RoomMessage,
}

impl Fixture {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("july-room-a2a-{}.db", ulid::Ulid::generate()));
        let mut store = SqliteStore::open(&path).unwrap();
        let room = Room {
            id: RoomId::new(),
            name: "VNA".into(),
            description: None,
            status: "active".into(),
            created_at: NOW.into(),
            updated_at: NOW.into(),
        };
        store.create_room(&room).unwrap();
        let agents: Vec<_> = ["cashpoint", "pay", "ops", "infra"]
            .into_iter()
            .map(|name| {
                let agent = Agent {
                    id: AgentId::new(),
                    name: name.into(),
                    project_root: "/workspace".into(),
                    transport_type: "acp".into(),
                    transport_config: json!({"secret":"private-config"}),
                    status: "active".into(),
                    metadata: json!({}),
                    created_at: NOW.into(),
                    updated_at: NOW.into(),
                };
                store.insert_agent(&agent).unwrap();
                store.add_room_member(room.id, agent.id, None, NOW).unwrap();
                agent
            })
            .collect();
        let original = RoomMessage {
            id: RoomMessageId::new(),
            room_id: room.id,
            sender_type: MemberType::User,
            sender_id: "local-user".into(),
            body: "Review contract".into(),
            mentions: vec![agents[0].id],
            reply_to: None,
            created_at: NOW.into(),
        };
        store.append_room_message(&original).unwrap();
        let message = RoomMessage {
            id: RoomMessageId::new(),
            sender_type: MemberType::Agent,
            sender_id: agents[0].id.to_string(),
            body: "@pay @ops Contract uses reference_id.\n  ".into(),
            mentions: vec![agents[1].id, agents[2].id],
            reply_to: Some(original.id),
            ..original
        };
        store.append_room_message(&message).unwrap();
        Self {
            path,
            agents,
            message,
        }
    }

    // Snapshot every durable row, not only counts: receive must never alter existing state.
    fn snapshot(&self) -> Vec<(String, Vec<Vec<Value>>)> {
        let connection = rusqlite::Connection::open(&self.path).unwrap();
        let mut tables = connection
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap();
        let names: Vec<String> = tables
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        names
            .into_iter()
            .map(|table| {
                let mut statement = connection
                    .prepare(&format!("SELECT * FROM \"{}\"", table.replace('"', "\"\"")))
                    .unwrap();
                let columns = statement.column_count();
                let mut rows: Vec<Vec<Value>> = statement
                    .query_map([], |row| {
                        Ok((0..columns)
                            .map(|index| match row.get_ref(index).unwrap() {
                                rusqlite::types::ValueRef::Null => Value::Null,
                                rusqlite::types::ValueRef::Integer(value) => json!(value),
                                rusqlite::types::ValueRef::Real(value) => json!(value),
                                rusqlite::types::ValueRef::Text(value) => {
                                    json!(String::from_utf8_lossy(value))
                                }
                                rusqlite::types::ValueRef::Blob(value) => json!(value),
                            })
                            .collect())
                    })
                    .unwrap()
                    .map(Result::unwrap)
                    .collect();
                rows.sort_by_cached_key(|row| serde_json::to_string(row).unwrap());
                (table, rows)
            })
            .collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[tokio::test]
async fn canonical_roundtrip_is_recipient_scoped_repeatable_and_read_only() {
    let fixture = Fixture::new();
    let mut worker = StorageWorker::open(&fixture.path).unwrap();
    let before = fixture.snapshot(); // Opening a worker may reconcile old runtime deliveries.
    let pay = fixture.agents[1].id;
    let ops = fixture.agents[2].id;
    let wire = worker
        .prepare_room_a2a_message(fixture.message.id, pay)
        .await
        .unwrap();
    let serialized = serde_json::to_string(&wire).unwrap();
    assert!(!serialized.contains("private-config"));
    let wire: Value = serde_json::from_str(&serialized).unwrap();
    for _ in 0..2 {
        let received = worker.receive_room_a2a_message(pay, &wire).await.unwrap();
        assert_eq!(received.target, pay);
        assert_eq!(received.message, fixture.message);
        assert_eq!(
            worker
                .prepare_room_a2a_message(fixture.message.id, pay)
                .await
                .unwrap(),
            wire
        );
    }
    let other = worker
        .prepare_room_a2a_message(fixture.message.id, ops)
        .await
        .unwrap();
    assert_ne!(wire["messageId"], other["messageId"]);
    assert!(worker.receive_room_a2a_message(ops, &wire).await.is_err());
    assert_eq!(
        worker
            .receive_room_a2a_message(ops, &other)
            .await
            .unwrap()
            .message,
        fixture.message
    );
    assert!(
        worker
            .prepare_room_a2a_message(fixture.message.id, fixture.agents[3].id)
            .await
            .is_err()
    );
    assert!(
        worker
            .prepare_room_a2a_message(RoomMessageId::new(), pay)
            .await
            .is_err()
    );
    assert!(
        worker
            .prepare_room_a2a_message(fixture.message.reply_to.unwrap(), fixture.agents[0].id)
            .await
            .is_err()
    );
    for pointer in [
        "/messageId",
        "/contextId",
        "/parts/0/text",
        "/metadata/july.room_id",
        "/metadata/july.sender_agent_id",
        "/metadata/july.target_agent_id",
        "/metadata/july.reply_to",
    ] {
        let mut forged = wire.clone();
        *forged.pointer_mut(pointer).unwrap() = json!("forged");
        assert!(
            worker.receive_room_a2a_message(pay, &forged).await.is_err(),
            "{pointer}"
        );
    }
    for malformed in [
        json!(null),
        json!([]),
        json!({}),
        json!({"metadata":{"july.room_message_id":"invalid"}}),
    ] {
        assert!(
            worker
                .receive_room_a2a_message(pay, &malformed)
                .await
                .is_err()
        );
    }
    let mut unknown = wire.clone();
    unknown["metadata"]["july.room_message_id"] = json!(RoomMessageId::new().to_string());
    assert!(
        worker
            .receive_room_a2a_message(pay, &unknown)
            .await
            .is_err()
    );
    for key in ["private_transcript", "taskId"] {
        let mut forged = wire.clone();
        forged[key] = json!("secret");
        assert!(worker.receive_room_a2a_message(pay, &forged).await.is_err());
    }
    assert_eq!(fixture.snapshot(), before);
    worker.shutdown().await.unwrap();
    let mut reopened = StorageWorker::open(&fixture.path).unwrap();
    assert_eq!(
        reopened
            .receive_room_a2a_message(pay, &wire)
            .await
            .unwrap()
            .message,
        fixture.message
    );
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn bridge_revalidates_sender_recipient_and_room_before_each_receive() {
    for mutation in [
        "sender_removed",
        "target_removed",
        "sender_inactive",
        "target_inactive",
        "room_inactive",
    ] {
        let fixture = Fixture::new();
        let mut worker = StorageWorker::open(&fixture.path).unwrap();
        let target = fixture.agents[1].id;
        let wire = worker
            .prepare_room_a2a_message(fixture.message.id, target)
            .await
            .unwrap();
        let mut store = SqliteStore::open(&fixture.path).unwrap();
        match mutation {
            "sender_removed" | "target_removed" => {
                let id = if mutation == "sender_removed" {
                    fixture.agents[0].id
                } else {
                    target
                };
                store
                    .remove_room_member(fixture.message.room_id, id, NOW)
                    .unwrap();
            }
            "sender_inactive" | "target_inactive" => {
                let mut agent = fixture.agents[usize::from(mutation == "target_inactive")].clone();
                agent.status = "inactive".into();
                store.update_agent(&agent).unwrap();
            }
            _ => {
                rusqlite::Connection::open(&fixture.path)
                    .unwrap()
                    .execute(
                        "UPDATE rooms SET status='archived' WHERE id=?1",
                        [fixture.message.room_id.to_string()],
                    )
                    .unwrap();
            }
        }
        let before = fixture.snapshot();
        assert!(
            worker
                .receive_room_a2a_message(target, &wire)
                .await
                .is_err(),
            "{mutation}"
        );
        assert!(
            worker
                .prepare_room_a2a_message(fixture.message.id, target)
                .await
                .is_err(),
            "{mutation}"
        );
        assert_eq!(fixture.snapshot(), before);
        worker.shutdown().await.unwrap();
    }
}
