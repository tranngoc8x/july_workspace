//! Phase 6.5.4 — a settled decision becomes executable work explicitly,
//! auditably, and only once.
use july_workspace::domain::{
    Agent, AgentId, Conversation, ConversationId, ConversationKind, Decision, DecisionId,
    DecisionOutcome, DecisionOwner, DecisionStatus, DecisionType, DecisionWork, Room, RoomId,
    WorkItemId, WorkStatus,
};
use july_workspace::storage::{SqliteStore, StoreError};
use serde_json::json;
use std::path::{Path, PathBuf};

const CREATED: &str = "2026-08-25T08:00:00Z";
const DECIDED: &str = "2026-08-25T09:00:00Z";
const CONVERTED: &str = "2026-08-25T10:00:00Z";

struct TestDatabase {
    directory: PathBuf,
    path: PathBuf,
}

impl TestDatabase {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("july-decision-work-{}", ulid::Ulid::generate()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("workspace.db");
        Self { directory, path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn store(&self) -> SqliteStore {
        SqliteStore::open(&self.path).unwrap()
    }
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn agent(name: &str) -> Agent {
    Agent {
        id: AgentId::new(),
        name: format!("{name}-{}", ulid::Ulid::generate()),
        project_root: format!("/workspace/{name}"),
        transport_type: "acp".into(),
        transport_config: json!({}),
        status: "active".into(),
        metadata: json!({}),
        created_at: CREATED.into(),
        updated_at: CREATED.into(),
    }
}

struct Seeded {
    thread_id: ConversationId,
    cashpoint: AgentId,
    pay: AgentId,
    outsider: AgentId,
}

fn seed(path: &Path) -> Seeded {
    let mut store = SqliteStore::open(path).unwrap();
    let room = Room {
        id: RoomId::new(),
        name: format!("Payments {}", ulid::Ulid::generate()),
        description: None,
        status: "active".into(),
        created_at: CREATED.into(),
        updated_at: CREATED.into(),
    };
    let thread = Conversation {
        id: ConversationId::new(),
        kind: ConversationKind::Thread,
        room_id: Some(room.id),
        title: Some("Callback retry".into()),
        goal: Some("Ship the retry fix".into()),
        parent_conversation_id: None,
        origin_conversation_id: None,
        status: "open".into(),
        created_at: CREATED.into(),
        updated_at: CREATED.into(),
    };
    let cashpoint = agent("cashpoint");
    let pay = agent("pay");
    let outsider = agent("outsider");
    store.insert_room(&room).unwrap();
    for member in [&cashpoint, &pay, &outsider] {
        store.insert_agent(member).unwrap();
        store
            .add_room_member(room.id, member.id, None, CREATED)
            .unwrap();
    }
    store
        .create_thread_with_primary_work(
            &thread,
            WorkItemId::new(),
            "tony",
            &[cashpoint.id, pay.id],
        )
        .unwrap();
    Seeded {
        thread_id: thread.id,
        cashpoint: cashpoint.id,
        pay: pay.id,
        outsider: outsider.id,
    }
}

fn pending_decision(seeded: &Seeded) -> Decision {
    Decision {
        id: DecisionId::new(),
        thread_id: seeded.thread_id,
        decision_type: DecisionType::Technical,
        title: "Retry ownership".into(),
        decision: None,
        reason: None,
        selected_proposal_id: None,
        alternatives: Vec::new(),
        evidence: Vec::new(),
        participants: vec![seeded.cashpoint, seeded.pay],
        decision_owner: DecisionOwner::User,
        status: DecisionStatus::Pending,
        supersedes_decision_id: None,
        created_at: CREATED.into(),
        updated_at: CREATED.into(),
    }
}

fn decided(store: &mut SqliteStore, seeded: &Seeded) -> Decision {
    let recorded = store.record_decision(&pending_decision(seeded)).unwrap();
    store
        .decide(
            recorded.id,
            &DecisionOutcome::new(DecisionOwner::User, "Pay retries, Cashpoint tests"),
            DECIDED,
        )
        .unwrap()
}

#[test]
fn a_decision_generates_owned_work_with_its_dependency() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut store = database.store();
    let decision = decided(&mut store, &seeded);

    let implement = DecisionWork::new("Implement callback retry").owned_by(seeded.pay);
    let verify = DecisionWork::new("Integration test for retry")
        .owned_by(seeded.cashpoint)
        .after(implement.work_id);

    let created = store
        .convert_decision_to_work(
            decision.id,
            std::slice::from_ref(&implement)
                .iter()
                .cloned()
                .chain([verify.clone()])
                .collect::<Vec<_>>()
                .as_slice(),
            CONVERTED,
        )
        .unwrap();

    assert_eq!(created.len(), 2);
    assert!(created.iter().all(|work| work.status == WorkStatus::Open));
    assert!(created.iter().all(|work| !work.is_primary));
    assert_eq!(created[0].owner_agent_id, Some(seeded.pay));
    assert_eq!(created[1].owner_agent_id, Some(seeded.cashpoint));
    assert_eq!(created[0].conversation_id, seeded.thread_id);

    let dependency = store
        .get_work_dependency(implement.work_id, verify.work_id)
        .unwrap()
        .expect("the split work waits on the implementation");
    assert_eq!(dependency.created_at, CONVERTED);

    let linked = store.list_decision_work(decision.id).unwrap();
    assert_eq!(linked.len(), 2, "the conversion stays auditable");
}

#[test]
fn converting_the_same_decision_twice_creates_nothing_new() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut store = database.store();
    let decision = decided(&mut store, &seeded);
    let items = vec![DecisionWork::new("Implement callback retry").owned_by(seeded.pay)];

    let first = store
        .convert_decision_to_work(decision.id, &items, CONVERTED)
        .unwrap();
    let second = store
        .convert_decision_to_work(decision.id, &items, CONVERTED)
        .unwrap();

    assert_eq!(first, second);
    assert_eq!(store.list_decision_work(decision.id).unwrap().len(), 1);
}

#[test]
fn a_replay_with_changed_content_is_a_conflict() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut store = database.store();
    let decision = decided(&mut store, &seeded);
    let item = DecisionWork::new("Implement callback retry").owned_by(seeded.pay);
    store
        .convert_decision_to_work(decision.id, std::slice::from_ref(&item), CONVERTED)
        .unwrap();

    let error = store
        .convert_decision_to_work(
            decision.id,
            &[DecisionWork {
                title: "Something else entirely".into(),
                ..item.clone()
            }],
            CONVERTED,
        )
        .unwrap_err();

    assert!(
        matches!(error, StoreError::DecisionWorkConflict { work_id, .. } if work_id == item.work_id),
        "unexpected error: {error}"
    );
}

#[test]
fn an_undecided_decision_generates_no_work() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut store = database.store();
    let pending = store.record_decision(&pending_decision(&seeded)).unwrap();

    let error = store
        .convert_decision_to_work(
            pending.id,
            &[DecisionWork::new("Premature work")],
            CONVERTED,
        )
        .unwrap_err();

    assert!(
        matches!(
            error,
            StoreError::DecisionNotDecided {
                status: DecisionStatus::Pending,
                ..
            }
        ),
        "unexpected error: {error}"
    );
    assert!(store.list_decision_work(pending.id).unwrap().is_empty());
}

#[test]
fn generated_work_cannot_be_owned_from_outside_the_thread() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut store = database.store();
    let decision = decided(&mut store, &seeded);

    let error = store
        .convert_decision_to_work(
            decision.id,
            &[DecisionWork::new("Outsider work").owned_by(seeded.outsider)],
            CONVERTED,
        )
        .unwrap_err();

    assert!(
        matches!(error, StoreError::WorkOwnerScopeRequired { owner_agent_id, .. } if owner_agent_id == seeded.outsider),
        "unexpected error: {error}"
    );
    assert!(store.list_decision_work(decision.id).unwrap().is_empty());
}

#[test]
fn a_decision_cannot_claim_work_it_did_not_generate() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut store = database.store();
    let first = decided(&mut store, &seeded);
    let second = decided(&mut store, &seeded);
    let item = DecisionWork::new("Implement callback retry").owned_by(seeded.pay);
    store
        .convert_decision_to_work(first.id, std::slice::from_ref(&item), CONVERTED)
        .unwrap();

    let error = store
        .convert_decision_to_work(second.id, std::slice::from_ref(&item), CONVERTED)
        .unwrap_err();

    assert!(
        matches!(error, StoreError::DecisionWorkConflict { decision_id, .. } if decision_id == second.id),
        "unexpected error: {error}"
    );
}

#[test]
fn generated_work_and_its_links_survive_restart() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let decision_id = {
        let mut store = database.store();
        let decision = decided(&mut store, &seeded);
        store
            .convert_decision_to_work(
                decision.id,
                &[DecisionWork::new("Implement callback retry").owned_by(seeded.pay)],
                CONVERTED,
            )
            .unwrap();
        decision.id
    };

    let store = database.store();
    let work = store.list_decision_work(decision_id).unwrap();
    assert_eq!(work.len(), 1);
    assert_eq!(work[0].title, "Implement callback retry");
    assert_eq!(work[0].owner_agent_id, Some(seeded.pay));
}
