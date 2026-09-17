use super::*;
use crate::domain::{RoomA2aTaskBinding, RoomWork, RoomWorkIntent};
use serde_json::{Value, json};

fn intent_value(intent: &RoomWorkIntent) -> Value {
    match intent {
        RoomWorkIntent::Create { title, goal } => {
            json!({"action":"create","title":title,"goal":goal})
        }
        RoomWorkIntent::Bind { work_id } => json!({"action":"bind","work_id":work_id.to_string()}),
    }
}

pub(super) fn bind_publication(
    connection: &Connection,
    message: &RoomMessage,
    request: &SendRoomMessage,
    replay: bool,
) -> Result<(), StoreError> {
    let previous = query_optional(
        connection,
        "SELECT intent_json FROM room_message_work WHERE message_id=?1",
        params![message.id.to_string()],
        |r| r.get::<_, String>(0).map_err(StoreError::from),
    )?;
    let intent = request.work.as_ref().map(intent_value);
    if replay {
        let previous = previous
            .map(|v| serde_json::from_str::<Value>(&v))
            .transpose()?;
        return if previous == intent {
            Ok(())
        } else {
            Err(StoreError::RoomPublicationConflict)
        };
    }
    let Some(work_intent) = &request.work else {
        return Ok(());
    };
    if request.request_id.is_none() || message.mentions.len() != 1 {
        return Err(StoreError::InvalidRoomMessageRequest(
            "Work requires request_id and exactly one owner target",
        ));
    }
    let requester: AgentId = message.sender_id.parse()?;
    let owner = message.mentions[0];
    let work = match work_intent {
        RoomWorkIntent::Create { title, goal } => {
            let work = WorkItem {
                id: WorkItemId::new(),
                scope: WorkScope::Room(message.room_id),
                title: title.clone(),
                goal: goal.clone(),
                status: WorkStatus::Open,
                owner_agent_id: None,
                is_primary: false,
                created_at: message.created_at.clone(),
                updated_at: message.created_at.clone(),
                completed_at: None,
            };
            insert_work_item(connection, &work)?;
            assign_work_owner(connection, work.id, owner, &message.created_at)?
        }
        RoomWorkIntent::Bind { work_id } => {
            let work = require_work_item(connection, *work_id)?;
            if work.scope != WorkScope::Room(message.room_id)
                || work.owner_agent_id != Some(owner)
                || work.status.is_terminal()
            {
                return Err(StoreError::InvalidRoomMessageRequest(
                    "Work must be active in this Room and owned by the target",
                ));
            }
            work
        }
    };
    if let Some(binding) = binding_for_work(connection, work.id)? {
        if binding.requester_agent_id != requester
            || binding.owner_agent_id != owner
            || binding.room_id != message.room_id
        {
            return Err(StoreError::InvalidRoomMessageRequest(
                "Work belongs to another delegation",
            ));
        }
    } else {
        // Only the owner may expose previously unbound Work. Created Work is scoped to this requester.
        if matches!(work_intent, RoomWorkIntent::Bind { .. }) && requester != owner {
            return Err(StoreError::InvalidRoomMessageRequest(
                "Only the owner may first bind existing Work",
            ));
        }
        connection.execute("INSERT INTO room_a2a_task_bindings(work_id,task_id,room_id,requester_agent_id,owner_agent_id) VALUES (?1,?2,?3,?4,?5)",
            params![work.id.to_string(), ulid::Ulid::generate().to_string(), message.room_id.to_string(), requester.to_string(), owner.to_string()])?;
    }
    connection.execute(
        "INSERT INTO room_message_work(message_id,work_id,intent_json) VALUES (?1,?2,?3)",
        params![
            message.id.to_string(),
            work.id.to_string(),
            intent.expect("work intent").to_string()
        ],
    )?;
    Ok(())
}

fn binding_for_work(
    connection: &Connection,
    work: WorkItemId,
) -> Result<Option<RoomA2aTaskBinding>, StoreError> {
    query_optional(
        connection,
        "SELECT work_id,task_id,room_id,requester_agent_id,owner_agent_id FROM room_a2a_task_bindings WHERE work_id=?1",
        params![work.to_string()],
        |row| {
            Ok(RoomA2aTaskBinding {
                work_id: row.get::<_, String>(0)?.parse()?,
                task_id: row.get(1)?,
                room_id: row.get::<_, String>(2)?.parse()?,
                requester_agent_id: row.get::<_, String>(3)?.parse()?,
                owner_agent_id: row.get::<_, String>(4)?.parse()?,
            })
        },
    )
}

impl SqliteStore {
    /// Shared Work linked to a durable Room message. This does not authorize an agent mutation.
    pub fn get_room_message_work(
        &self,
        message: RoomMessageId,
    ) -> Result<Option<RoomWork>, StoreError> {
        // Snapshot Work and its results together, even while another connection writes.
        let transaction = self.connection.unchecked_transaction()?;
        let work_id = query_optional(
            &transaction,
            "SELECT work_id FROM room_message_work WHERE message_id=?1",
            params![message.to_string()],
            |row| row.get::<_, String>(0).map_err(StoreError::from),
        )?;
        let Some(work_id) = work_id else {
            return Ok(None);
        };
        let work_id = work_id.parse()?;
        let binding = binding_for_work(&transaction, work_id)?
            .ok_or(StoreError::InvalidStoredValue("Room task binding"))?;
        let work = require_work_item(&transaction, work_id)?;
        let results = query_all(
            &transaction,
            "SELECT id,work_id,status,summary,outputs_json,evidence_json,supersedes_result_id,created_at FROM work_results WHERE work_id=?1 ORDER BY created_at,id",
            params![work_id.to_string()],
            records::work_result,
        )?;
        transaction.commit()?;
        Ok(Some(RoomWork {
            binding,
            work,
            results,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::a2a::encode_room_task;

    const NOW: &str = "2026-09-09T00:00:00Z";

    struct Fixture {
        store: SqliteStore,
        room: RoomId,
        trigger: RoomMessageId,
        sender: AgentId,
        owner: AgentId,
    }
    impl Fixture {
        fn new() -> Self {
            let mut store = SqliteStore::open(":memory:").unwrap();
            let room = RoomId::new();
            store
                .create_room(&Room {
                    id: room,
                    name: "room".into(),
                    description: None,
                    status: "active".into(),
                    created_at: NOW.into(),
                    updated_at: NOW.into(),
                })
                .unwrap();
            let mut agents = Vec::new();
            for name in ["sender", "owner", "other"] {
                let id = AgentId::new();
                store
                    .insert_agent(&Agent {
                        id,
                        name: name.into(),
                        project_root: "/tmp".into(),
                        transport_type: "acp".into(),
                        transport_config: json!({}),
                        status: "active".into(),
                        metadata: json!({}),
                        created_at: NOW.into(),
                        updated_at: NOW.into(),
                    })
                    .unwrap();
                store.add_room_member(room, id, None, NOW).unwrap();
                agents.push(id);
            }
            let trigger = RoomMessageId::new();
            store
                .append_room_message(&RoomMessage {
                    id: trigger,
                    room_id: room,
                    sender_type: MemberType::User,
                    sender_id: "july".into(),
                    body: "Implement contract".into(),
                    mentions: vec![agents[0]],
                    reply_to: None,
                    created_at: NOW.into(),
                })
                .unwrap();
            let claim = store
                .claim_room_activation(trigger, agents[0], NOW)
                .unwrap()
                .unwrap();
            store
                .attach_room_remote_session(claim.binding.id, "session", NOW)
                .unwrap();
            Self {
                store,
                room,
                trigger,
                sender: agents[0],
                owner: agents[1],
            }
        }
        fn request(&self) -> SendRoomMessage {
            SendRoomMessage {
                targets: vec!["owner".into()],
                body: "Implement contract".into(),
                reply_to: Some(self.trigger),
                request_id: Some("delegate".into()),
                work: Some(RoomWorkIntent::Create {
                    title: "Contract".into(),
                    goal: Some("test evidence".into()),
                }),
            }
        }
        fn send(&mut self, request: &SendRoomMessage) -> Result<RoomMessage, StoreError> {
            self.store.send_agent_room_message(
                self.trigger,
                self.sender,
                request,
                NOW,
                &AtomicBool::new(true),
            )
        }
        fn count(&self, table: &str) -> i64 {
            self.store
                .connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap()
        }
    }

    #[test]
    fn explicit_delegation_is_atomic_scoped_and_replay_safe() {
        let mut f = Fixture::new();
        let request = f.request();
        for invalid in [
            SendRoomMessage {
                request_id: None,
                ..request.clone()
            },
            SendRoomMessage {
                targets: vec![],
                ..request.clone()
            },
            SendRoomMessage {
                targets: vec!["owner".into(), "other".into()],
                ..request.clone()
            },
            SendRoomMessage {
                work: Some(RoomWorkIntent::Create {
                    title: " ".into(),
                    goal: None,
                }),
                ..request.clone()
            },
        ] {
            assert!(f.send(&invalid).is_err());
            assert_eq!(f.count("work_items"), 0);
            assert_eq!(f.count("room_messages"), 1);
        }
        let message = f.send(&request).unwrap();
        let shared = f.store.get_room_message_work(message.id).unwrap().unwrap();
        assert_eq!(shared.work.scope, WorkScope::Room(f.room));
        assert_eq!(shared.work.owner_agent_id, Some(f.owner));
        assert_eq!(shared.binding.requester_agent_id, f.sender);
        assert_eq!(shared.binding.owner_agent_id, f.owner);
        assert_eq!(f.send(&request).unwrap(), message);
        assert_eq!(f.count("work_items"), 1);
        assert_eq!(f.count("room_a2a_task_bindings"), 1);
        assert_eq!(f.count("conversations"), 0);
        for conflict in [
            SendRoomMessage {
                work: None,
                ..request.clone()
            },
            SendRoomMessage {
                work: Some(RoomWorkIntent::Create {
                    title: "changed".into(),
                    goal: None,
                }),
                ..request.clone()
            },
            SendRoomMessage {
                work: Some(RoomWorkIntent::Bind {
                    work_id: shared.work.id,
                }),
                ..request.clone()
            },
        ] {
            assert!(matches!(
                f.send(&conflict),
                Err(StoreError::RoomPublicationConflict)
            ));
        }
        let bind = SendRoomMessage {
            work: Some(RoomWorkIntent::Bind {
                work_id: shared.work.id,
            }),
            request_id: Some("followup".into()),
            ..request.clone()
        };
        let followup = f.send(&bind).unwrap();
        assert_eq!(
            f.store
                .get_room_message_work(followup.id)
                .unwrap()
                .unwrap()
                .binding,
            shared.binding
        );
        assert_eq!(f.count("work_items"), 1);
        assert!(
            f.send(&SendRoomMessage {
                targets: vec!["other".into()],
                request_id: Some("wrong-owner".into()),
                ..bind.clone()
            })
            .is_err()
        );
        let unbound = WorkItem {
            id: WorkItemId::new(),
            owner_agent_id: None,
            ..shared.work.clone()
        };
        f.store.insert_work_item(&unbound).unwrap();
        f.store.assign_work_owner(unbound.id, f.owner, NOW).unwrap();
        let bind_unbound = SendRoomMessage {
            work: Some(RoomWorkIntent::Bind {
                work_id: unbound.id,
            }),
            request_id: Some("unbound".into()),
            ..bind.clone()
        };
        assert!(f.send(&bind_unbound).is_err());
        f.store
            .assign_work_owner(unbound.id, f.sender, NOW)
            .unwrap();
        let bound = f
            .send(&SendRoomMessage {
                targets: vec!["sender".into()],
                ..bind_unbound
            })
            .unwrap();
        assert_eq!(
            f.store
                .get_room_message_work(bound.id)
                .unwrap()
                .unwrap()
                .work
                .id,
            unbound.id
        );
        let other_room = RoomId::new();
        f.store
            .create_room(&Room {
                id: other_room,
                name: "elsewhere".into(),
                description: None,
                status: "active".into(),
                created_at: NOW.into(),
                updated_at: NOW.into(),
            })
            .unwrap();
        let foreign = WorkItem {
            id: WorkItemId::new(),
            scope: WorkScope::Room(other_room),
            owner_agent_id: None,
            ..shared.work.clone()
        };
        f.store.insert_work_item(&foreign).unwrap();
        assert!(
            f.send(&SendRoomMessage {
                work: Some(RoomWorkIntent::Bind {
                    work_id: foreign.id
                }),
                request_id: Some("foreign".into()),
                ..bind.clone()
            })
            .is_err()
        );
        let unauthorized = f
            .store
            .list_room_members(f.room)
            .unwrap()
            .into_iter()
            .find(|m| m.agent_id != f.sender && m.agent_id != f.owner)
            .unwrap()
            .agent_id;
        assert!(
            f.store
                .assign_work_owner(shared.work.id, unauthorized, NOW)
                .is_err()
        );
        f.store.remove_room_member(f.room, f.owner, NOW).unwrap();
        assert!(f.send(&request).is_err());
        assert_eq!(f.count("room_message_work"), 3);
    }

    #[test]
    fn task_status_and_artifacts_follow_only_canonical_work_and_results() {
        let mut f = Fixture::new();
        let message = f.send(&f.request()).unwrap();
        let shared = f.store.get_room_message_work(message.id).unwrap().unwrap();
        let task = encode_room_task(&message, f.owner, &shared).unwrap();
        assert_eq!(task["status"]["state"], "submitted");
        assert!(task["artifacts"].as_array().unwrap().is_empty());
        for status in [
            WorkStatus::Working,
            WorkStatus::Blocked,
            WorkStatus::Working,
        ] {
            f.store
                .transition_work(shared.work.id, status, NOW)
                .unwrap();
            let current = f.store.get_room_message_work(message.id).unwrap().unwrap();
            assert_eq!(
                encode_room_task(&message, f.owner, &current).unwrap()["status"]["state"],
                "working"
            );
        }
        let result = WorkResult {
            id: ResultId::new(),
            work_id: shared.work.id,
            status: "success".into(),
            summary: "Contract implemented".into(),
            outputs: vec!["src/contract.rs".into()],
            evidence: vec!["contract tests pass".into()],
            supersedes_result_id: None,
            created_at: NOW.into(),
        };
        f.store.create_work_result(&result).unwrap();
        let ready = f.store.get_room_message_work(message.id).unwrap().unwrap();
        let task = encode_room_task(&message, f.owner, &ready).unwrap();
        assert_eq!(task["status"]["state"], "working"); // July still requires acceptance of READY.
        assert_eq!(task["artifacts"][0]["artifactId"], result.id.to_string());
        assert_eq!(
            task["artifacts"][0]["parts"][1]["data"]["evidence"],
            json!(result.evidence)
        );
        f.store
            .transition_work(shared.work.id, WorkStatus::Done, NOW)
            .unwrap();
        let done = f.store.get_room_message_work(message.id).unwrap().unwrap();
        assert_eq!(
            encode_room_task(&message, f.owner, &done).unwrap()["status"]["state"],
            "completed"
        );
        assert_eq!(done.binding, shared.binding);
        // Replaying a publication after completion cannot create or reopen Work.
        assert_eq!(f.send(&f.request()).unwrap(), message);
        assert_eq!(
            f.store
                .get_work_item(shared.work.id)
                .unwrap()
                .unwrap()
                .status,
            WorkStatus::Done
        );
        for (terminal, expected) in [
            (WorkStatus::Failed, "failed"),
            (WorkStatus::Cancelled, "canceled"),
        ] {
            let mut another = Fixture::new();
            let message = another.send(&another.request()).unwrap();
            let shared = another
                .store
                .get_room_message_work(message.id)
                .unwrap()
                .unwrap();
            another
                .store
                .transition_work(shared.work.id, WorkStatus::Working, NOW)
                .unwrap();
            another
                .store
                .transition_work(shared.work.id, terminal, NOW)
                .unwrap();
            let current = another
                .store
                .get_room_message_work(message.id)
                .unwrap()
                .unwrap();
            assert_eq!(
                encode_room_task(&message, another.owner, &current).unwrap()["status"]["state"],
                expected
            );
        }
        assert!(
            f.store
                .publish_result(PublishId::new(), result.id, ConversationId::new(), NOW)
                .is_err()
        );
    }
}
