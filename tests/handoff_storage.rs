//! Phase 6.5.1 — ownership negotiation is durable, evidence-backed, and never
//! reassigns work that the target agent rejected.
use july_workspace::domain::{
    Agent, AgentId, Conversation, ConversationId, ConversationKind, Handoff, HandoffDecision,
    HandoffResponse, HandoffStatus, Room, RoomId, WorkItem, WorkItemId, WorkStatus,
};
use july_workspace::storage::{SqliteStore, StoreError};
use serde_json::json;
use std::path::{Path, PathBuf};

const CREATED: &str = "2026-08-25T08:00:00Z";
const ANSWERED: &str = "2026-08-25T09:00:00Z";
const CLOSED: &str = "2026-08-25T10:00:00Z";

struct TestDatabase {
    directory: PathBuf,
    path: PathBuf,
}

impl TestDatabase {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("july-handoff-{}", ulid::Ulid::generate()));
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
    work_id: WorkItemId,
    agent_order: AgentId,
    pay: AgentId,
    infra: AgentId,
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
        goal: Some("Decide who owns callback retry".into()),
        parent_conversation_id: None,
        origin_conversation_id: None,
        status: "open".into(),
        created_at: CREATED.into(),
        updated_at: CREATED.into(),
    };
    let agent_order = agent("agent_order");
    let pay = agent("pay");
    let infra = agent("infra");
    let outsider = agent("outsider");
    store.insert_room(&room).unwrap();
    for member in [&agent_order, &pay, &infra, &outsider] {
        store.insert_agent(member).unwrap();
        store
            .add_room_member(room.id, member.id, None, CREATED)
            .unwrap();
    }
    let work_id = WorkItemId::new();
    store
        .create_thread_with_primary_work(
            &thread,
            work_id,
            "tony",
            &[agent_order.id, pay.id, infra.id],
        )
        .unwrap();
    Seeded {
        thread_id: thread.id,
        work_id,
        agent_order: agent_order.id,
        pay: pay.id,
        infra: infra.id,
        outsider: outsider.id,
    }
}

fn proposal(seeded: &Seeded) -> Handoff {
    Handoff {
        id: Default::default(),
        thread_id: seeded.thread_id,
        work_id: seeded.work_id,
        from_agent_id: seeded.agent_order,
        to_agent_id: seeded.pay,
        status: HandoffStatus::Proposed,
        reason: Some("Pay owns delivery".into()),
        evidence: vec!["src/payment/callback.rs".into()],
        owned_scope: Vec::new(),
        rejected_scope: Vec::new(),
        proposed_owner_id: None,
        round_count: 0,
        decision_id: None,
        created_at: CREATED.into(),
        updated_at: CREATED.into(),
    }
}

#[test]
fn accepting_a_handoff_transfers_work_ownership() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut store = database.store();
    let handoff = store.propose_handoff(&proposal(&seeded)).unwrap();
    assert_eq!(
        store
            .get_work_item(seeded.work_id)
            .unwrap()
            .unwrap()
            .owner_agent_id,
        None
    );

    let accepted = store
        .respond_to_handoff(handoff.id, &HandoffResponse::accept(seeded.pay), ANSWERED)
        .unwrap();

    assert_eq!(accepted.status, HandoffStatus::Accepted);
    assert_eq!(accepted.round_count, 0);
    assert_eq!(accepted.updated_at, ANSWERED);
    let work = store.get_work_item(seeded.work_id).unwrap().unwrap();
    assert_eq!(work.owner_agent_id, Some(seeded.pay));
    assert_eq!(work.updated_at, ANSWERED);
}

#[test]
fn rejecting_a_handoff_keeps_the_owner_and_records_evidence() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut store = database.store();
    let handoff = store.propose_handoff(&proposal(&seeded)).unwrap();

    let rejected = store
        .respond_to_handoff(
            handoff.id,
            &HandoffResponse::reject(
                seeded.pay,
                "Pay returns transaction_ref per the current contract",
                vec![
                    "src/payment/callback.rs".into(),
                    "test:payment_contract".into(),
                ],
            )
            .with_proposed_owner(seeded.agent_order),
            ANSWERED,
        )
        .unwrap();

    assert_eq!(rejected.status, HandoffStatus::Rejected);
    assert_eq!(rejected.round_count, 1);
    assert_eq!(rejected.proposed_owner_id, Some(seeded.agent_order));
    assert_eq!(rejected.evidence.len(), 2);
    assert_eq!(
        store
            .get_work_item(seeded.work_id)
            .unwrap()
            .unwrap()
            .owner_agent_id,
        None,
        "rejected work must not be silently reassigned"
    );
}

#[test]
fn a_rejection_without_evidence_is_refused() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut store = database.store();
    let handoff = store.propose_handoff(&proposal(&seeded)).unwrap();

    let error = store
        .respond_to_handoff(
            handoff.id,
            &HandoffResponse::reject(seeded.pay, "not mine", Vec::new()),
            ANSWERED,
        )
        .unwrap_err();

    assert!(
        matches!(error, StoreError::Domain(_)),
        "unexpected error: {error}"
    );
    assert_eq!(
        store.get_handoff(handoff.id).unwrap().unwrap().status,
        HandoffStatus::Proposed
    );
}

#[test]
fn partial_ownership_records_both_scopes() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut store = database.store();
    let handoff = store.propose_handoff(&proposal(&seeded)).unwrap();

    let partial = store
        .respond_to_handoff(
            handoff.id,
            &HandoffResponse::partial(
                seeded.pay,
                "Pay can add the field, mapping stays in Cashpoint",
                vec!["add new callback field".into()],
                vec!["map callback into voucher record".into()],
            )
            .with_proposed_owner(seeded.agent_order),
            ANSWERED,
        )
        .unwrap();

    assert_eq!(partial.status, HandoffStatus::Partial);
    assert_eq!(
        partial.owned_scope,
        vec!["add new callback field".to_owned()]
    );
    assert_eq!(
        partial.rejected_scope,
        vec!["map callback into voucher record".to_owned()]
    );
    assert_eq!(
        store
            .get_work_item(seeded.work_id)
            .unwrap()
            .unwrap()
            .owner_agent_id,
        None,
        "a split needs an explicit decision before ownership moves"
    );
}

#[test]
fn a_partial_response_without_both_scopes_is_refused() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut store = database.store();
    let handoff = store.propose_handoff(&proposal(&seeded)).unwrap();

    let error = store
        .respond_to_handoff(
            handoff.id,
            &HandoffResponse::partial(seeded.pay, "half", vec!["only mine".into()], Vec::new()),
            ANSWERED,
        )
        .unwrap_err();

    assert!(
        matches!(error, StoreError::Domain(_)),
        "unexpected: {error}"
    );
}

#[test]
fn only_the_target_agent_may_respond() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut store = database.store();
    let handoff = store.propose_handoff(&proposal(&seeded)).unwrap();

    let error = store
        .respond_to_handoff(handoff.id, &HandoffResponse::accept(seeded.infra), ANSWERED)
        .unwrap_err();

    assert!(
        matches!(error, StoreError::HandoffRespondentMismatch { expected, .. } if expected == seeded.pay),
        "unexpected error: {error}"
    );
}

#[test]
fn a_second_response_to_the_same_proposal_is_refused() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut store = database.store();
    let handoff = store.propose_handoff(&proposal(&seeded)).unwrap();
    store
        .respond_to_handoff(handoff.id, &HandoffResponse::accept(seeded.pay), ANSWERED)
        .unwrap();

    let error = store
        .respond_to_handoff(
            handoff.id,
            &HandoffResponse::reject(seeded.pay, "changed my mind", vec!["log".into()]),
            CLOSED,
        )
        .unwrap_err();

    assert!(
        matches!(
            error,
            StoreError::InvalidHandoffTransition {
                from: HandoffStatus::Accepted,
                to: HandoffStatus::Rejected,
                ..
            }
        ),
        "unexpected error: {error}"
    );
}

#[test]
fn the_source_agent_resolves_a_rejection_it_accepts() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut store = database.store();
    let handoff = store.propose_handoff(&proposal(&seeded)).unwrap();
    store
        .respond_to_handoff(
            handoff.id,
            &HandoffResponse::reject(
                seeded.pay,
                "contract is correct",
                vec!["test:contract".into()],
            ),
            ANSWERED,
        )
        .unwrap();

    let resolved = store
        .resolve_handoff(handoff.id, seeded.agent_order, CLOSED)
        .unwrap();
    assert_eq!(resolved.status, HandoffStatus::Resolved);
    assert!(!resolved.status.is_open());

    let error = store
        .resolve_handoff(handoff.id, seeded.agent_order, CLOSED)
        .unwrap_err();
    assert!(
        matches!(error, StoreError::InvalidHandoffTransition { .. }),
        "unexpected error: {error}"
    );
}

#[test]
fn only_the_source_agent_may_resolve_or_cancel() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut store = database.store();
    let handoff = store.propose_handoff(&proposal(&seeded)).unwrap();

    let error = store
        .cancel_handoff(handoff.id, seeded.pay, CLOSED)
        .unwrap_err();
    assert!(
        matches!(error, StoreError::HandoffSourceMismatch { expected, .. } if expected == seeded.agent_order),
        "unexpected error: {error}"
    );

    let cancelled = store
        .cancel_handoff(handoff.id, seeded.agent_order, CLOSED)
        .unwrap();
    assert_eq!(cancelled.status, HandoffStatus::Cancelled);
}

#[test]
fn a_work_item_carries_at_most_one_open_handoff() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut store = database.store();
    store.propose_handoff(&proposal(&seeded)).unwrap();

    let competing = Handoff {
        from_agent_id: seeded.infra,
        ..proposal(&seeded)
    };
    let error = store.propose_handoff(&competing).unwrap_err();
    assert!(
        matches!(error, StoreError::HandoffAlreadyOpen(work_id) if work_id == seeded.work_id),
        "unexpected error: {error}"
    );
}

#[test]
fn an_identical_proposal_replay_is_a_no_op_and_a_changed_one_conflicts() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut store = database.store();
    let handoff = proposal(&seeded);
    store.propose_handoff(&handoff).unwrap();

    assert_eq!(store.propose_handoff(&handoff).unwrap(), handoff);

    let changed = Handoff {
        reason: Some("different claim".into()),
        ..handoff.clone()
    };
    let error = store.propose_handoff(&changed).unwrap_err();
    assert!(
        matches!(error, StoreError::HandoffIdConflict(id) if id == handoff.id),
        "unexpected error: {error}"
    );
}

#[test]
fn a_handoff_requires_both_agents_inside_the_thread() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut store = database.store();

    let error = store
        .propose_handoff(&Handoff {
            to_agent_id: seeded.outsider,
            ..proposal(&seeded)
        })
        .unwrap_err();

    assert!(
        matches!(error, StoreError::WorkOwnerScopeRequired { owner_agent_id, .. } if owner_agent_id == seeded.outsider),
        "unexpected error: {error}"
    );
}

#[test]
fn terminal_work_cannot_open_a_handoff() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut store = database.store();
    store
        .transition_work(seeded.work_id, WorkStatus::Working, CREATED)
        .unwrap();
    store
        .transition_work(seeded.work_id, WorkStatus::Cancelled, CREATED)
        .unwrap();

    let error = store.propose_handoff(&proposal(&seeded)).unwrap_err();
    assert!(
        matches!(error, StoreError::TerminalWorkOwnerImmutable(work_id) if work_id == seeded.work_id),
        "unexpected error: {error}"
    );
}

#[test]
fn handoffs_survive_restart_with_their_evidence() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let handoff_id = {
        let mut store = database.store();
        let handoff = store.propose_handoff(&proposal(&seeded)).unwrap();
        store
            .respond_to_handoff(
                handoff.id,
                &HandoffResponse::reject(
                    seeded.pay,
                    "contract is correct",
                    vec!["test:payment_contract".into()],
                )
                .with_proposed_owner(seeded.agent_order),
                ANSWERED,
            )
            .unwrap();
        handoff.id
    };

    let reopened = database.store().get_handoff(handoff_id).unwrap().unwrap();
    assert_eq!(reopened.status, HandoffStatus::Rejected);
    assert_eq!(reopened.evidence, vec!["test:payment_contract".to_owned()]);
    assert_eq!(reopened.proposed_owner_id, Some(seeded.agent_order));
    assert_eq!(
        database
            .store()
            .list_handoffs_for_work(seeded.work_id)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn handoff_decisions_round_trip_as_text() {
    for (decision, text) in [
        (HandoffDecision::Accept, "accept"),
        (HandoffDecision::Reject, "reject"),
        (HandoffDecision::Partial, "partial"),
    ] {
        assert_eq!(decision.to_string(), text);
        assert_eq!(text.parse::<HandoffDecision>().unwrap(), decision);
    }
}

#[test]
fn a_handoff_cannot_target_work_from_another_thread() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let other = seed(database.path());
    let mut store = database.store();

    let error = store
        .propose_handoff(&Handoff {
            work_id: other.work_id,
            ..proposal(&seeded)
        })
        .unwrap_err();

    assert!(
        matches!(error, StoreError::HandoffWorkOutOfThread { thread_id, .. } if thread_id == seeded.thread_id),
        "unexpected error: {error}"
    );
}

#[test]
fn an_unknown_work_item_has_no_handoffs() {
    let database = TestDatabase::new();
    seed(database.path());
    assert!(
        database
            .store()
            .list_handoffs_for_work(WorkItemId::new())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_blank_timestamp_is_refused() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut store = database.store();
    let handoff = store.propose_handoff(&proposal(&seeded)).unwrap();

    let error = store
        .respond_to_handoff(handoff.id, &HandoffResponse::accept(seeded.pay), "  ")
        .unwrap_err();
    assert!(
        matches!(error, StoreError::InvalidHandoffTimestamp),
        "unexpected error: {error}"
    );
}

#[test]
fn responding_to_an_unknown_handoff_fails() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut store = database.store();

    let error = store
        .respond_to_handoff(
            Default::default(),
            &HandoffResponse::accept(seeded.pay),
            ANSWERED,
        )
        .unwrap_err();
    assert!(
        matches!(error, StoreError::HandoffNotFound(_)),
        "unexpected error: {error}"
    );
}

#[test]
fn work_items_keep_their_owner_when_a_handoff_is_cancelled() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut store = database.store();
    store
        .assign_work_owner(seeded.work_id, seeded.agent_order, CREATED)
        .unwrap();
    let handoff = store.propose_handoff(&proposal(&seeded)).unwrap();
    store
        .cancel_handoff(handoff.id, seeded.agent_order, CLOSED)
        .unwrap();

    let work: WorkItem = store.get_work_item(seeded.work_id).unwrap().unwrap();
    assert_eq!(work.owner_agent_id, Some(seeded.agent_order));
}
