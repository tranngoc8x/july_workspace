# July Workspace — Agent Deliberation & Ownership Negotiation Upgrade Plan

## Status

**Planned upgrade — deferred until the collaboration substrate is complete.**

This plan must **not** expand the scope of the currently running Phase 2.

Recommended implementation point:

```text
Phase 6 complete
    ↓
Phase 6.5 — Agent Deliberation & Decision Protocol
    ↓
Phase 7 — Memory + Session Recovery
```

The feature depends on:

- Phase 4 — Room + Thread;
- Phase 5 — Agent-to-Agent Messaging;
- Phase 6 — Work / Result / Publish / Dependency.

---

# 1. Motivation

July currently models collaboration mainly as:

```text
message
→ work
→ result
→ dependency
```

Real engineering collaboration also includes:

```text
claim
→ challenge
→ evidence
→ ownership negotiation
→ proposal
→ decision
→ work
→ result
```

Examples:

### Ownership dispute

```text
cashpoint:
This issue belongs to Pay.

pay:
I checked Pay.
The contract is correct.
This belongs to Cashpoint.
```

### Technical debate

```text
cashpoint:
Retry in Cashpoint.

pay:
Retry belongs to Pay because Pay owns delivery.

infra:
Both approaches are weaker than a durable queue.
```

July should support this without turning into an unrestricted multi-agent chat loop.

---

# 2. Product Goal

Enable a Thread to behave like a focused engineering meeting where agents can:

- accept or reject ownership;
- request missing evidence;
- challenge assumptions;
- propose alternatives;
- amend proposals;
- record technical decisions;
- assign resulting work;
- escalate unresolved disagreements;
- converge without infinite discussion.

Target flow:

```text
Discussion
    ↓
Evidence
    ↓
Decision
    ↓
Executable work
```

Not:

```text
agents chat forever
```

---

# 3. Design Principles

## 3.1 Evidence before persuasion

> Disagreement should converge through evidence, not rhetoric.

Preferred evidence:

- source file/reference;
- test result;
- API contract;
- commit;
- runtime/log evidence;
- documented business constraint.

A statement like “this is your task” is a claim, not a decision.

## 3.2 Agents may reject work

A project agent must be allowed to respond:

```text
ACCEPT
REJECT
PARTIAL
```

Forced acceptance would recreate the weaknesses of a central supervisor.

## 3.3 Thread is the meeting boundary

Do not introduce a separate `Meeting` entity initially.

A Thread already provides:

- scoped members;
- isolated context;
- work lifecycle;
- messages;
- results;
- dependencies.

Deliberation is a Thread capability.

## 3.4 Runtime controls the protocol

Deterministic runtime manages:

- handoff state;
- proposal state;
- challenge rounds;
- decision state;
- escalation threshold;
- resulting WorkItems.

LLM reasoning handles semantic content, not protocol bookkeeping.

## 3.5 Debate must be bounded

No unbounded:

```text
agent A → agent B → agent A → agent B → ...
```

If no meaningful progress occurs within the debate budget:

```text
→ NEEDS_DECISION
```

## 3.6 Decision is durable

A technical or ownership decision becomes workspace state.

It must not exist only as a buried chat message.

---

# 4. Thread Model Extension

Current conceptual Thread:

```text
Thread
├── Messages
├── WorkItems
├── Results
└── Dependencies
```

Extended:

```text
Thread
├── Messages
├── Handoffs
├── Proposals
├── Decisions
├── WorkItems
├── Results
└── Dependencies
```

No generic Meeting/Debate framework is required.

---

# 5. Thread Phase

Do **not** overload `WorkStatus`.

Add an independent Thread phase:

```text
DISCOVERY
DELIBERATION
EXECUTION
REVIEW
CLOSED
```

A Thread need not visit every phase.

Simple work:

```text
EXECUTION → CLOSED
```

Complex design issue:

```text
DISCOVERY
→ DELIBERATION
→ EXECUTION
→ REVIEW
→ CLOSED
```

Phase transitions should initially be explicit or driven by structured events rather than inferred from arbitrary conversation.

---

# 6. Handoff / Ownership Negotiation

## 6.1 Purpose

A Handoff means:

> Agent A proposes that Agent B owns all or part of a piece of work.

It is not merely a chat message.

## 6.2 Handoff states

```text
PROPOSED
ACCEPTED
REJECTED
PARTIAL
DISPUTED
RESOLVED
CANCELLED
```

Suggested flow:

```text
PROPOSED
├── ACCEPTED → target owns work
├── PARTIAL  → split ownership
└── REJECTED
       ↓
source accepts rejection
       ↓
RESOLVED

or

source challenges rejection
       ↓
DISPUTED
```

## 6.3 Ownership response

Structured response:

```text
decision: ACCEPT | REJECT | PARTIAL
reason
evidence[]
proposed_owner?
owned_scope?
rejected_scope?
required_input?
```

Example:

```json
{
  "decision": "REJECT",
  "reason": "Pay returns transaction_ref according to the current contract.",
  "evidence": [
    "src/payment/callback.rs",
    "test:payment_contract"
  ],
  "proposed_owner": "cashpoint"
}
```

Partial ownership:

```json
{
  "decision": "PARTIAL",
  "owned_scope": [
    "add new callback field"
  ],
  "rejected_scope": [
    "map callback into voucher record"
  ],
  "proposed_owner": "cashpoint"
}
```

---

# 7. Ownership Dispute Protocol

## 7.1 Normal flow

```text
A proposes ownership to B
        ↓
B investigates
        ↓
B ACCEPT / PARTIAL / REJECT
```

If rejected and A accepts the evidence:

```text
→ RESOLVED
```

If A disagrees:

```text
→ DISPUTED
```

## 7.2 Bounded dispute

Initial recommendation:

```text
max structured challenge rounds = 2
```

A round consists of:

```text
claim/challenge
+
response
+
new evidence or explicit no-new-evidence result
```

If unresolved:

```text
handoff.status = DISPUTED
thread.phase = DELIBERATION
decision.status = NEEDS_DECISION
```

No further automatic ping-pong.

## 7.3 Resolution

Supported decision owners:

### User

Default safest behavior.

```text
decision_owner = user
```

### Named agent

Example:

```text
decision_owner = architect
```

### July semantic facilitator

May produce a `RECOMMENDATION`, but should not override agents by default.

---

# 8. Proposal Model

A `Proposal` captures one candidate solution.

Suggested fields:

```text
id
thread_id
author_agent_id
title
problem_statement
approach
benefits[]
costs[]
risks[]
assumptions[]
evidence[]
status
supersedes_proposal_id?
created_at
```

States:

```text
OPEN
AMENDED
ACCEPTED
REJECTED
SUPERSEDED
WITHDRAWN
```

Responses:

```text
SUPPORT
CHALLENGE
AMEND
REJECT
```

A challenge should contain at least one of:

- counter-evidence;
- violated constraint;
- unhandled risk;
- incorrect assumption;
- competing proposal.

Avoid empty disagreement without actionable content.

---

# 9. Decision Criteria

Complex debates may define shared criteria.

Example:

```text
reliability: HIGH
implementation_time: MEDIUM
operational_complexity: MEDIUM
backward_compatibility: HIGH
infrastructure_cost: LOW
```

This is not intended as a universal voting/scoring engine.

It gives agents a common evaluation frame.

---

# 10. Decision Model

A `Decision` is a durable conclusion.

Suggested fields:

```text
id
thread_id
decision_type
title
decision
reason
selected_proposal_id?
alternatives_json
evidence_json
participants_json
decision_owner
status
created_at
supersedes_decision_id?
```

Initial decision types:

```text
OWNERSHIP
TECHNICAL
SCOPE
```

Decision states:

```text
PENDING
NEEDS_DECISION
DECIDED
SUPERSEDED
CANCELLED
```

---

# 11. Decision → Work

After a Decision:

```text
Decision
    ↓
WorkItems
```

Example:

```text
Decision:
Pay owns callback retry.

Generated work:
- pay → implement retry
- cashpoint → integration test
```

This conversion must be explicit, deterministic and auditable.

---

# 12. July Facilitator Role

## Runtime role

July Runtime acts as meeting infrastructure:

- invite/manage participants;
- store structured artifacts;
- track dispute rounds;
- enforce limits;
- record decisions;
- create downstream work/dependencies;
- stop loops.

No LLM required.

## Semantic facilitator

Optional LLM facilitator may:

- summarize disagreement;
- identify conflicting assumptions;
- identify missing evidence;
- compare proposals;
- ask a targeted question;
- recommend a compromise;
- prepare a decision summary.

It receives compact structured context:

```text
problem
participants
claims
evidence
proposals
decision criteria
unresolved points
```

not full workspace history.

## Authority

Default:

```text
facilitator != decision_owner
```

The facilitator recommends.
The configured decision owner decides.

---

# 13. Provisional Database Additions

Do **not** freeze these schemas during Phase 2.

They should be finalized only after Phase 6 behavior exists.

## Handoffs

```text
handoffs
- id
- thread_id
- work_id
- from_agent_id
- to_agent_id
- status
- reason
- evidence_json
- proposed_owner_id
- round_count
- created_at
- updated_at
```

## Proposals

```text
proposals
- id
- thread_id
- author_agent_id
- title
- problem_statement
- approach
- benefits_json
- costs_json
- risks_json
- assumptions_json
- evidence_json
- status
- supersedes_proposal_id
- created_at
- updated_at
```

## Proposal responses

```text
proposal_responses
- id
- proposal_id
- agent_id
- response_type
- reason
- evidence_json
- created_at
```

## Decisions

```text
decisions
- id
- thread_id
- decision_type
- title
- decision
- reason
- selected_proposal_id
- alternatives_json
- evidence_json
- decision_owner
- status
- supersedes_decision_id
- created_at
```

---

# 14. Integration with the Existing Roadmap

## Phase 2 — AgentTransport + ACP

**No scope change.**

Do not introduce:

- Handoff;
- Proposal;
- Decision;
- debate behavior;
- semantic facilitator.

The current Phase 2 remains focused on transport/session correctness.

## Phase 3 — DM MVP

No feature expansion.

Only ensure:

- stable conversation identifiers;
- messages can carry extensible typed metadata.

Do not expose deliberation UX.

## Phase 4 — Room + Thread MVP

No major expansion.

Ensure the Thread model does not block future:

```text
phase
decision_owner
structured thread artifacts
```

Do not implement these unless trivial and naturally compatible.

The locked Phase 4 primary Work marker and generational membership model are
compatibility foundations only. Phase 4 does not add Thread phase,
`decision_owner`, Handoff, Proposal, Decision or deliberation commands.

## Phase 5 — Agent-to-Agent Messaging

This provides the communication substrate.

Ensure:

- reliable sender/recipient attribution;
- deterministic dynamic join;
- extensible internal message/event typing.

Do not implement debate semantics here.

## Phase 6 — Work / Result / Publish / Dependency

Complete as currently planned.

This provides prerequisites:

- WorkItem;
- owner;
- Result/evidence;
- dependency graph;
- durable state transitions.

---

# 15. New Phase 6.5 — Agent Deliberation & Decision Protocol

Insert after Phase 6 and before Phase 7.

## 6.5.1 Handoff

Implement:

- ownership proposal;
- ACCEPT;
- REJECT;
- PARTIAL;
- DISPUTED;
- evidence;
- proposed alternate owner.

## 6.5.2 Bounded dispute

Implement:

- structured challenge rounds;
- default/configurable max rounds;
- `NEEDS_DECISION`;
- no infinite auto-conversation.

## 6.5.3 Proposal

Implement:

- create proposal;
- SUPPORT;
- CHALLENGE;
- AMEND;
- REJECT;
- evidence/references.

## 6.5.4 Decision

Implement:

- ownership decision;
- technical decision;
- decision owner;
- alternatives;
- evidence;
- supersede behavior.

## 6.5.5 Decision → Work

Implement explicit conversion:

```text
Decision
→ WorkItem(s)
→ Dependency
```

## 6.5.6 Optional semantic facilitator

Only after the structured deterministic protocol works.

Implement:

- disagreement summary;
- missing evidence detection;
- proposal comparison;
- recommendation.

The system must work without the facilitator.

---

# 16. Phase 6.5 Definition of Done

Required E2E scenario:

```text
cashpoint claims Pay owns issue
        ↓
handoff to pay
        ↓
pay REJECTS with code/test evidence
        ↓
cashpoint CHALLENGES with evidence
        ↓
dispute does not loop forever
        ↓
proposals A/B created
        ↓
decision owner selects B
        ↓
Decision persisted
        ↓
WorkItems generated
        ↓
normal execution continues
```

Guarantees:

- no unbounded agent loop;
- ownership changes are auditable;
- evidence stays attached to claims/proposals/decisions;
- decisions survive restart;
- rejected work is not silently reassigned;
- Decision and Work remain distinct;
- thread context isolation remains intact.

---

# 17. Tests

## Handoff

- ACCEPT;
- REJECT;
- PARTIAL;
- alternate owner;
- target offline;
- duplicate response;
- invalid state transition.

## Dispute

- one challenge resolved;
- max rounds reached;
- no new evidence;
- `NEEDS_DECISION`;
- user resolution;
- named decision-owner resolution.

## Proposal

- create;
- challenge;
- amend;
- supersede;
- withdraw;
- evidence preservation.

## Decision

- ownership;
- technical;
- selected proposal;
- supersede;
- restart persistence.

## Decision → Work

- one work item;
- split ownership;
- dependency creation;
- idempotent conversion.

## Context isolation

Participants receive:

- current Thread context;
- relevant proposals/decisions;

but not:

- unrelated DMs;
- unrelated Threads;
- whole Room history.

## Loop prevention

Repeated mutual rejection must result in:

```text
bounded rounds
→ NEEDS_DECISION
→ no further automatic turns
```

---

# 18. Metrics

Track:

```text
rounds_to_decision
```

Evidence coverage:

```text
evidence-backed disputes / all disputes
```

Escalation rate:

```text
user-escalated disputes / all disputes
```

Token efficiency:

```text
bounded structured deliberation
vs
unrestricted agent chat
```

Ownership correction rate:

```text
final owner differs from initial proposed owner
```

---

# 19. MVP Boundary

Initial upgrade:

```text
✓ Handoff ACCEPT/REJECT/PARTIAL
✓ evidence
✓ bounded disputes
✓ Proposal
✓ Challenge
✓ Decision
✓ Decision → Work
```

Defer:

```text
✗ voting engine
✗ consensus algorithms
✗ complex meeting roles
✗ autonomous management hierarchy
✗ debate tournaments
✗ arbitrary agent councils
✗ generic numeric scoring engine
```

---

# 20. Risks

## Agent loops

Mitigation:
- bounded rounds;
- explicit state machine;
- escalation.

## Over-structuring simple work

Mitigation:
- deliberation is optional;
- simple DM/Thread stays simple.

## Domain explosion

Mitigation:

Only introduce:

```text
Handoff
Proposal
Decision
```

Do not create generic Meeting/Debate/Negotiation entities.

## Facilitator becomes another supervisor

Mitigation:
- facilitator receives scoped structured context;
- recommendation is default;
- routing/state remain deterministic.

---

# 21. Guiding Rule

> July should make collaboration deterministic where possible and use LLM reasoning only where semantics genuinely require it.

The goal is not to make agents talk more.

The goal is to make disagreement converge into:

```text
evidence
→ decision
→ work
→ result
```
