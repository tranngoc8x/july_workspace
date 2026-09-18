//! Phase 6.5.3 — candidate solutions are durable, disagreement must be
//! actionable, and revisions supersede instead of overwriting.
use july_workspace::domain::{
    Agent, AgentId, Conversation, ConversationId, ConversationKind, Decision, DecisionId,
    DecisionOutcome, DecisionOwner, DecisionStatus, DecisionType, Proposal, ProposalId,
    ProposalResponse, ProposalResponseId, ProposalResponseType, ProposalStatus, Room, RoomId,
    WorkItemId,
};
use july_workspace::storage::{SqliteStore, StoreError};
use serde_json::json;
use std::path::{Path, PathBuf};

const CREATED: &str = "2026-08-25T08:00:00Z";
const ANSWERED: &str = "2026-08-25T09:00:00Z";
const REVISED: &str = "2026-08-25T10:00:00Z";

struct TestDatabase {
    directory: PathBuf,
    path: PathBuf,
}

impl TestDatabase {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("july-proposal-{}", ulid::Ulid::generate()));
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
    agent_order: AgentId,
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
        title: Some("Retry strategy".into()),
        goal: Some("Pick a retry approach".into()),
        parent_conversation_id: None,
        origin_conversation_id: None,
        status: "open".into(),
        created_at: CREATED.into(),
        updated_at: CREATED.into(),
    };
    let agent_order = agent("agent_order");
    let pay = agent("pay");
    let outsider = agent("outsider");
    store.insert_room(&room).unwrap();
    for member in [&agent_order, &pay, &outsider] {
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
            &[agent_order.id, pay.id],
        )
        .unwrap();
    Seeded {
        thread_id: thread.id,
        agent_order: agent_order.id,
        pay: pay.id,
        outsider: outsider.id,
    }
}

fn proposal(seeded: &Seeded, title: &str) -> Proposal {
    Proposal {
        id: ProposalId::new(),
        thread_id: seeded.thread_id,
        author_agent_id: seeded.pay,
        title: title.into(),
        problem_statement: Some("Callbacks time out under load".into()),
        approach: Some("Retry inside Pay with jittered backoff".into()),
        benefits: vec!["no new infrastructure".into()],
        costs: vec!["Pay owns the retry budget".into()],
        risks: vec!["duplicate callbacks".into()],
        assumptions: vec!["callbacks are idempotent".into()],
        evidence: vec!["log:callback_timeout".into()],
        status: ProposalStatus::Open,
        supersedes_proposal_id: None,
        created_at: CREATED.into(),
        updated_at: CREATED.into(),
    }
}

fn response(
    proposal_id: ProposalId,
    agent_id: AgentId,
    response_type: ProposalResponseType,
) -> ProposalResponse {
    ProposalResponse {
        id: ProposalResponseId::new(),
        proposal_id,
        agent_id,
        response_type,
        reason: Some("Retry hides the missing contract field".into()),
        evidence: vec!["src/payment/callback.rs".into()],
        created_at: ANSWERED.into(),
    }
}

#[test]
fn a_proposal_keeps_its_structure_across_restart() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let created = database
        .store()
        .create_proposal(&proposal(&seeded, "Retry in Pay"))
        .unwrap();

    let stored = database.store().get_proposal(created.id).unwrap().unwrap();
    assert_eq!(stored, created);
    assert_eq!(stored.status, ProposalStatus::Open);
    assert_eq!(stored.risks, vec!["duplicate callbacks".to_owned()]);
    assert_eq!(stored.evidence, vec!["log:callback_timeout".to_owned()]);
}

#[test]
fn supporting_a_proposal_needs_no_evidence_but_challenging_does() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut store = database.store();
    let created = store
        .create_proposal(&proposal(&seeded, "Retry in Pay"))
        .unwrap();

    store
        .respond_to_proposal(&ProposalResponse {
            reason: None,
            evidence: Vec::new(),
            ..response(created.id, seeded.pay, ProposalResponseType::Support)
        })
        .unwrap();

    let error = store
        .respond_to_proposal(&ProposalResponse {
            evidence: Vec::new(),
            ..response(
                created.id,
                seeded.agent_order,
                ProposalResponseType::Challenge,
            )
        })
        .unwrap_err();
    assert!(
        matches!(error, StoreError::Domain(_)),
        "unexpected: {error}"
    );

    let challenge = store
        .respond_to_proposal(&response(
            created.id,
            seeded.agent_order,
            ProposalResponseType::Challenge,
        ))
        .unwrap();
    assert_eq!(challenge.evidence.len(), 1);
    assert_eq!(store.list_proposal_responses(created.id).unwrap().len(), 2);
    assert_eq!(
        store.get_proposal(created.id).unwrap().unwrap().status,
        ProposalStatus::Open,
        "a challenge does not by itself change the proposal"
    );
}

#[test]
fn an_amendment_request_marks_the_proposal_amended() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut store = database.store();
    let created = store
        .create_proposal(&proposal(&seeded, "Retry in Pay"))
        .unwrap();

    store
        .respond_to_proposal(&ProposalResponse {
            evidence: Vec::new(),
            ..response(created.id, seeded.agent_order, ProposalResponseType::Amend)
        })
        .unwrap();

    assert_eq!(
        store.get_proposal(created.id).unwrap().unwrap().status,
        ProposalStatus::Amended
    );
}

#[test]
fn a_revision_supersedes_the_proposal_it_replaces() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut store = database.store();
    let first = store
        .create_proposal(&proposal(&seeded, "Retry in Pay"))
        .unwrap();

    let revision = Proposal {
        id: ProposalId::new(),
        title: "Retry in Pay with a dead letter topic".into(),
        supersedes_proposal_id: Some(first.id),
        created_at: REVISED.into(),
        updated_at: REVISED.into(),
        ..proposal(&seeded, "unused")
    };
    store.create_proposal(&revision).unwrap();

    assert_eq!(
        store.get_proposal(first.id).unwrap().unwrap().status,
        ProposalStatus::Superseded
    );
    let error = store
        .respond_to_proposal(&response(
            first.id,
            seeded.agent_order,
            ProposalResponseType::Support,
        ))
        .unwrap_err();
    assert!(
        matches!(
            error,
            StoreError::ProposalNotLive {
                status: ProposalStatus::Superseded,
                ..
            }
        ),
        "unexpected error: {error}"
    );
    assert_eq!(
        store
            .list_proposals_for_thread(seeded.thread_id)
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn only_the_author_withdraws_a_proposal() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut store = database.store();
    let created = store
        .create_proposal(&proposal(&seeded, "Retry in Pay"))
        .unwrap();

    let error = store
        .withdraw_proposal(created.id, seeded.agent_order, REVISED)
        .unwrap_err();
    assert!(
        matches!(error, StoreError::ProposalAuthorMismatch { expected, .. } if expected == seeded.pay),
        "unexpected error: {error}"
    );

    let withdrawn = store
        .withdraw_proposal(created.id, seeded.pay, REVISED)
        .unwrap();
    assert_eq!(withdrawn.status, ProposalStatus::Withdrawn);
    assert!(!withdrawn.status.is_live());
}

#[test]
fn only_thread_members_may_author_or_answer() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut store = database.store();

    let error = store
        .create_proposal(&Proposal {
            author_agent_id: seeded.outsider,
            ..proposal(&seeded, "Outsider plan")
        })
        .unwrap_err();
    assert!(
        matches!(error, StoreError::ThreadMembershipRequired { agent_id, .. } if agent_id == seeded.outsider),
        "unexpected error: {error}"
    );

    let created = store
        .create_proposal(&proposal(&seeded, "Retry in Pay"))
        .unwrap();
    let error = store
        .respond_to_proposal(&response(
            created.id,
            seeded.outsider,
            ProposalResponseType::Challenge,
        ))
        .unwrap_err();
    assert!(
        matches!(error, StoreError::ThreadMembershipRequired { agent_id, .. } if agent_id == seeded.outsider),
        "unexpected error: {error}"
    );
}

#[test]
fn replays_are_no_ops_and_changed_content_conflicts() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut store = database.store();
    let created = proposal(&seeded, "Retry in Pay");
    store.create_proposal(&created).unwrap();
    assert_eq!(store.create_proposal(&created).unwrap(), created);

    let error = store
        .create_proposal(&Proposal {
            title: "Retry somewhere else".into(),
            ..created.clone()
        })
        .unwrap_err();
    assert!(
        matches!(error, StoreError::ProposalIdConflict(id) if id == created.id),
        "unexpected error: {error}"
    );

    let answer = response(
        created.id,
        seeded.agent_order,
        ProposalResponseType::Challenge,
    );
    store.respond_to_proposal(&answer).unwrap();
    assert_eq!(store.respond_to_proposal(&answer).unwrap(), answer);
    let error = store
        .respond_to_proposal(&ProposalResponse {
            reason: Some("different objection".into()),
            ..answer.clone()
        })
        .unwrap_err();
    assert!(
        matches!(error, StoreError::ProposalResponseIdConflict(id) if id == answer.id),
        "unexpected error: {error}"
    );
    assert_eq!(store.list_proposal_responses(created.id).unwrap().len(), 1);
}

#[test]
fn a_decision_that_selects_a_proposal_accepts_it() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut store = database.store();
    let chosen = store
        .create_proposal(&proposal(&seeded, "Durable queue"))
        .unwrap();
    let decision = store
        .record_decision(&Decision {
            id: DecisionId::new(),
            thread_id: seeded.thread_id,
            decision_type: DecisionType::Technical,
            title: "Retry strategy".into(),
            decision: None,
            reason: None,
            selected_proposal_id: None,
            alternatives: vec!["retry in Pay".into()],
            evidence: Vec::new(),
            participants: vec![seeded.agent_order, seeded.pay],
            decision_owner: DecisionOwner::User,
            status: DecisionStatus::Pending,
            supersedes_decision_id: None,
            created_at: CREATED.into(),
            updated_at: CREATED.into(),
        })
        .unwrap();

    let decided = store
        .decide(
            decision.id,
            &DecisionOutcome {
                selected_proposal_id: Some(chosen.id),
                ..DecisionOutcome::new(DecisionOwner::User, "Adopt the durable queue")
            },
            REVISED,
        )
        .unwrap();

    assert_eq!(decided.selected_proposal_id, Some(chosen.id));
    assert_eq!(
        store.get_proposal(chosen.id).unwrap().unwrap().status,
        ProposalStatus::Accepted
    );
}

#[test]
fn a_decision_cannot_select_a_proposal_from_another_thread() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let other = seed(database.path());
    let mut store = database.store();
    let foreign = store
        .create_proposal(&proposal(&other, "Foreign plan"))
        .unwrap();
    let decision = store
        .record_decision(&Decision {
            id: DecisionId::new(),
            thread_id: seeded.thread_id,
            decision_type: DecisionType::Technical,
            title: "Retry strategy".into(),
            decision: None,
            reason: None,
            selected_proposal_id: None,
            alternatives: Vec::new(),
            evidence: Vec::new(),
            participants: vec![seeded.pay],
            decision_owner: DecisionOwner::User,
            status: DecisionStatus::Pending,
            supersedes_decision_id: None,
            created_at: CREATED.into(),
            updated_at: CREATED.into(),
        })
        .unwrap();

    let error = store
        .decide(
            decision.id,
            &DecisionOutcome {
                selected_proposal_id: Some(foreign.id),
                ..DecisionOutcome::new(DecisionOwner::User, "Adopt the foreign plan")
            },
            REVISED,
        )
        .unwrap_err();

    assert!(
        matches!(error, StoreError::ProposalOutOfThread { proposal_id, .. } if proposal_id == foreign.id),
        "unexpected error: {error}"
    );
}

#[test]
fn a_blank_proposal_timestamp_is_refused() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let error = database
        .store()
        .create_proposal(&Proposal {
            created_at: "   ".into(),
            ..proposal(&seeded, "Retry in Pay")
        })
        .unwrap_err();
    assert!(
        matches!(error, StoreError::InvalidProposalTimestamp),
        "unexpected error: {error}"
    );
}
