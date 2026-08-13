use july_workspace::domain::{
    Agent, AgentId, Conversation, ConversationId, ConversationKind, MemberType, Room, RoomId,
    WorkItem, WorkItemId, WorkStatus,
};
use july_workspace::storage::{SqliteStore, StoreError};
use serde_json::json;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread as std_thread;
use std::time::Duration;
use ulid::Ulid;

const CREATED: &str = "2026-08-13T08:00:00Z";
const LEFT: &str = "2026-08-13T09:00:00Z";
const REJOINED: &str = "2026-08-13T10:00:00Z";

struct TestDatabase {
    directory: PathBuf,
    path: PathBuf,
}

impl TestDatabase {
    fn new() -> Self {
        let directory = env::temp_dir().join(format!("july-phase4-{}", Ulid::generate()));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("workspace.db");
        Self { directory, path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
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

fn room(name: &str) -> Room {
    Room {
        id: RoomId::new(),
        name: name.into(),
        description: None,
        status: "active".into(),
        created_at: CREATED.into(),
        updated_at: CREATED.into(),
    }
}

fn thread(room_id: RoomId, title: &str) -> Conversation {
    Conversation {
        id: ConversationId::new(),
        kind: ConversationKind::Thread,
        room_id: Some(room_id),
        title: Some(title.into()),
        goal: Some(format!("Goal for {title}")),
        parent_conversation_id: None,
        origin_conversation_id: None,
        status: "open".into(),
        created_at: CREATED.into(),
        updated_at: CREATED.into(),
    }
}

fn persist_thread(store: &mut SqliteStore, conversation: &Conversation) {
    store
        .create_thread_with_primary_work(conversation, WorkItemId::new(), "tony", &[])
        .unwrap();
}

#[test]
fn rooms_and_threads_are_listed_in_their_own_scope() {
    let database = TestDatabase::new();
    let mut store = SqliteStore::open(database.path()).unwrap();
    let alpha = room("alpha");
    let beta = room("beta");
    let alpha_thread = thread(alpha.id, "alpha work");
    let beta_thread = thread(beta.id, "beta work");
    let dm = Conversation {
        id: ConversationId::new(),
        kind: ConversationKind::Dm,
        room_id: None,
        title: None,
        goal: None,
        parent_conversation_id: None,
        origin_conversation_id: None,
        status: "open".into(),
        created_at: CREATED.into(),
        updated_at: CREATED.into(),
    };

    store.insert_room(&beta).unwrap();
    store.insert_room(&alpha).unwrap();
    persist_thread(&mut store, &alpha_thread);
    persist_thread(&mut store, &beta_thread);
    store.insert_conversation(&dm).unwrap();

    assert_eq!(store.list_rooms().unwrap(), vec![alpha.clone(), beta]);
    assert_eq!(store.get_room_by_name("alpha").unwrap(), Some(alpha));
    assert_eq!(
        store.list_threads(alpha_thread.room_id.unwrap()).unwrap(),
        vec![alpha_thread.clone()]
    );
    assert_eq!(
        store.get_thread(alpha_thread.id).unwrap(),
        Some(alpha_thread)
    );
    assert_eq!(store.get_thread(dm.id).unwrap(), None);
}

#[test]
fn raw_thread_insert_requires_the_primary_work_aggregate() {
    let database = TestDatabase::new();
    let store = SqliteStore::open(database.path()).unwrap();
    let room = room("aggregate-only");
    let conversation = thread(room.id, "cannot bypass");
    store.insert_room(&room).unwrap();

    assert!(matches!(
        store.insert_conversation(&conversation),
        Err(StoreError::ThreadAggregateRequired(id)) if id == conversation.id
    ));
    assert_eq!(store.get_conversation(conversation.id).unwrap(), None);
}

#[test]
fn room_creation_reports_id_and_name_conflicts_without_overwriting() {
    let database = TestDatabase::new();
    let mut store = SqliteStore::open(database.path()).unwrap();
    let original = room("operations");
    store.create_room(&original).unwrap();

    let mut same_id = room("different-name");
    same_id.id = original.id;
    assert!(matches!(
        store.create_room(&same_id),
        Err(StoreError::RoomIdConflict(id)) if id == original.id
    ));

    let same_name = room("operations");
    assert!(matches!(
        store.create_room(&same_name),
        Err(StoreError::RoomNameConflict(name)) if name == "operations"
    ));
    assert_eq!(store.list_rooms().unwrap(), vec![original]);
}

#[test]
fn room_membership_is_idempotent_and_retains_generations() {
    let database = TestDatabase::new();
    let mut store = SqliteStore::open(database.path()).unwrap();
    let room = room("operations");
    let worker = agent("worker");
    store.insert_room(&room).unwrap();
    store.insert_agent(&worker).unwrap();

    assert!(
        store
            .add_room_member(room.id, worker.id, Some("reviewer"), CREATED)
            .unwrap()
    );
    assert!(
        !store
            .add_room_member(room.id, worker.id, Some("owner"), LEFT)
            .unwrap()
    );
    assert!(store.remove_room_member(room.id, worker.id, LEFT).unwrap());
    assert!(
        !store
            .remove_room_member(room.id, worker.id, REJOINED)
            .unwrap()
    );
    assert!(
        store
            .add_room_member(room.id, worker.id, None, REJOINED)
            .unwrap()
    );

    let history = store.list_room_members(room.id).unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(
        (history[0].generation, history[0].role.as_deref()),
        (1, Some("reviewer"))
    );
    assert_eq!(history[0].joined_at, CREATED);
    assert_eq!(history[0].left_at.as_deref(), Some(LEFT));
    assert_eq!(
        (history[1].generation, history[1].joined_at.as_str()),
        (2, REJOINED)
    );
    assert_eq!(history[1].left_at, None);
}

#[test]
fn already_active_membership_retry_is_unchanged_after_agent_deactivates() {
    let database = TestDatabase::new();
    let mut store = SqliteStore::open(database.path()).unwrap();
    let room = room("closing-room");
    let mut worker = agent("closing-worker");
    let conversation = thread(room.id, "closing-thread");
    store.insert_room(&room).unwrap();
    store.insert_agent(&worker).unwrap();
    persist_thread(&mut store, &conversation);
    store
        .add_room_member(room.id, worker.id, None, CREATED)
        .unwrap();
    store
        .add_thread_member(conversation.id, worker.id, CREATED)
        .unwrap();

    worker.status = "disabled".into();
    worker.updated_at = LEFT.into();
    store.update_agent(&worker).unwrap();

    assert!(
        !store
            .add_room_member(room.id, worker.id, None, LEFT)
            .unwrap()
    );
    assert!(
        !store
            .add_thread_member(conversation.id, worker.id, LEFT)
            .unwrap()
    );
    assert_eq!(
        store.list_room_members(room.id).unwrap()[0].joined_at,
        CREATED
    );
    assert_eq!(
        store.list_conversation_members(conversation.id).unwrap()[0].joined_at,
        CREATED
    );
}

#[test]
fn membership_guards_reject_inactive_or_out_of_scope_agents() {
    let database = TestDatabase::new();
    let mut store = SqliteStore::open(database.path()).unwrap();
    let active_room = room("active");
    let other_room = room("other");
    let mut inactive_room = room("inactive");
    inactive_room.status = "closed".into();
    let active = agent("active-agent");
    let mut inactive = agent("inactive-agent");
    inactive.status = "disabled".into();
    let conversation = thread(active_room.id, "isolated");
    for room in [&active_room, &other_room, &inactive_room] {
        store.insert_room(room).unwrap();
    }
    store.insert_agent(&active).unwrap();
    store.insert_agent(&inactive).unwrap();
    persist_thread(&mut store, &conversation);

    assert!(matches!(
        store.add_room_member(inactive_room.id, active.id, None, CREATED),
        Err(StoreError::RoomInactive(id)) if id == inactive_room.id
    ));
    assert!(matches!(
        store.add_room_member(active_room.id, inactive.id, None, CREATED),
        Err(StoreError::AgentInactive(id)) if id == inactive.id
    ));
    assert!(matches!(
        store.add_thread_member(conversation.id, active.id, CREATED),
        Err(StoreError::RoomMembershipRequired { room_id, agent_id })
            if room_id == active_room.id && agent_id == active.id
    ));
    store
        .add_room_member(other_room.id, active.id, None, CREATED)
        .unwrap();
    assert!(matches!(
        store.add_thread_member(conversation.id, active.id, CREATED),
        Err(StoreError::RoomMembershipRequired { .. })
    ));
}

#[test]
fn thread_membership_rejoin_preserves_history_and_blocks_room_removal() {
    let database = TestDatabase::new();
    let mut store = SqliteStore::open(database.path()).unwrap();
    let room = room("delivery");
    let worker = agent("worker");
    let conversation = thread(room.id, "ship");
    store.insert_room(&room).unwrap();
    store.insert_agent(&worker).unwrap();
    persist_thread(&mut store, &conversation);
    store
        .add_room_member(room.id, worker.id, None, CREATED)
        .unwrap();

    assert!(
        store
            .add_thread_member(conversation.id, worker.id, CREATED)
            .unwrap()
    );
    assert!(
        !store
            .add_thread_member(conversation.id, worker.id, LEFT)
            .unwrap()
    );
    assert!(matches!(
        store.remove_room_member(room.id, worker.id, LEFT),
        Err(StoreError::RoomRemovalBlocked { room_id, agent_id })
            if room_id == room.id && agent_id == worker.id
    ));
    assert!(
        store
            .remove_thread_member(conversation.id, worker.id, LEFT)
            .unwrap()
    );
    assert!(
        !store
            .remove_thread_member(conversation.id, worker.id, REJOINED)
            .unwrap()
    );
    assert!(
        store
            .add_thread_member(conversation.id, worker.id, REJOINED)
            .unwrap()
    );

    let history: Vec<_> = store
        .list_conversation_members(conversation.id)
        .unwrap()
        .into_iter()
        .filter(|member| member.member_type == MemberType::Agent)
        .collect();
    assert_eq!(history.len(), 2);
    assert_eq!(
        (history[0].generation, history[0].joined_at.as_str()),
        (1, CREATED)
    );
    assert_eq!(history[0].left_at.as_deref(), Some(LEFT));
    assert_eq!(
        (history[1].generation, history[1].joined_at.as_str()),
        (2, REJOINED)
    );
    assert_eq!(history[1].left_at, None);
}

#[test]
fn thread_primary_work_creation_is_atomic_and_deduplicates_initial_agents() {
    let database = TestDatabase::new();
    let mut store = SqliteStore::open(database.path()).unwrap();
    let room = room("project");
    let first = agent("first");
    let second = agent("second");
    let conversation = thread(room.id, "Implement Phase 4");
    let work_id = WorkItemId::new();
    store.insert_room(&room).unwrap();
    for agent in [&first, &second] {
        store.insert_agent(agent).unwrap();
        store
            .add_room_member(room.id, agent.id, None, CREATED)
            .unwrap();
    }

    let work = store
        .create_thread_with_primary_work(
            &conversation,
            work_id,
            "tony",
            &[first.id, first.id, second.id],
        )
        .unwrap();

    assert_eq!(
        store.get_thread(conversation.id).unwrap(),
        Some(conversation.clone())
    );
    let members = store.list_conversation_members(conversation.id).unwrap();
    assert_eq!(members.len(), 3);
    assert!(
        members
            .iter()
            .any(|member| { member.member_type == MemberType::User && member.member_id == "tony" })
    );
    assert_eq!(
        members
            .iter()
            .filter(|m| m.member_type == MemberType::Agent)
            .count(),
        2
    );
    assert_eq!(work.id, work_id);
    assert_eq!(work.conversation_id, conversation.id);
    assert_eq!(work.title, conversation.title.clone().unwrap());
    assert_eq!(work.goal, conversation.goal);
    assert_eq!(work.status, WorkStatus::Open);
    assert_eq!(work.owner_agent_id, None);
    assert!(work.is_primary);
    assert_eq!(store.get_work_item(work_id).unwrap(), Some(work));
}

#[test]
fn thread_creation_serializes_with_an_existing_immediate_writer() {
    let database = TestDatabase::new();
    let store = SqliteStore::open(database.path()).unwrap();
    let room = room("contention");
    let conversation = thread(room.id, "serialized create");
    let work_id = WorkItemId::new();
    store.insert_room(&room).unwrap();
    drop(store);
    let mut creator_store = SqliteStore::open(database.path()).unwrap();

    let path = database.path().to_path_buf();
    let room_id = room.id;
    let (held, ready) = mpsc::sync_channel(0);
    let (release, released) = mpsc::sync_channel(0);
    let holder = std_thread::spawn(move || {
        let connection = rusqlite::Connection::open(path).unwrap();
        connection.execute_batch("BEGIN IMMEDIATE").unwrap();
        connection
            .execute(
                "UPDATE rooms SET description = 'contender' WHERE id = ?1",
                [room_id.to_string()],
            )
            .unwrap();
        held.send(()).unwrap();
        released.recv().unwrap();
        connection.execute_batch("COMMIT").unwrap();
    });
    ready.recv().unwrap();

    let expected = conversation.clone();
    let (finished, result) = mpsc::sync_channel(0);
    let creator = std_thread::spawn(move || {
        finished
            .send(creator_store.create_thread_with_primary_work(&expected, work_id, "tony", &[]))
            .unwrap();
    });
    assert!(matches!(
        result.recv_timeout(Duration::from_millis(50)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    release.send(()).unwrap();
    result
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    holder.join().unwrap();
    creator.join().unwrap();

    let store = SqliteStore::open(database.path()).unwrap();
    assert_eq!(
        store.get_thread(conversation.id).unwrap(),
        Some(conversation)
    );
    assert!(store.get_work_item(work_id).unwrap().unwrap().is_primary);
}

#[test]
fn thread_creation_conflicts_and_validation_roll_back_the_entire_aggregate() {
    let database = TestDatabase::new();
    let mut store = SqliteStore::open(database.path()).unwrap();
    let room = room("project");
    let member = agent("member");
    let outsider = agent("outsider");
    store.insert_room(&room).unwrap();
    store.insert_agent(&member).unwrap();
    store.insert_agent(&outsider).unwrap();
    store
        .add_room_member(room.id, member.id, None, CREATED)
        .unwrap();

    let rejected = thread(room.id, "rejected");
    let rejected_work_id = WorkItemId::new();
    assert!(matches!(
        store.create_thread_with_primary_work(
            &rejected,
            rejected_work_id,
            "tony",
            &[member.id, outsider.id],
        ),
        Err(StoreError::RoomMembershipRequired { agent_id, .. }) if agent_id == outsider.id
    ));
    assert_eq!(store.get_conversation(rejected.id).unwrap(), None);
    assert_eq!(store.get_work_item(rejected_work_id).unwrap(), None);

    let existing_dm = Conversation {
        id: ConversationId::new(),
        kind: ConversationKind::Dm,
        room_id: None,
        title: None,
        goal: None,
        parent_conversation_id: None,
        origin_conversation_id: None,
        status: "open".into(),
        created_at: CREATED.into(),
        updated_at: CREATED.into(),
    };
    let occupied_work_id = WorkItemId::new();
    let occupied_work = WorkItem {
        id: occupied_work_id,
        conversation_id: existing_dm.id,
        title: "occupied".into(),
        goal: None,
        status: WorkStatus::Open,
        owner_agent_id: None,
        is_primary: false,
        created_at: CREATED.into(),
        updated_at: CREATED.into(),
        completed_at: None,
    };
    store.insert_conversation(&existing_dm).unwrap();
    store.insert_work_item(&occupied_work).unwrap();
    let conflicted = thread(room.id, "conflicted");
    assert!(matches!(
        store.create_thread_with_primary_work(
            &conflicted,
            occupied_work_id,
            "tony",
            &[member.id],
        ),
        Err(StoreError::PrimaryWorkIdConflict(id)) if id == occupied_work_id
    ));
    assert_eq!(store.get_conversation(conflicted.id).unwrap(), None);
    assert!(
        store
            .list_conversation_members(conflicted.id)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store.get_work_item(occupied_work_id).unwrap(),
        Some(occupied_work)
    );

    let existing_thread = thread(room.id, "existing");
    persist_thread(&mut store, &existing_thread);
    let new_work_id = WorkItemId::new();
    assert!(matches!(
        store.create_thread_with_primary_work(
            &existing_thread,
            new_work_id,
            "tony",
            &[member.id],
        ),
        Err(StoreError::ThreadIdConflict(id)) if id == existing_thread.id
    ));
    assert_eq!(store.get_work_item(new_work_id).unwrap(), None);
}
