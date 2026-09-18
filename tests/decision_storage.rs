//! Phase 6.5.2 — disagreement converges: challenge rounds are bounded, an
//! exhausted dispute becomes a durable decision, and deciding it is what moves
//! ownership.
use july_workspace::domain::{
    Agent, AgentId, Conversation, ConversationId, ConversationKind, Decision, DecisionId,
    DecisionOutcome, DecisionOwner, DecisionStatus, DecisionType, Handoff, HandoffChallenge,
    HandoffResponse, HandoffStatus, Room, RoomId, WorkItemId,
};
use july_workspace::storage::{SqliteStore, StoreError};
use serde_json::json;
use std::path::{Path, PathBuf};

const CREATED: &str = "2026-08-25T08:00:00Z";
const ROUND_ONE: &str = "2026-08-25T09:00:00Z";
const ROUND_TWO: &str = "2026-08-25T10:00:00Z";
const DECIDED: &str = "2026-08-25T11:00:00Z";

struct TestDatabase {
    directory: PathBuf,
    path: PathBuf,
}

impl TestDatabase {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("july-decision-{}", ulid::Ulid::generate()));
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
    architect: AgentId,
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
    let architect = agent("architect");
    store.insert_room(&room).unwrap();
    for member in [&agent_order, &pay, &architect] {
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
            &[agent_order.id, pay.id, architect.id],
        )
        .unwrap();
    Seeded {
        thread_id: thread.id,
        work_id,
        agent_order: agent_order.id,
        pay: pay.id,
        architect: architect.id,
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

fn rejection(seeded: &Seeded, evidence: &str) -> HandoffResponse {
    HandoffResponse::reject(
        seeded.pay,
        "Pay returns transaction_ref per the current contract",
        vec![evidence.into()],
    )
    .with_proposed_owner(seeded.agent_order)
}

#[test]
fn a_challenge_inside_the_budget_reopens_the_proposal() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut store = database.store();
    let handoff = store.propose_handoff(&proposal(&seeded)).unwrap();
    store
        .respond_to_handoff(handoff.id, &rejection(&seeded, "test:contract"), ROUND_ONE)
        .unwrap();

    let (challenged, decision) = store
        .challenge_handoff(
            handoff.id,
            &HandoffChallenge::new(seeded.agent_order, vec!["log:callback_timeout".into()]),
            ROUND_TWO,
        )
        .unwrap();

    assert_eq!(challenged.status, HandoffStatus::Proposed);
    assert_eq!(challenged.round_count, 1);
    assert!(decision.is_none(), "the budget was not spent yet");
    assert!(
        challenged
            .evidence
            .contains(&"log:callback_timeout".to_owned()),
        "challenge evidence stays attached: {:?}",
        challenged.evidence
    );

    // The target owes one more structured answer, and may now accept.
    let accepted = store
        .respond_to_handoff(handoff.id, &HandoffResponse::accept(seeded.pay), DECIDED)
        .unwrap();
    assert_eq!(accepted.status, HandoffStatus::Accepted);
    assert!(
        store
            .list_decisions_for_thread(seeded.thread_id)
            .unwrap()
            .is_empty(),
        "a resolved dispute needs no decision"
    );
}

#[test]
fn an_exhausted_dispute_escalates_to_needs_decision_and_stops() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut store = database.store();
    let handoff = store.propose_handoff(&proposal(&seeded)).unwrap();
    store
        .respond_to_handoff(handoff.id, &rejection(&seeded, "test:contract"), ROUND_ONE)
        .unwrap();
    store
        .challenge_handoff(
            handoff.id,
            &HandoffChallenge::new(seeded.agent_order, vec!["log:callback_timeout".into()]),
            ROUND_ONE,
        )
        .unwrap();
    store
        .respond_to_handoff(
            handoff.id,
            &rejection(&seeded, "test:contract_v2"),
            ROUND_TWO,
        )
        .unwrap();

    let (disputed, decision) = store
        .challenge_handoff(
            handoff.id,
            &HandoffChallenge::new(seeded.agent_order, vec!["commit:9f21a".into()]),
            ROUND_TWO,
        )
        .unwrap();

    assert_eq!(disputed.status, HandoffStatus::Disputed);
    assert_eq!(disputed.round_count, Handoff::MAX_DISPUTE_ROUNDS);
    let decision = decision.expect("an exhausted dispute opens a decision");
    assert_eq!(disputed.decision_id, Some(decision.id));
    assert_eq!(decision.status, DecisionStatus::NeedsDecision);
    assert_eq!(decision.decision_type, DecisionType::Ownership);
    assert_eq!(decision.decision_owner, DecisionOwner::User);
    assert_eq!(decision.participants, vec![seeded.agent_order, seeded.pay]);
    assert!(decision.evidence.contains(&"commit:9f21a".to_owned()));

    // No further automatic turns: neither side may keep the loop running.
    let error = store
        .challenge_handoff(
            handoff.id,
            &HandoffChallenge::new(seeded.agent_order, vec!["commit:aa11b".into()]),
            DECIDED,
        )
        .unwrap_err();
    assert!(
        matches!(error, StoreError::InvalidHandoffTransition { .. }),
        "unexpected error: {error}"
    );
    let error = store
        .respond_to_handoff(handoff.id, &HandoffResponse::accept(seeded.pay), DECIDED)
        .unwrap_err();
    assert!(
        matches!(error, StoreError::InvalidHandoffTransition { .. }),
        "unexpected error: {error}"
    );
}

#[test]
fn a_challenge_without_new_evidence_is_refused() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut store = database.store();
    let handoff = store.propose_handoff(&proposal(&seeded)).unwrap();
    store
        .respond_to_handoff(handoff.id, &rejection(&seeded, "test:contract"), ROUND_ONE)
        .unwrap();

    let error = store
        .challenge_handoff(
            handoff.id,
            &HandoffChallenge::new(seeded.agent_order, Vec::new()),
            ROUND_TWO,
        )
        .unwrap_err();

    assert!(
        matches!(error, StoreError::HandoffChallengeMissingEvidence(id) if id == handoff.id),
        "unexpected error: {error}"
    );
    assert_eq!(
        store.get_handoff(handoff.id).unwrap().unwrap().status,
        HandoffStatus::Rejected
    );
}

#[test]
fn only_the_source_agent_may_challenge() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut store = database.store();
    let handoff = store.propose_handoff(&proposal(&seeded)).unwrap();
    store
        .respond_to_handoff(handoff.id, &rejection(&seeded, "test:contract"), ROUND_ONE)
        .unwrap();

    let error = store
        .challenge_handoff(
            handoff.id,
            &HandoffChallenge::new(seeded.pay, vec!["log:x".into()]),
            ROUND_TWO,
        )
        .unwrap_err();

    assert!(
        matches!(error, StoreError::HandoffSourceMismatch { expected, .. } if expected == seeded.agent_order),
        "unexpected error: {error}"
    );
}

fn escalate(store: &mut SqliteStore, seeded: &Seeded, owner: DecisionOwner) -> (Handoff, Decision) {
    let handoff = store.propose_handoff(&proposal(seeded)).unwrap();
    store
        .respond_to_handoff(handoff.id, &rejection(seeded, "test:contract"), ROUND_ONE)
        .unwrap();
    store
        .challenge_handoff(
            handoff.id,
            &HandoffChallenge::new(seeded.agent_order, vec!["log:timeout".into()]).decided_by(owner),
            ROUND_ONE,
        )
        .unwrap();
    store
        .respond_to_handoff(
            handoff.id,
            &rejection(seeded, "test:contract_v2"),
            ROUND_TWO,
        )
        .unwrap();
    let (handoff, decision) = store
        .challenge_handoff(
            handoff.id,
            &HandoffChallenge::new(seeded.agent_order, vec!["commit:9f21a".into()]).decided_by(owner),
            ROUND_TWO,
        )
        .unwrap();
    (
        handoff,
        decision.expect("escalated dispute opens a decision"),
    )
}

#[test]
fn the_user_decides_a_dispute_and_ownership_follows_the_decision() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut store = database.store();
    let (handoff, decision) = escalate(&mut store, &seeded, DecisionOwner::User);

    let decided = store
        .decide(
            decision.id,
            &DecisionOutcome::new(DecisionOwner::User, "Cashpoint keeps the mapping work")
                .because(
                    "The contract is stable",
                    vec!["test:payment_contract".into()],
                )
                .assigning_owner(seeded.agent_order),
            DECIDED,
        )
        .unwrap();

    assert_eq!(decided.status, DecisionStatus::Decided);
    assert_eq!(
        decided.decision.as_deref(),
        Some("Cashpoint keeps the mapping work")
    );
    let handoff = store.get_handoff(handoff.id).unwrap().unwrap();
    assert_eq!(handoff.status, HandoffStatus::Resolved);
    assert_eq!(
        store
            .get_work_item(seeded.work_id)
            .unwrap()
            .unwrap()
            .owner_agent_id,
        Some(seeded.agent_order)
    );
}

#[test]
fn the_user_can_cancel_a_disputed_decision_without_leaving_the_handoff_open() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut store = database.store();
    let (handoff, decision) = escalate(&mut store, &seeded, DecisionOwner::User);

    let cancelled = store
        .cancel_decision(
            decision.id,
            DecisionOwner::User,
            "keep current ownership",
            DECIDED,
        )
        .unwrap();

    assert_eq!(cancelled.status, DecisionStatus::Cancelled);
    assert_eq!(cancelled.reason.as_deref(), Some("keep current ownership"));
    assert_eq!(
        store.get_handoff(handoff.id).unwrap().unwrap().status,
        HandoffStatus::Resolved
    );
    assert_eq!(
        store
            .get_work_item(seeded.work_id)
            .unwrap()
            .unwrap()
            .owner_agent_id,
        None
    );
}

#[test]
fn a_named_agent_can_own_the_decision_and_others_cannot_settle_it() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut store = database.store();
    let (_, decision) = escalate(&mut store, &seeded, DecisionOwner::Agent(seeded.architect));
    assert_eq!(
        decision.decision_owner,
        DecisionOwner::Agent(seeded.architect)
    );

    let error = store
        .decide(
            decision.id,
            &DecisionOutcome::new(DecisionOwner::User, "I decide instead"),
            DECIDED,
        )
        .unwrap_err();
    assert!(
        matches!(error, StoreError::DecisionOwnerMismatch { .. }),
        "unexpected error: {error}"
    );

    let decided = store
        .decide(
            decision.id,
            &DecisionOutcome::new(
                DecisionOwner::Agent(seeded.architect),
                "Pay owns retry, Cashpoint owns mapping",
            )
            .assigning_owner(seeded.pay),
            DECIDED,
        )
        .unwrap();
    assert_eq!(decided.status, DecisionStatus::Decided);
}

#[test]
fn a_decided_dispute_cannot_be_decided_twice() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut store = database.store();
    let (_, decision) = escalate(&mut store, &seeded, DecisionOwner::User);
    store
        .decide(
            decision.id,
            &DecisionOutcome::new(DecisionOwner::User, "Cashpoint owns it"),
            DECIDED,
        )
        .unwrap();

    let error = store
        .decide(
            decision.id,
            &DecisionOutcome::new(DecisionOwner::User, "Pay owns it after all"),
            DECIDED,
        )
        .unwrap_err();
    assert!(
        matches!(
            error,
            StoreError::InvalidDecisionTransition {
                from: DecisionStatus::Decided,
                ..
            }
        ),
        "unexpected error: {error}"
    );
}

fn technical_decision(seeded: &Seeded) -> Decision {
    Decision {
        id: DecisionId::new(),
        thread_id: seeded.thread_id,
        decision_type: DecisionType::Technical,
        title: "Retry strategy".into(),
        decision: None,
        reason: None,
        selected_proposal_id: None,
        alternatives: vec!["retry in Pay".into(), "durable queue".into()],
        evidence: vec!["doc:sla".into()],
        participants: vec![seeded.agent_order, seeded.pay],
        decision_owner: DecisionOwner::User,
        status: DecisionStatus::Pending,
        supersedes_decision_id: None,
        created_at: CREATED.into(),
        updated_at: CREATED.into(),
    }
}

#[test]
fn a_technical_decision_can_be_recorded_decided_and_superseded() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut store = database.store();
    let recorded = store.record_decision(&technical_decision(&seeded)).unwrap();
    assert_eq!(recorded.status, DecisionStatus::Pending);

    store
        .decide(
            recorded.id,
            &DecisionOutcome::new(DecisionOwner::User, "Use a durable queue"),
            ROUND_TWO,
        )
        .unwrap();

    let replacement = Decision {
        id: DecisionId::new(),
        decision: Some("Use a durable queue with a dead letter topic".into()),
        status: DecisionStatus::Decided,
        supersedes_decision_id: Some(recorded.id),
        created_at: DECIDED.into(),
        updated_at: DECIDED.into(),
        ..technical_decision(&seeded)
    };
    store.record_decision(&replacement).unwrap();

    let superseded = store.get_decision(recorded.id).unwrap().unwrap();
    assert_eq!(superseded.status, DecisionStatus::Superseded);
    assert_eq!(
        superseded.decision.as_deref(),
        Some("Use a durable queue"),
        "a superseded decision keeps what it once stated"
    );
    assert_eq!(
        store
            .list_decisions_for_thread(seeded.thread_id)
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn only_a_decided_decision_can_be_superseded() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut store = database.store();
    let recorded = store.record_decision(&technical_decision(&seeded)).unwrap();

    let error = store
        .record_decision(&Decision {
            id: DecisionId::new(),
            decision: Some("Replace an undecided decision".into()),
            status: DecisionStatus::Decided,
            supersedes_decision_id: Some(recorded.id),
            created_at: DECIDED.into(),
            updated_at: DECIDED.into(),
            ..technical_decision(&seeded)
        })
        .unwrap_err();

    assert!(
        matches!(
            error,
            StoreError::InvalidDecisionTransition {
                to: DecisionStatus::Superseded,
                ..
            }
        ),
        "unexpected error: {error}"
    );
}

#[test]
fn assigning_an_owner_needs_a_handoff_to_settle() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut store = database.store();
    let recorded = store.record_decision(&technical_decision(&seeded)).unwrap();

    let error = store
        .decide(
            recorded.id,
            &DecisionOutcome::new(DecisionOwner::User, "Pay owns it").assigning_owner(seeded.pay),
            DECIDED,
        )
        .unwrap_err();

    assert!(
        matches!(error, StoreError::DecisionHasNoHandoff(id) if id == recorded.id),
        "unexpected error: {error}"
    );
}

#[test]
fn a_decision_states_its_outcome_only_once_settled() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut store = database.store();

    let error = store
        .record_decision(&Decision {
            decision: Some("premature".into()),
            ..technical_decision(&seeded)
        })
        .unwrap_err();

    assert!(
        matches!(error, StoreError::Domain(_)),
        "unexpected: {error}"
    );
}

#[test]
fn decisions_and_their_disputes_survive_restart() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let (handoff_id, decision_id) = {
        let mut store = database.store();
        let (handoff, decision) = escalate(&mut store, &seeded, DecisionOwner::User);
        (handoff.id, decision.id)
    };

    let mut store = database.store();
    let decision = store.get_decision(decision_id).unwrap().unwrap();
    assert_eq!(decision.status, DecisionStatus::NeedsDecision);
    assert_eq!(
        store.get_handoff(handoff_id).unwrap().unwrap().status,
        HandoffStatus::Disputed
    );

    store
        .decide(
            decision_id,
            &DecisionOutcome::new(DecisionOwner::User, "Pay owns retry")
                .assigning_owner(seeded.pay),
            DECIDED,
        )
        .unwrap();

    let reopened = database.store();
    assert_eq!(
        reopened.get_decision(decision_id).unwrap().unwrap().status,
        DecisionStatus::Decided
    );
    assert_eq!(
        reopened.get_handoff(handoff_id).unwrap().unwrap().status,
        HandoffStatus::Resolved
    );
}

#[test]
fn a_decision_owner_round_trips_as_text() {
    let agent_id = AgentId::new();
    for owner in [DecisionOwner::User, DecisionOwner::Agent(agent_id)] {
        assert_eq!(
            owner.to_string().parse::<DecisionOwner>().unwrap(),
            owner,
            "round trip failed for {owner}"
        );
    }
    assert!("not-an-owner".parse::<DecisionOwner>().is_err());
}
