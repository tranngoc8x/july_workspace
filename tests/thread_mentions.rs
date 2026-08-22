use july_workspace::domain::{
    Agent, AgentId, Conversation, ConversationId, ConversationKind, DeliveryStatus, DomainError,
    MemberType, Message, MessageId, Room, RoomId, WorkItemId,
};
use july_workspace::storage::{SqliteStore, StoreError};
use serde_json::json;
use std::path::{Path, PathBuf};

const CREATED: &str = "2026-08-20T10:00:00Z";
const LEFT: &str = "2026-08-20T11:00:00Z";
const MENTIONED: &str = "2026-08-20T12:00:00Z";
const CAPSULE: &str = "Goal: review only this thread";

struct TestDatabase {
    directory: PathBuf,
    path: PathBuf,
}

impl TestDatabase {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("july-thread-mention-{}", ulid::Ulid::generate()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("workspace.db");
        Self { directory, path }
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

struct Fixture {
    _database: TestDatabase,
    store: SqliteStore,
    room: Room,
    thread: Conversation,
    source: Agent,
    target: Agent,
}

fn agent(name: &str) -> Agent {
    Agent {
        id: AgentId::new(),
        name: name.into(),
        project_root: format!("/workspace/{name}"),
        transport_type: "acp".into(),
        transport_config: json!({}),
        status: "active".into(),
        metadata: json!({}),
        created_at: CREATED.into(),
        updated_at: CREATED.into(),
    }
}

fn setup(source_in_thread: bool, target_in_room: bool, target_in_thread: bool) -> Fixture {
    let database = TestDatabase::new();
    let mut store = SqliteStore::open(database.path()).unwrap();
    let room = Room {
        id: RoomId::new(),
        name: "mentions".into(),
        description: None,
        status: "active".into(),
        created_at: CREATED.into(),
        updated_at: CREATED.into(),
    };
    let thread = Conversation {
        id: ConversationId::new(),
        kind: ConversationKind::Thread,
        room_id: Some(room.id),
        title: Some("Atomic mentions".into()),
        goal: None,
        parent_conversation_id: None,
        origin_conversation_id: None,
        status: "open".into(),
        created_at: CREATED.into(),
        updated_at: CREATED.into(),
    };
    let source = agent("source");
    let target = agent("target");
    store.insert_room(&room).unwrap();
    store.insert_agent(&source).unwrap();
    store.insert_agent(&target).unwrap();
    store
        .add_room_member(room.id, source.id, None, CREATED)
        .unwrap();
    if target_in_room {
        store
            .add_room_member(room.id, target.id, None, CREATED)
            .unwrap();
    }
    let mut initial_agents = Vec::new();
    if source_in_thread {
        initial_agents.push(source.id);
    }
    if target_in_thread {
        initial_agents.push(target.id);
    }
    store
        .create_thread_with_primary_work(&thread, WorkItemId::new(), "tony", &initial_agents)
        .unwrap();
    Fixture {
        _database: database,
        store,
        room,
        thread,
        source,
        target,
    }
}

fn mention(fixture: &Fixture) -> Message {
    Message {
        id: MessageId::new(),
        conversation_id: fixture.thread.id,
        sender_type: MemberType::Agent,
        sender_id: fixture.source.id.to_string(),
        body: "@target please review".into(),
        reply_to: None,
        metadata: json!({"mention": fixture.target.id.to_string()}),
        created_at: MENTIONED.into(),
    }
}

fn target_members(fixture: &Fixture) -> Vec<july_workspace::domain::ConversationMember> {
    fixture
        .store
        .list_conversation_members(fixture.thread.id)
        .unwrap()
        .into_iter()
        .filter(|member| {
            member.member_type == MemberType::Agent
                && member.member_id == fixture.target.id.to_string()
        })
        .collect()
}

#[test]
fn mention_joins_target_and_persists_source_message() {
    let mut fixture = setup(true, true, false);
    let message = mention(&fixture);

    let (membership_changed, returned_delivery) = fixture
        .store
        .persist_thread_mention(&message, fixture.source.id, fixture.target.id, CAPSULE)
        .unwrap()
        .unwrap();
    assert!(membership_changed);

    let members = target_members(&fixture);
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].generation, 1);
    assert_eq!(members[0].joined_at, MENTIONED);
    assert_eq!(members[0].left_at, None);
    assert_eq!(
        fixture.store.get_message(message.id).unwrap(),
        Some(message.clone())
    );
    let delivery = fixture
        .store
        .get_message_delivery(message.id, fixture.target.id)
        .unwrap()
        .unwrap();
    assert_eq!(returned_delivery, delivery);
    assert_eq!(delivery.status, DeliveryStatus::Pending);
    assert_eq!(delivery.capsule.as_deref(), Some(CAPSULE));
    assert_eq!(delivery.capsule_delivered_at, None);

    assert_eq!(
        fixture
            .store
            .persist_thread_mention(&message, fixture.source.id, fixture.target.id, CAPSULE)
            .unwrap(),
        None
    );
    let members = target_members(&fixture);
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].generation, 1);
}

#[test]
fn mention_keeps_active_target_membership_and_persists_message() {
    let mut fixture = setup(true, true, true);
    let message = mention(&fixture);

    let (membership_changed, returned_delivery) = fixture
        .store
        .persist_thread_mention(&message, fixture.source.id, fixture.target.id, CAPSULE)
        .unwrap()
        .unwrap();
    assert!(!membership_changed);

    let members = target_members(&fixture);
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].generation, 1);
    assert_eq!(members[0].joined_at, CREATED);
    assert_eq!(
        fixture.store.get_message(message.id).unwrap(),
        Some(message.clone())
    );
    let delivery = fixture
        .store
        .get_message_delivery(message.id, fixture.target.id)
        .unwrap()
        .unwrap();
    assert_eq!(returned_delivery, delivery);
    assert_eq!(delivery.status, DeliveryStatus::Pending);
    assert_eq!(delivery.capsule, None);
}

#[test]
fn mention_rejoins_target_with_next_generation() {
    let mut fixture = setup(true, true, true);
    fixture
        .store
        .remove_thread_member(fixture.thread.id, fixture.target.id, LEFT)
        .unwrap();
    let message = mention(&fixture);

    let (membership_changed, returned_delivery) = fixture
        .store
        .persist_thread_mention(&message, fixture.source.id, fixture.target.id, CAPSULE)
        .unwrap()
        .unwrap();
    assert!(membership_changed);

    let members = target_members(&fixture);
    assert_eq!(members.len(), 2);
    assert_eq!(
        (members[0].generation, members[0].left_at.as_deref()),
        (1, Some(LEFT))
    );
    assert_eq!(
        (members[1].generation, members[1].joined_at.as_str()),
        (2, MENTIONED)
    );
    assert_eq!(members[1].left_at, None);
    assert_eq!(returned_delivery.capsule.as_deref(), Some(CAPSULE));
    assert_eq!(
        fixture
            .store
            .get_message_delivery(message.id, fixture.target.id)
            .unwrap()
            .unwrap()
            .capsule
            .as_deref(),
        Some(CAPSULE)
    );
}

#[test]
fn failed_retry_hydration_error_rolls_back_the_claim() {
    let mut fixture = setup(true, true, false);
    let message = mention(&fixture);
    fixture
        .store
        .persist_thread_mention(&message, fixture.source.id, fixture.target.id, CAPSULE)
        .unwrap();
    fixture
        .store
        .mark_delivery_failed(message.id, fixture.target.id, LEFT)
        .unwrap();
    let connection = rusqlite::Connection::open(fixture._database.path()).unwrap();
    connection
        .pragma_update(None, "ignore_check_constraints", true)
        .unwrap();
    connection
        .execute(
            "UPDATE messages SET metadata_json = 'not-json' WHERE id = ?1",
            [message.id.to_string()],
        )
        .unwrap();

    assert!(matches!(
        fixture.store.claim_failed_thread_mention_delivery(
            message.id,
            fixture.target.id,
            MENTIONED,
        ),
        Err(StoreError::Json(_))
    ));
    assert_eq!(
        fixture
            .store
            .get_message_delivery(message.id, fixture.target.id)
            .unwrap()
            .unwrap()
            .status,
        DeliveryStatus::Failed
    );
}

#[test]
fn mention_scope_rejections_leave_no_target_membership_or_message() {
    let mut missing_source = setup(false, true, false);
    let source_message = mention(&missing_source);
    assert!(matches!(
        missing_source.store.persist_thread_mention(
            &source_message,
            missing_source.source.id,
            missing_source.target.id,
            CAPSULE,
        ),
        Err(StoreError::ThreadMembershipRequired { thread_id, agent_id })
            if thread_id == missing_source.thread.id && agent_id == missing_source.source.id
    ));
    assert!(target_members(&missing_source).is_empty());
    assert_eq!(
        missing_source.store.get_message(source_message.id).unwrap(),
        None
    );

    let mut missing_target_room = setup(true, false, false);
    let target_message = mention(&missing_target_room);
    assert!(matches!(
        missing_target_room.store.persist_thread_mention(
            &target_message,
            missing_target_room.source.id,
            missing_target_room.target.id,
            CAPSULE,
        ),
        Err(StoreError::RoomMembershipRequired { room_id, agent_id })
            if room_id == missing_target_room.room.id && agent_id == missing_target_room.target.id
    ));
    assert!(target_members(&missing_target_room).is_empty());
    assert_eq!(
        missing_target_room
            .store
            .get_message(target_message.id)
            .unwrap(),
        None
    );

    let mut wrong_sender = setup(true, true, false);
    let mut sender_message = mention(&wrong_sender);
    sender_message.sender_id = wrong_sender.target.id.to_string();
    assert!(matches!(
        wrong_sender.store.persist_thread_mention(
            &sender_message,
            wrong_sender.source.id,
            wrong_sender.target.id,
            CAPSULE,
        ),
        Err(StoreError::MessageSenderMismatch(id)) if id == wrong_sender.source.id
    ));
    assert!(target_members(&wrong_sender).is_empty());
    assert_eq!(
        wrong_sender.store.get_message(sender_message.id).unwrap(),
        None
    );

    let mut invalid_message = setup(true, true, false);
    let mut blank_sender = mention(&invalid_message);
    blank_sender.sender_id.clear();
    assert!(matches!(
        invalid_message.store.persist_thread_mention(
            &blank_sender,
            invalid_message.source.id,
            invalid_message.target.id,
            CAPSULE,
        ),
        Err(StoreError::Domain(DomainError::EmptyField(
            "message.sender_id"
        )))
    ));
    assert!(target_members(&invalid_message).is_empty());
    assert_eq!(
        invalid_message.store.get_message(blank_sender.id).unwrap(),
        None
    );
}

#[test]
fn message_conflict_rolls_back_target_membership() {
    let mut fixture = setup(true, true, false);
    let message = mention(&fixture);
    let existing = Message {
        body: "existing body".into(),
        ..message.clone()
    };
    fixture.store.insert_message(&existing).unwrap();

    assert!(matches!(
        fixture
            .store
            .persist_thread_mention(&message, fixture.source.id, fixture.target.id, CAPSULE),
        Err(StoreError::MessageConflict { id }) if id == message.id
    ));

    assert!(target_members(&fixture).is_empty());
    assert_eq!(
        fixture.store.get_message(message.id).unwrap(),
        Some(existing)
    );
    assert_eq!(
        fixture
            .store
            .get_message_delivery(message.id, fixture.target.id)
            .unwrap(),
        None
    );
}
