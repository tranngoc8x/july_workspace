use july_workspace::application::{
    AddRoomMember, AgentRef, AppendRoomMessage, CollaborationError, CollaborationService,
    CreateRoom, RemoveRoomMember, RoomRef,
};
use july_workspace::domain::{Agent, AgentId, MemberType, RoomId, RoomMessage, RoomMessageId};
use july_workspace::runtime::StorageWorker;
use july_workspace::storage::{SqliteStore, StoreError};
use serde_json::json;
use std::path::{Path, PathBuf};

const CREATED: &str = "2026-09-05T09:00:00Z";

struct TestDatabase {
    directory: PathBuf,
    path: PathBuf,
}

impl TestDatabase {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("july-room-messages-{}", ulid::Ulid::generate()));
        std::fs::create_dir(&directory).unwrap();
        Self {
            path: directory.join("workspace.db"),
            directory,
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn agent(name: &str, status: &str) -> Agent {
    Agent {
        id: AgentId::new(),
        name: name.into(),
        project_root: format!("/workspace/{name}"),
        transport_type: "acp".into(),
        transport_config: json!({}),
        status: status.into(),
        metadata: json!({}),
        created_at: CREATED.into(),
        updated_at: CREATED.into(),
    }
}

fn new_service(path: &Path) -> CollaborationService<StorageWorker> {
    CollaborationService::new(StorageWorker::open(path).unwrap())
}

async fn room(service: &mut CollaborationService<StorageWorker>, name: &str) -> RoomId {
    service
        .create_room(CreateRoom {
            room_id: RoomId::new(),
            name: name.into(),
            description: None,
            created_at: CREATED.into(),
        })
        .await
        .unwrap()
}

async fn add_member(
    service: &mut CollaborationService<StorageWorker>,
    room_id: RoomId,
    agent_id: AgentId,
) {
    service
        .add_room_member(AddRoomMember {
            room: RoomRef::Id(room_id),
            agent: AgentRef::Id(agent_id),
            role: None,
            changed_at: CREATED.into(),
        })
        .await
        .unwrap();
}

#[allow(clippy::too_many_arguments)]
fn message(
    id: RoomMessageId,
    room_id: RoomId,
    sender_type: MemberType,
    sender_id: impl Into<String>,
    body: impl Into<String>,
    mentions: Vec<AgentId>,
    reply_to: Option<RoomMessageId>,
    created_at: &str,
) -> RoomMessage {
    RoomMessage {
        id,
        room_id,
        sender_type,
        sender_id: sender_id.into(),
        body: body.into(),
        mentions,
        reply_to,
        created_at: created_at.into(),
    }
}

#[tokio::test]
async fn room_messages_survive_restart_and_list_by_created_at_then_id() {
    let database = TestDatabase::new();
    let mut service = new_service(database.path());
    let room_id = room(&mut service, "Payments").await;
    let first_id = RoomMessageId::new();
    let second_id = RoomMessageId::new();
    let (lower_id, higher_id) = if first_id < second_id {
        (first_id, second_id)
    } else {
        (second_id, first_id)
    };
    let higher = message(
        higher_id,
        room_id,
        MemberType::User,
        "july",
        "second by id",
        vec![],
        None,
        CREATED,
    );
    let lower = message(
        lower_id,
        room_id,
        MemberType::User,
        "july",
        "first by id",
        vec![],
        None,
        CREATED,
    );
    let newer = message(
        RoomMessageId::new(),
        room_id,
        MemberType::User,
        "july",
        "newer timestamp",
        vec![],
        None,
        "2026-09-05T09:00:01Z",
    );

    service
        .append_room_message(AppendRoomMessage {
            message: higher.clone(),
        })
        .await
        .unwrap();
    service
        .append_room_message(AppendRoomMessage {
            message: lower.clone(),
        })
        .await
        .unwrap();
    service
        .append_room_message(AppendRoomMessage {
            message: newer.clone(),
        })
        .await
        .unwrap();
    let mut worker = service.into_runtime();
    worker.shutdown().await.unwrap();

    let mut reloaded = new_service(database.path());
    assert_eq!(
        reloaded
            .list_recent_room_messages(room_id, 50)
            .await
            .unwrap(),
        (vec![lower.clone(), higher.clone(), newer.clone()], false)
    );
    assert_eq!(
        reloaded
            .list_recent_room_messages(room_id, 2)
            .await
            .unwrap(),
        (vec![higher, newer], true)
    );
}

#[tokio::test]
async fn room_message_rejects_an_untrusted_user_sender_without_persisting() {
    let database = TestDatabase::new();
    let mut service = new_service(database.path());
    let room_id = room(&mut service, "Payments").await;
    let untrusted = message(
        RoomMessageId::new(),
        room_id,
        MemberType::User,
        "evil-user",
        "forged room message",
        vec![],
        None,
        CREATED,
    );

    assert_eq!(
        service
            .append_room_message(AppendRoomMessage { message: untrusted })
            .await,
        Err(CollaborationError::UntrustedRoomUserSender(
            "evil-user".into()
        ))
    );
    assert_eq!(
        service
            .list_recent_room_messages(room_id, 50)
            .await
            .unwrap(),
        (vec![], false)
    );
}

#[tokio::test]
async fn exact_room_message_replay_is_idempotent_but_conflicts_on_different_content() {
    let database = TestDatabase::new();
    let mut service = new_service(database.path());
    let room_id = room(&mut service, "Payments").await;
    let original = message(
        RoomMessageId::new(),
        room_id,
        MemberType::User,
        "july",
        "check refund",
        vec![],
        None,
        CREATED,
    );

    assert_eq!(
        service
            .append_room_message(AppendRoomMessage {
                message: original.clone(),
            })
            .await
            .unwrap(),
        original
    );
    assert_eq!(
        service
            .append_room_message(AppendRoomMessage {
                message: original.clone(),
            })
            .await
            .unwrap(),
        original
    );
    let mut conflicting = original.clone();
    conflicting.body = "different body".into();
    assert_eq!(
        service
            .append_room_message(AppendRoomMessage {
                message: conflicting,
            })
            .await,
        Err(CollaborationError::RoomMessageIdConflict(original.id))
    );
}

#[tokio::test]
async fn agent_room_message_replay_canonicalizes_sender_identity_before_comparison_and_persistence()
{
    let database = TestDatabase::new();
    let active = agent("cashpoint", "active");
    SqliteStore::open(database.path())
        .unwrap()
        .insert_agent(&active)
        .unwrap();
    let mut service = new_service(database.path());
    let room_id = room(&mut service, "Payments").await;
    add_member(&mut service, room_id, active.id).await;

    let canonical = message(
        RoomMessageId::new(),
        room_id,
        MemberType::Agent,
        active.id.to_string(),
        "canonical agent reply",
        vec![],
        None,
        CREATED,
    );
    let mut lowercase = canonical.clone();
    lowercase.sender_id = lowercase.sender_id.to_lowercase();

    assert_eq!(
        service
            .append_room_message(AppendRoomMessage { message: lowercase })
            .await
            .unwrap(),
        canonical
    );
    assert_eq!(
        service
            .append_room_message(AppendRoomMessage {
                message: canonical.clone(),
            })
            .await
            .unwrap(),
        canonical
    );
    assert_eq!(
        service
            .list_recent_room_messages(room_id, 50)
            .await
            .unwrap(),
        (vec![canonical], false)
    );
}

#[tokio::test]
async fn room_message_reply_must_exist_in_the_same_room() {
    let database = TestDatabase::new();
    let mut service = new_service(database.path());
    let room_id = room(&mut service, "Payments").await;
    let other_room_id = room(&mut service, "Claims").await;
    let parent = message(
        RoomMessageId::new(),
        room_id,
        MemberType::User,
        "july",
        "parent",
        vec![],
        None,
        CREATED,
    );
    service
        .append_room_message(AppendRoomMessage {
            message: parent.clone(),
        })
        .await
        .unwrap();

    let missing = message(
        RoomMessageId::new(),
        room_id,
        MemberType::User,
        "july",
        "missing reply",
        vec![],
        Some(RoomMessageId::new()),
        CREATED,
    );
    assert!(matches!(
        service
            .append_room_message(AppendRoomMessage { message: missing })
            .await,
        Err(CollaborationError::RoomMessageReplyNotFound(_))
    ));

    let cross_room = message(
        RoomMessageId::new(),
        other_room_id,
        MemberType::User,
        "july",
        "cross room reply",
        vec![],
        Some(parent.id),
        CREATED,
    );
    assert_eq!(
        service
            .append_room_message(AppendRoomMessage {
                message: cross_room,
            })
            .await,
        Err(CollaborationError::RoomMessageReplyNotInRoom {
            room_id: other_room_id,
            reply_to: parent.id,
        })
    );
}

#[tokio::test]
async fn agent_room_senders_must_exist_be_active_and_be_active_room_members() {
    let database = TestDatabase::new();
    let active = agent("cashpoint", "active");
    let inactive = agent("pay", "inactive");
    SqliteStore::open(database.path())
        .unwrap()
        .insert_agent(&active)
        .unwrap();
    SqliteStore::open(database.path())
        .unwrap()
        .insert_agent(&inactive)
        .unwrap();
    let mut service = new_service(database.path());
    let room_id = room(&mut service, "Payments").await;
    add_member(&mut service, room_id, active.id).await;

    let missing_id = AgentId::new();
    for (sender_id, expected) in [
        (
            missing_id,
            CollaborationError::AgentNotFound(missing_id.to_string()),
        ),
        (inactive.id, CollaborationError::AgentInactive(inactive.id)),
    ] {
        assert_eq!(
            service
                .append_room_message(AppendRoomMessage {
                    message: message(
                        RoomMessageId::new(),
                        room_id,
                        MemberType::Agent,
                        sender_id.to_string(),
                        "agent reply",
                        vec![],
                        None,
                        CREATED,
                    ),
                })
                .await,
            Err(expected)
        );
    }

    let replay = message(
        RoomMessageId::new(),
        room_id,
        MemberType::Agent,
        active.id.to_string(),
        "durable agent reply",
        vec![],
        None,
        CREATED,
    );
    service
        .append_room_message(AppendRoomMessage {
            message: replay.clone(),
        })
        .await
        .unwrap();
    service
        .remove_room_member(RemoveRoomMember {
            room: RoomRef::Id(room_id),
            agent: AgentRef::Id(active.id),
            changed_at: "2026-09-05T10:00:00Z".into(),
        })
        .await
        .unwrap();
    assert_eq!(
        service
            .append_room_message(AppendRoomMessage {
                message: replay.clone(),
            })
            .await
            .unwrap(),
        replay,
        "exact replay is idempotent even after its original sender leaves"
    );

    let other_room_id = room(&mut service, "Claims").await;
    assert_eq!(
        service
            .append_room_message(AppendRoomMessage {
                message: message(
                    RoomMessageId::new(),
                    other_room_id,
                    MemberType::Agent,
                    active.id.to_string(),
                    "not a member here",
                    vec![],
                    None,
                    CREATED,
                ),
            })
            .await,
        Err(CollaborationError::RoomMembershipRequired {
            room_id: other_room_id,
            agent_id: active.id,
        })
    );
}

#[tokio::test]
async fn room_mention_targets_are_validated_atomically_at_the_storage_boundary() {
    let database = TestDatabase::new();
    let pay = agent("pay", "active");
    let infra = agent("infra", "active");
    let inactive = agent("inactive", "inactive");
    let mut store = SqliteStore::open(database.path()).unwrap();
    for target in [&pay, &infra, &inactive] {
        store.insert_agent(target).unwrap();
    }
    let mut service = new_service(database.path());
    let room_id = room(&mut service, "VNA").await;
    add_member(&mut service, room_id, pay.id).await;
    let unknown = AgentId::new();
    for target in [infra.id, inactive.id, unknown] {
        let message = message(
            RoomMessageId::new(),
            room_id,
            MemberType::User,
            "july",
            "@pay @other check refund",
            vec![pay.id, target],
            None,
            CREATED,
        );
        match store.append_room_message(&message).unwrap_err() {
            StoreError::RoomMembershipRequired {
                room_id: rejected_room,
                agent_id,
            } => {
                assert_eq!(
                    (rejected_room, agent_id, target),
                    (room_id, infra.id, infra.id)
                );
            }
            StoreError::AgentInactive(agent_id) => {
                assert_eq!((agent_id, target), (inactive.id, inactive.id));
            }
            StoreError::AgentNotFound(agent_id) => {
                assert_eq!((agent_id, target), (unknown, unknown));
            }
            error => panic!("unexpected target validation error: {error}"),
        }
        assert!(
            store
                .list_recent_room_messages(room_id, 50)
                .unwrap()
                .0
                .is_empty()
        );
    }
    assert_eq!(
        service
            .list_room_members(RoomRef::Id(room_id))
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn room_mention_resolution_preserves_body_and_deduplicates_targets_in_order() {
    let database = TestDatabase::new();
    let cashpoint = agent("cashpoint", "active");
    let pay = agent("pay", "active");
    let store = SqliteStore::open(database.path()).unwrap();
    for target in [&cashpoint, &pay] {
        store.insert_agent(target).unwrap();
    }
    let mut service = new_service(database.path());
    let room_id = room(&mut service, "VNA").await;
    for target in [&cashpoint, &pay] {
        add_member(&mut service, room_id, target.id).await;
    }
    let original = message(
        RoomMessageId::new(),
        room_id,
        MemberType::User,
        "july",
        "  @pay @cashpoint @pay check refund\nkeep this body",
        vec![],
        None,
        CREATED,
    );
    let names = vec!["pay".into(), "cashpoint".into(), "pay".into()];
    let saved = service
        .append_room_message_with_mentions(
            AppendRoomMessage {
                message: original.clone(),
            },
            &names,
        )
        .await
        .unwrap();
    assert_eq!(saved.body, original.body);
    assert_eq!(saved.id, original.id);
    assert_eq!(saved.mentions, vec![pay.id, cashpoint.id]);
    assert_eq!(
        service
            .append_room_message_with_mentions(AppendRoomMessage { message: original }, &names,)
            .await
            .unwrap(),
        saved
    );
    assert_eq!(
        service
            .list_recent_room_messages(room_id, 50)
            .await
            .unwrap()
            .0,
        vec![saved]
    );
}

#[tokio::test]
async fn room_mention_resolution_rejects_unknown_names_before_persistence() {
    let database = TestDatabase::new();
    let pay = agent("pay", "active");
    SqliteStore::open(database.path())
        .unwrap()
        .insert_agent(&pay)
        .unwrap();
    let mut service = new_service(database.path());
    let room_id = room(&mut service, "VNA").await;
    add_member(&mut service, room_id, pay.id).await;
    let original = message(
        RoomMessageId::new(),
        room_id,
        MemberType::User,
        "july",
        "@pay @unknown check refund",
        vec![],
        None,
        CREATED,
    );
    assert_eq!(
        service
            .append_room_message_with_mentions(
                AppendRoomMessage { message: original },
                &["pay".into(), "unknown".into()],
            )
            .await,
        Err(CollaborationError::AgentNotFound("unknown".into()))
    );
    assert!(
        service
            .list_recent_room_messages(room_id, 50)
            .await
            .unwrap()
            .0
            .is_empty()
    );
}

#[tokio::test]
async fn departed_room_target_rejects_new_messages_but_not_exact_history_replay() {
    let database = TestDatabase::new();
    let pay = agent("pay", "active");
    SqliteStore::open(database.path())
        .unwrap()
        .insert_agent(&pay)
        .unwrap();
    let mut service = new_service(database.path());
    let room_id = room(&mut service, "VNA").await;
    add_member(&mut service, room_id, pay.id).await;
    let original = message(
        RoomMessageId::new(),
        room_id,
        MemberType::User,
        "july",
        "@pay check refund",
        vec![pay.id],
        None,
        CREATED,
    );
    service
        .append_room_message(AppendRoomMessage {
            message: original.clone(),
        })
        .await
        .unwrap();
    service
        .remove_room_member(RemoveRoomMember {
            room: RoomRef::Id(room_id),
            agent: AgentRef::Id(pay.id),
            changed_at: CREATED.into(),
        })
        .await
        .unwrap();
    assert_eq!(
        service
            .append_room_message(AppendRoomMessage {
                message: original.clone()
            })
            .await
            .unwrap(),
        original
    );
    let mut fresh = original.clone();
    fresh.id = RoomMessageId::new();
    assert_eq!(
        service
            .append_room_message(AppendRoomMessage { message: fresh })
            .await,
        Err(CollaborationError::RoomMembershipRequired {
            room_id,
            agent_id: pay.id
        })
    );
    assert_eq!(
        service
            .list_recent_room_messages(room_id, 50)
            .await
            .unwrap()
            .0,
        vec![original]
    );
}
