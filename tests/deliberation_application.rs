//! Phase 6.5.5 — the deliberation protocol is reachable through the
//! application boundary with typed, caller-actionable errors.
use july_workspace::application::{DeliberationError, DeliberationService};
use july_workspace::domain::{
    Agent, AgentId, Conversation, ConversationId, ConversationKind, Decision, DecisionId,
    DecisionOutcome, DecisionOwner, DecisionStatus, DecisionType, DecisionWork, Handoff,
    HandoffChallenge, HandoffId, HandoffResponse, HandoffStatus, Proposal, ProposalId,
    ProposalResponse, ProposalResponseId, ProposalResponseType, ProposalStatus, Room, RoomId,
    WorkItemId, WorkStatus,
};
use july_workspace::runtime::StorageWorker;
use july_workspace::storage::SqliteStore;
use serde_json::json;
use std::path::{Path, PathBuf};

const CREATED: &str = "2026-08-25T08:00:00Z";
const ANSWERED: &str = "2026-08-25T09:00:00Z";
const DECIDED: &str = "2026-08-25T10:00:00Z";

struct TestDatabase {
    directory: PathBuf,
    path: PathBuf,
}

impl TestDatabase {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("july-deliberation-app-{}", ulid::Ulid::generate()));
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

struct Seeded {
    thread_id: ConversationId,
    work_id: WorkItemId,
    agent_order: AgentId,
    pay: AgentId,
    outsider: AgentId,
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
        goal: Some("Settle callback retry ownership".into()),
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
    let work_id = WorkItemId::new();
    store
        .create_thread_with_primary_work(&thread, work_id, "tony", &[agent_order.id, pay.id])
        .unwrap();
    Seeded {
        thread_id: thread.id,
        work_id,
        agent_order: agent_order.id,
        pay: pay.id,
        outsider: outsider.id,
    }
}

fn service(path: &Path) -> DeliberationService<StorageWorker> {
    DeliberationService::new(StorageWorker::open(path).unwrap())
}

fn proposal_handoff(seeded: &Seeded) -> Handoff {
    Handoff {
        id: HandoffId::new(),
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

#[tokio::test]
async fn a_handoff_runs_through_the_service_and_moves_ownership() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut service = service(database.path());

    let handoff = service
        .propose_handoff(proposal_handoff(&seeded))
        .await
        .unwrap();
    let accepted = service
        .respond_to_handoff(
            handoff.id,
            HandoffResponse::accept(seeded.pay),
            ANSWERED.into(),
        )
        .await
        .unwrap();

    assert_eq!(accepted.status, HandoffStatus::Accepted);
    assert_eq!(
        SqliteStore::open(database.path())
            .unwrap()
            .get_work_item(seeded.work_id)
            .unwrap()
            .unwrap()
            .owner_agent_id,
        Some(seeded.pay)
    );
}

#[tokio::test]
async fn a_rejected_handoff_can_be_resolved_by_its_source() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut service = service(database.path());
    let handoff = service
        .propose_handoff(proposal_handoff(&seeded))
        .await
        .unwrap();
    service
        .respond_to_handoff(
            handoff.id,
            HandoffResponse::reject(
                seeded.pay,
                "contract is correct",
                vec!["test:contract".into()],
            ),
            ANSWERED.into(),
        )
        .await
        .unwrap();

    let resolved = service
        .resolve_handoff(handoff.id, seeded.agent_order, DECIDED.into())
        .await
        .unwrap();

    assert_eq!(resolved.status, HandoffStatus::Resolved);
}

#[tokio::test]
async fn an_exhausted_dispute_becomes_a_decision_and_then_work() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut service = service(database.path());
    let handoff = service
        .propose_handoff(proposal_handoff(&seeded))
        .await
        .unwrap();

    for evidence in ["test:contract", "test:contract_v2"] {
        service
            .respond_to_handoff(
                handoff.id,
                HandoffResponse::reject(seeded.pay, "contract is correct", vec![evidence.into()]),
                ANSWERED.into(),
            )
            .await
            .unwrap();
        let (_, decision) = service
            .challenge_handoff(
                handoff.id,
                HandoffChallenge::new(seeded.agent_order, vec![format!("log:{evidence}")]),
                ANSWERED.into(),
            )
            .await
            .unwrap();
        if let Some(decision) = decision {
            let decided = service
                .decide(
                    decision.id,
                    DecisionOutcome::new(DecisionOwner::User, "Pay owns retry")
                        .assigning_owner(seeded.pay),
                    DECIDED.into(),
                )
                .await
                .unwrap();
            assert_eq!(decided.status, DecisionStatus::Decided);

            let work = service
                .convert_decision_to_work(
                    decided.id,
                    vec![DecisionWork::new("Implement callback retry").owned_by(seeded.pay)],
                    DECIDED.into(),
                )
                .await
                .unwrap();
            assert_eq!(work.len(), 1);
            assert_eq!(work[0].status, WorkStatus::Open);
            assert_eq!(work[0].owner_agent_id, Some(seeded.pay));
            return;
        }
    }
    panic!("the dispute never escalated within its round budget");
}

#[tokio::test]
async fn proposals_flow_through_the_service() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut service = service(database.path());

    let proposal = service
        .create_proposal(Proposal {
            id: ProposalId::new(),
            thread_id: seeded.thread_id,
            author_agent_id: seeded.pay,
            title: "Durable queue".into(),
            problem_statement: Some("Callbacks time out".into()),
            approach: Some("Move retries into a queue".into()),
            benefits: vec!["one retry policy".into()],
            costs: vec!["new infrastructure".into()],
            risks: Vec::new(),
            assumptions: Vec::new(),
            evidence: vec!["doc:sla".into()],
            status: ProposalStatus::Open,
            supersedes_proposal_id: None,
            created_at: CREATED.into(),
            updated_at: CREATED.into(),
        })
        .await
        .unwrap();

    service
        .respond_to_proposal(ProposalResponse {
            id: ProposalResponseId::new(),
            proposal_id: proposal.id,
            agent_id: seeded.agent_order,
            response_type: ProposalResponseType::Support,
            reason: None,
            evidence: Vec::new(),
            created_at: ANSWERED.into(),
        })
        .await
        .unwrap();

    let withdrawn = service
        .withdraw_proposal(proposal.id, seeded.pay, DECIDED.into())
        .await
        .unwrap();
    assert_eq!(withdrawn.status, ProposalStatus::Withdrawn);
}

#[tokio::test]
async fn service_errors_are_grouped_by_what_the_caller_can_do() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut service = service(database.path());

    assert!(matches!(
        service
            .respond_to_handoff(
                HandoffId::new(),
                HandoffResponse::accept(seeded.pay),
                ANSWERED.into()
            )
            .await,
        Err(DeliberationError::NotFound(_))
    ));

    let handoff = service
        .propose_handoff(proposal_handoff(&seeded))
        .await
        .unwrap();
    assert!(matches!(
        service
            .respond_to_handoff(
                handoff.id,
                HandoffResponse::accept(seeded.agent_order),
                ANSWERED.into()
            )
            .await,
        Err(DeliberationError::NotPermitted(_))
    ));
    assert!(matches!(
        service
            .respond_to_handoff(
                handoff.id,
                HandoffResponse::reject(seeded.pay, "no", Vec::new()),
                ANSWERED.into()
            )
            .await,
        Err(DeliberationError::Invalid(_))
    ));
    service
        .respond_to_handoff(
            handoff.id,
            HandoffResponse::accept(seeded.pay),
            ANSWERED.into(),
        )
        .await
        .unwrap();
    assert!(matches!(
        service
            .respond_to_handoff(
                handoff.id,
                HandoffResponse::accept(seeded.pay),
                ANSWERED.into()
            )
            .await,
        Err(DeliberationError::InvalidTransition(_))
    ));
    assert!(matches!(
        service
            .create_proposal(Proposal {
                id: ProposalId::new(),
                thread_id: seeded.thread_id,
                author_agent_id: seeded.outsider,
                title: "Outsider plan".into(),
                problem_statement: None,
                approach: None,
                benefits: Vec::new(),
                costs: Vec::new(),
                risks: Vec::new(),
                assumptions: Vec::new(),
                evidence: Vec::new(),
                status: ProposalStatus::Open,
                supersedes_proposal_id: None,
                created_at: CREATED.into(),
                updated_at: CREATED.into(),
            })
            .await,
        Err(DeliberationError::NotPermitted(_))
    ));
}

#[tokio::test]
async fn an_undecided_decision_generates_no_work_through_the_service() {
    let database = TestDatabase::new();
    let seeded = seed(database.path());
    let mut service = service(database.path());
    let decision = service
        .record_decision(Decision {
            id: DecisionId::new(),
            thread_id: seeded.thread_id,
            decision_type: DecisionType::Scope,
            title: "Scope of the retry fix".into(),
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
        .await
        .unwrap();

    assert!(matches!(
        service
            .convert_decision_to_work(
                decision.id,
                vec![DecisionWork::new("Premature work")],
                DECIDED.into()
            )
            .await,
        Err(DeliberationError::InvalidTransition(_))
    ));
}
