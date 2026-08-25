//! Phase 6.5.6 — the required end-to-end scenario from
//! `docs/16-AGENT-DELIBERATION-UPGRADE-PLAN.md` section 16:
//!
//! cashpoint claims Pay owns the issue -> pay REJECTS with evidence ->
//! cashpoint CHALLENGES -> the dispute stops instead of looping -> proposals
//! A/B -> the decision owner selects B -> the decision persists -> work items
//! are generated -> normal execution continues.
use july_workspace::application::DeliberationService;
use july_workspace::domain::{
    Agent, AgentId, Conversation, ConversationId, ConversationKind, DecisionOutcome, DecisionOwner,
    DecisionStatus, DecisionType, DecisionWork, Handoff, HandoffChallenge, HandoffId,
    HandoffResponse, HandoffStatus, Proposal, ProposalId, ProposalResponse, ProposalResponseId,
    ProposalResponseType, ProposalStatus, Room, RoomId, WorkItemId, WorkStatus,
};
use july_workspace::runtime::StorageWorker;
use july_workspace::storage::SqliteStore;
use serde_json::json;
use std::path::{Path, PathBuf};

const T0: &str = "2026-08-25T08:00:00Z";
const T1: &str = "2026-08-25T09:00:00Z";
const T2: &str = "2026-08-25T10:00:00Z";
const T3: &str = "2026-08-25T11:00:00Z";
const T4: &str = "2026-08-25T12:00:00Z";

struct TestDatabase {
    directory: PathBuf,
    path: PathBuf,
}

impl TestDatabase {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("july-deliberation-e2e-{}", ulid::Ulid::generate()));
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

fn agent(name: &str) -> Agent {
    Agent {
        id: AgentId::new(),
        name: format!("{name}-{}", ulid::Ulid::generate()),
        project_root: format!("/workspace/{name}"),
        transport_type: "acp".into(),
        transport_config: json!({}),
        status: "active".into(),
        metadata: json!({}),
        created_at: T0.into(),
        updated_at: T0.into(),
    }
}

struct Workspace {
    thread_id: ConversationId,
    other_thread_id: ConversationId,
    work_id: WorkItemId,
    cashpoint: AgentId,
    pay: AgentId,
    architect: AgentId,
}

fn thread(room_id: RoomId, title: &str) -> Conversation {
    Conversation {
        id: ConversationId::new(),
        kind: ConversationKind::Thread,
        room_id: Some(room_id),
        title: Some(title.into()),
        goal: Some("Settle callback retry ownership".into()),
        parent_conversation_id: None,
        origin_conversation_id: None,
        status: "open".into(),
        created_at: T0.into(),
        updated_at: T0.into(),
    }
}

fn seed(path: &Path) -> Workspace {
    let mut store = SqliteStore::open(path).unwrap();
    let room = Room {
        id: RoomId::new(),
        name: format!("Payments {}", ulid::Ulid::generate()),
        description: None,
        status: "active".into(),
        created_at: T0.into(),
        updated_at: T0.into(),
    };
    let disputed = thread(room.id, "Callback retry");
    let unrelated = thread(room.id, "Voucher export");
    let cashpoint = agent("cashpoint");
    let pay = agent("pay");
    let architect = agent("architect");
    store.insert_room(&room).unwrap();
    for member in [&cashpoint, &pay, &architect] {
        store.insert_agent(member).unwrap();
        store.add_room_member(room.id, member.id, None, T0).unwrap();
    }
    let work_id = WorkItemId::new();
    store
        .create_thread_with_primary_work(
            &disputed,
            work_id,
            "tony",
            &[cashpoint.id, pay.id, architect.id],
        )
        .unwrap();
    store
        .create_thread_with_primary_work(&unrelated, WorkItemId::new(), "tony", &[cashpoint.id])
        .unwrap();
    Workspace {
        thread_id: disputed.id,
        other_thread_id: unrelated.id,
        work_id,
        cashpoint: cashpoint.id,
        pay: pay.id,
        architect: architect.id,
    }
}

fn proposal(workspace: &Workspace, author: AgentId, title: &str, approach: &str) -> Proposal {
    Proposal {
        id: ProposalId::new(),
        thread_id: workspace.thread_id,
        author_agent_id: author,
        title: title.into(),
        problem_statement: Some("Callbacks time out under load".into()),
        approach: Some(approach.into()),
        benefits: vec!["fewer lost callbacks".into()],
        costs: vec!["someone owns the retry budget".into()],
        risks: vec!["duplicate callbacks".into()],
        assumptions: vec!["callbacks are idempotent".into()],
        evidence: vec!["log:callback_timeout".into()],
        status: ProposalStatus::Open,
        supersedes_proposal_id: None,
        created_at: T2.into(),
        updated_at: T2.into(),
    }
}

#[tokio::test]
async fn a_disputed_handoff_converges_into_a_decision_and_executable_work() {
    let database = TestDatabase::new();
    let workspace = seed(database.path());
    let mut service = DeliberationService::new(StorageWorker::open(database.path()).unwrap());

    // 1. cashpoint claims Pay owns the issue.
    let handoff = service
        .propose_handoff(Handoff {
            id: HandoffId::new(),
            thread_id: workspace.thread_id,
            work_id: workspace.work_id,
            from_agent_id: workspace.cashpoint,
            to_agent_id: workspace.pay,
            status: HandoffStatus::Proposed,
            reason: Some("This issue belongs to Pay".into()),
            evidence: vec!["src/cashpoint/voucher.rs".into()],
            owned_scope: Vec::new(),
            rejected_scope: Vec::new(),
            proposed_owner_id: None,
            round_count: 0,
            decision_id: None,
            created_at: T0.into(),
            updated_at: T0.into(),
        })
        .await
        .unwrap();

    // 2. pay rejects with code and test evidence, and names an alternate owner.
    let rejected = service
        .respond_to_handoff(
            handoff.id,
            HandoffResponse::reject(
                workspace.pay,
                "Pay returns transaction_ref per the current contract",
                vec![
                    "src/payment/callback.rs".into(),
                    "test:payment_contract".into(),
                ],
            )
            .with_proposed_owner(workspace.cashpoint),
            T1.into(),
        )
        .await
        .unwrap();
    assert_eq!(rejected.status, HandoffStatus::Rejected);
    assert_eq!(
        SqliteStore::open(database.path())
            .unwrap()
            .get_work_item(workspace.work_id)
            .unwrap()
            .unwrap()
            .owner_agent_id,
        None,
        "a rejection never reassigns work on its own"
    );

    // 3. cashpoint challenges with new evidence; pay holds its position.
    let (reopened, none_yet) = service
        .challenge_handoff(
            handoff.id,
            HandoffChallenge::new(workspace.cashpoint, vec!["log:callback_timeout".into()])
                .decided_by(DecisionOwner::Agent(workspace.architect)),
            T1.into(),
        )
        .await
        .unwrap();
    assert_eq!(reopened.status, HandoffStatus::Proposed);
    assert!(none_yet.is_none());
    service
        .respond_to_handoff(
            handoff.id,
            HandoffResponse::reject(
                workspace.pay,
                "The contract still puts mapping in Cashpoint",
                vec!["test:payment_contract_v2".into()],
            ),
            T1.into(),
        )
        .await
        .unwrap();

    // 4. The second challenge exhausts the budget: the dispute stops.
    let (disputed, decision) = service
        .challenge_handoff(
            handoff.id,
            HandoffChallenge::new(workspace.cashpoint, vec!["commit:9f21a".into()])
                .decided_by(DecisionOwner::Agent(workspace.architect)),
            T2.into(),
        )
        .await
        .unwrap();
    assert_eq!(disputed.status, HandoffStatus::Disputed);
    let decision = decision.expect("an exhausted dispute must escalate");
    assert_eq!(decision.status, DecisionStatus::NeedsDecision);
    assert_eq!(decision.decision_type, DecisionType::Ownership);
    assert!(
        service
            .challenge_handoff(
                handoff.id,
                HandoffChallenge::new(workspace.cashpoint, vec!["commit:bb44c".into()]),
                T2.into(),
            )
            .await
            .is_err(),
        "no further automatic turns after escalation"
    );

    // 5. Both sides put a proposal on the table.
    let plan_a = service
        .create_proposal(proposal(
            &workspace,
            workspace.cashpoint,
            "Retry inside Pay",
            "Pay retries the callback with backoff",
        ))
        .await
        .unwrap();
    let plan_b = service
        .create_proposal(proposal(
            &workspace,
            workspace.pay,
            "Durable queue",
            "Both sides publish to a durable queue",
        ))
        .await
        .unwrap();
    service
        .respond_to_proposal(ProposalResponse {
            id: ProposalResponseId::new(),
            proposal_id: plan_a.id,
            agent_id: workspace.pay,
            response_type: ProposalResponseType::Challenge,
            reason: Some("Retry hides the missing contract field".into()),
            evidence: vec!["test:payment_contract_v2".into()],
            created_at: T2.into(),
        })
        .await
        .unwrap();

    // 6. The named decision owner settles it by selecting plan B.
    let decided = service
        .decide(
            decision.id,
            DecisionOutcome {
                selected_proposal_id: Some(plan_b.id),
                ..DecisionOutcome::new(
                    DecisionOwner::Agent(workspace.architect),
                    "Adopt the durable queue; Pay owns retry, Cashpoint owns mapping",
                )
                .because(
                    "Neither side owns the whole path",
                    vec!["doc:sla".into(), "commit:9f21a".into()],
                )
                .assigning_owner(workspace.pay)
            },
            T3.into(),
        )
        .await
        .unwrap();
    assert_eq!(decided.status, DecisionStatus::Decided);

    // 7. The decision generates executable work, and normal execution follows.
    let implement =
        DecisionWork::new("Publish callbacks to the durable queue").owned_by(workspace.pay);
    let verify = DecisionWork::new("Map queue events into voucher records")
        .owned_by(workspace.cashpoint)
        .after(implement.work_id);
    let generated = service
        .convert_decision_to_work(
            decided.id,
            vec![implement.clone(), verify.clone()],
            T4.into(),
        )
        .await
        .unwrap();
    assert_eq!(generated.len(), 2);

    // Everything survives a restart, with the audit trail intact.
    let mut store = SqliteStore::open(database.path()).unwrap();
    let settled_handoff = store.get_handoff(handoff.id).unwrap().unwrap();
    assert_eq!(settled_handoff.status, HandoffStatus::Resolved);
    assert_eq!(settled_handoff.decision_id, Some(decision.id));
    assert!(
        settled_handoff
            .evidence
            .contains(&"test:payment_contract_v2".to_owned()),
        "evidence stays attached to the claim: {:?}",
        settled_handoff.evidence
    );
    let stored_decision = store.get_decision(decision.id).unwrap().unwrap();
    assert_eq!(stored_decision.selected_proposal_id, Some(plan_b.id));
    assert_eq!(stored_decision.evidence.len(), 2);
    assert_eq!(
        store.get_proposal(plan_b.id).unwrap().unwrap().status,
        ProposalStatus::Accepted
    );
    assert_eq!(
        store.get_proposal(plan_a.id).unwrap().unwrap().status,
        ProposalStatus::Open,
        "an unselected proposal is not silently rejected"
    );
    assert_eq!(
        store
            .get_work_item(workspace.work_id)
            .unwrap()
            .unwrap()
            .owner_agent_id,
        Some(workspace.pay),
        "ownership moved because a decision said so, not because of a claim"
    );
    assert_eq!(store.list_decision_work(decided.id).unwrap().len(), 2);

    // Decision and Work stay distinct, and execution proceeds normally.
    store
        .transition_work(implement.work_id, WorkStatus::Working, T4)
        .unwrap();
    assert_eq!(
        store
            .get_work_item(implement.work_id)
            .unwrap()
            .unwrap()
            .status,
        WorkStatus::Working
    );
    assert_eq!(
        store.get_decision(decision.id).unwrap().unwrap().status,
        DecisionStatus::Decided
    );

    // Context isolation: the unrelated thread learns nothing from any of this.
    assert!(
        store
            .list_decisions_for_thread(workspace.other_thread_id)
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .list_proposals_for_thread(workspace.other_thread_id)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store
            .list_proposals_for_thread(workspace.thread_id)
            .unwrap()
            .len(),
        2
    );
}
