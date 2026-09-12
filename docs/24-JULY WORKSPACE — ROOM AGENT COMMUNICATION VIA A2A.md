# JULY WORKSPACE — ROOM AGENT COMMUNICATION VIA A2A

## 1. Mục tiêu

Thay thế định hướng A2A cũ tập trung vào external-agent interoperability bằng mục tiêu đúng với product intent của July:

> **Các July-managed agents trong cùng một Room làm việc như các teammate thật trong một group chat. Agents dùng mention để gọi nhau, A2A để giao tiếp agent↔agent, ACP để July vận hành từng coding-agent runtime, còn July giữ vai trò bridge, router, persistence layer và membership boundary.**

Primary flow:

```text
Room: VNA
Members: user, cashpoint, pay

user:
@cashpoint @pay kiểm tra refund flow.

cashpoint:
Tôi kiểm tra callback integration.

pay:
Tôi kiểm tra refund/payment state.

cashpoint:
@pay bên tôi đang gửi payment_ref.
Bên pay expect field nào?

pay:
@cashpoint bên tôi expect reference_id.
Có vẻ contract đang lệch.
```

External A2A interoperability **không phải Definition of Done của phase này**.

External agents có thể được hỗ trợ sau như một extension, nhưng không phải primary goal.

---

# 2. Product model cần khóa

User-facing model:

```text
Agent = teammate
Room  = shared team/group chat
@     = attention + routing
Work  = structured durable work state
```

Internal responsibility:

```text
ACP  = July ↔ agent runtime execution
A2A  = agent ↔ agent communication
July = bridge + routing + persistence + policy + recovery
```

Hard rule:

```text
ACP != A2A
```

Không coi ACP và A2A là hai runtime adapter ngang hàng.

Một logical Agent có thể đồng thời:

```text
cashpoint
├── runtime: ACP → Codex
└── collaboration: A2A → pay
```

---

# 3. Kiến trúc mục tiêu

```text
                         JULY WORKSPACE

                              Room
                 ┌─────────────┴─────────────┐
                 │                           │
             Room Chat                  Work Manager
                 │                           │
        RoomMessage / mentions          Work / Result
        replies / shared history        Artifact / Decision
                 │                           │
                 └─────────────┬─────────────┘
                               │
                     Collaboration Bridge
                               │
                              A2A
                    ┌──────────┴──────────┐
                    │                     │
                cashpoint                pay
                    │                     │
                   ACP                   ACP
                    │                     │
                  Codex             Claude Code
```

Responsibilities:

```text
Room
→ membership boundary
→ shared visible conversation

Room Chat
→ canonical shared conversation log

A2A
→ agent-to-agent interaction / delivery

ACP
→ run/resume/send input to coding-agent runtime

Work Manager
→ structured durable work
→ ownership
→ dependencies
→ results
→ artifacts
→ decisions

July
→ identity
→ routing
→ persistence
→ membership enforcement
→ activation
→ recovery
```

---

# 4. Hard architecture invariants

## 4.1 Room là collaboration boundary

Agent-agent communication chỉ được phép khi sender và target cùng là member của Room.

```text
A → B allowed
IFF
A ∈ Room
AND
B ∈ Room
```

Không hỗ trợ:

```text
agent → agent DM outside Room
cross-room A2A by default
silent auto-add member
```

Ví dụ:

```text
Room VNA:
- cashpoint
- pay
```

Nếu cashpoint gửi:

```text
@infra check staging
```

và infra không thuộc VNA:

```text
infra is not a member of VNA.

Add infra to this room before messaging it.
```

Không DM infra.

Không cross-room route.

Không tự thêm infra.

---

## 4.2 Room là conversation thật

Room không còn chỉ là:

```text
namespace
member pool
work launcher
```

Room phải có **durable shared chat log**.

Target:

```text
Room
├── Members
├── Chat
├── Work
├── Results
├── Artifacts
└── Decisions
```

---

## 4.3 Shared Room messages khác private runtime transcript

```text
Room Chat
= intentionally shared collaboration history

Agent Runtime Transcript
= private execution/reasoning context
```

Principle:

> **Room messages and explicit Results/Artifacts may cross agent boundaries. Private runtime transcripts do not.**

Agent có thể:

```text
inspect files
run commands
use tools
retry
reason internally
```

nhưng Room chỉ thấy output agent chủ động chia sẻ.

---

## 4.4 Mention quyết định activation

Phải tách:

```text
message visibility
```

khỏi:

```text
agent activation
```

MVP:

```text
mentioned agent
→ activate / wake

not mentioned
→ message persisted and visible
→ do not wake by default
```

Không broadcast mọi Room message tới mọi agent runtime.

---

## 4.5 Routing deterministic

Explicit mention không dùng LLM.

```text
@pay
→ AgentRegistry
→ validate Room membership
→ route
```

July không reason xem “agent nào phù hợp” khi target đã explicit.

---

# 5. Canonical RoomMessage

Room cần message model riêng.

Conceptual:

```rust
struct RoomMessage {
    id: RoomMessageId,
    room_id: RoomId,
    sender: ParticipantId,
    body: String,
    mentions: Vec<AgentId>,
    reply_to: Option<RoomMessageId>,
    created_at: Timestamp,
}
```

Sender có thể là:

```text
User
Agent(cashpoint)
Agent(pay)
System
```

Có thể mở rộng sau:

```text
artifact_refs
result_refs
work_refs
attachments
edited_at
```

`RoomMessage` là canonical workspace record.

Không dùng raw A2A Message làm database/domain model của Room.

---

# 6. User → Agent flow

User:

```text
[VNA] > @cashpoint kiểm tra callback retry
```

July:

```text
1. parse mention
2. resolve cashpoint
3. validate cashpoint ∈ VNA
4. persist RoomMessage
5. render message in Room
6. activate/resume cashpoint ACP runtime
7. deliver relevant Room context
8. receive shared agent response
9. persist response as RoomMessage
10. render response in Room
```

Không bắt buộc tạo Work.

---

# 7. User → multiple agents

User:

```text
[VNA] > @cashpoint @pay kiểm tra refund flow
```

July:

```text
persist one RoomMessage
        ↓
resolve mentions
        ↓
cashpoint ∈ VNA ✓
pay ∈ VNA ✓
        ↓
activate cashpoint
activate pay
```

Cả hai responses xuất hiện trong Room:

```text
cashpoint:
Tôi kiểm tra integration.

pay:
Tôi kiểm tra refund API.
```

Không tự tạo Work chỉ vì có nhiều mentions.

---

# 8. Agent → Agent flow

Cashpoint:

```text
@pay check refund contract.
```

Flow:

```text
cashpoint ACP runtime
        │
        │ collaboration intent
        ▼
July Room Messaging
        │
        ├── validate cashpoint ∈ Room
        ├── resolve @pay
        ├── validate pay ∈ Room
        ├── persist RoomMessage
        └── render RoomMessage
                │
                ▼
            A2A Bridge
                │
                ▼
            pay Agent
                │
               ACP
                │
                ▼
            pay runtime
                │
                ▼
        shared response
                │
                ▼
          RoomMessage
```

User nhìn thấy toàn bộ shared conversation.

---

# 9. A2A role

A2A không phải Room source of truth.

```text
RoomMessage
    ↓
A2A representation
    ↓
target Agent
```

Có thể attach metadata:

```text
july.room_id
july.room_message_id
july.sender_agent_id
july.target_agent_id
```

Nhưng July domain không phụ thuộc A2A wire schema.

---

# 10. ACP role

ACP vẫn chỉ là runtime execution layer.

```text
logical agent
      │
     ACP
      │
      ▼
Codex / Claude Code / ...
```

Không model:

```text
adapter = acp OR a2a
```

Đúng conceptual separation:

```text
AgentRuntimeBinding {
    agent_id
    runtime_adapter
    runtime_ref
}
```

và:

```text
AgentCommunication {
    protocol
}
```

Ví dụ:

```text
cashpoint.runtime = ACP/Codex
cashpoint.communication = A2A
```

Reuse existing types nếu code hiện tại đã có equivalent.

---

# 11. A2A façade do July quản lý

Codex/Claude Code không cần tự trở thành A2A server.

July cung cấp A2A façade cho logical Agent.

```text
pay logical agent
┌──────────────────────────────┐
│ July-managed A2A façade      │
│                              │
│ receive interaction          │
│          ↓                   │
│ Agent Session Manager        │
│          ↓ ACP               │
│ Claude/Codex                 │
└──────────────────────────────┘
```

Không để runtime biết network topology:

```text
Codex
→ http://localhost/.../pay
```

Ưu tiên:

```text
Agent runtime
    ↓ July collaboration API/tool
July
    ↓ A2A bridge
target logical agent
```

---

# 12. Agent-facing messaging interface

Agent cần interface để gửi message tới Room member khác.

Conceptual:

```text
send_room_message(
    target_agent = "pay",
    body = "Which refund status is public?"
)
```

Hoặc tool tương đương.

Agent không cần biết:

```text
A2A endpoint
protocol version
HTTP URL
authentication
runtime session ID
provider
model
```

July resolve tất cả.

---

# 13. Mention parsing

Support:

```text
@cashpoint
@pay
@cashpoint @pay
```

Validation:

```text
agent exists?
sender belongs to Room?
target belongs to Room?
```

Unknown:

```text
Unknown agent: @foo
```

Not member:

```text
pay is not a member of VNA.
```

Không silently reroute.

---

# 14. Message visibility vs activation

All members có thể xem shared Room messages.

Nhưng chỉ mentioned agents được wake.

```text
@cashpoint
→ wake cashpoint

@pay
→ wake pay

@cashpoint @pay
→ wake both

no mention
→ persist/display only
```

Có thể thêm sau:

```text
@all
@room
```

Không cần trong MVP.

---

# 15. Agent context delivery

Khi agent được mention, không replay toàn bộ Room history.

Deliver:

```text
Room identity
current message
bounded recent Room context
new shared messages since last cursor
explicit Work/Result/Artifact references
```

Conceptual:

```text
AgentRoomCursor {
    agent_id
    room_id
    last_seen_message_id
}
```

Agent private ACP session vẫn sở hữu model context riêng.

Goal:

```text
incremental shared context
```

thay vì:

```text
full Room replay
```

---

# 16. Room chat rendering

Primary UX:

```text
VNA
cashpoint · pay

────────────────────────────────────

you
@cashpoint @pay kiểm tra refund flow.

cashpoint
Tôi sẽ kiểm tra callback integration.

pay
Tôi kiểm tra refund/payment state.

cashpoint
@pay bên tôi đang gửi payment_ref.
Bên pay expect field nào?

pay
@cashpoint bên tôi expect reference_id.

────────────────────────────────────
[vna] > _
```

Agent identity phải rõ ràng.

System lifecycle events chỉ nên nhẹ:

```text
pay • working
pay ✓ completed
```

Không đưa low-level ACP/tool events vào Room chat.

---

# 17. Work không phải prerequisite của chat

Không:

```text
mention
→ always create Work
```

Ví dụ:

```text
cashpoint:
@pay API đang dùng status nào?
```

chỉ cần conversation.

Structured Work chỉ được tạo khi thực sự có work lifecycle.

Ví dụ:

```text
cashpoint:
@pay implement signature validation and add contract tests.
```

Có thể tạo:

```text
Work {
    room_id = VNA
    requester = cashpoint
    owner = pay
}
```

Mental model:

```text
Chat = primary interaction
Work = durable structured state behind chat
```

---

# 18. A2A Message vs A2A Task

## Message

Dùng cho:

```text
question
clarification
information exchange
short collaboration
```

Ví dụ:

```text
cashpoint → pay:
Which refund status should cashpoint consume?
```

## Task

Dùng khi target cần thực hiện work có lifecycle.

Ví dụ:

```text
cashpoint → pay:
Implement signature verification and return test evidence.
```

User không cần biết distinction này.

UI vẫn là Room chat.

---

# 19. Work ↔ A2A Task

July Work vẫn canonical.

```text
July Work
    ↕
A2A Task Binding
```

Ví dụ:

```text
Work #43
room = VNA
requester = cashpoint
owner = pay
status = working

A2ABinding
work_id = #43
a2a_task_id = ...
```

Không:

```text
A2A Task = July source of truth
```

---

# 20. Existing collaboration semantics

Các semantics hiện có:

```text
Handoff
Proposal
Decision
ACCEPT
REJECT
PARTIAL
DISPUTED
CHALLENGE
AMEND
```

không cần rewrite ngay.

Nhưng tránh biến chúng thành một communication protocol cạnh A2A.

Ưu tiên:

```text
A2A Message / Task
+
July collaboration metadata/state
```

Giữ:

```text
bounded deliberation
anti-loop
Decision state
ownership
dependency
```

nếu implementation hiện tại đang hoạt động tốt.

---

# 21. Agent Registry và Agent Card

Internal routing:

```text
@pay
  ↓
AgentRegistry
  ↓
logical pay agent
```

Không cần remote Agent Card discovery cho July-managed agents.

Agent Card có thể synthesize từ:

```text
name
description
skills
capabilities
```

nhưng public HTTP discovery không phải MVP requirement.

---

# 22. Membership enforcement

Mọi agent-agent route phải đi qua membership validation.

```text
send_room_message(sender, target, room):

    assert sender ∈ room
    assert target ∈ room

    persist
    route
```

Required case:

```text
cashpoint ∈ VNA
pay ∈ VNA
infra ∉ VNA

cashpoint → @pay
PASS

cashpoint → @infra
REJECT
```

---

# 23. Persistence

Persist tối thiểu:

```text
Room
RoomMember
RoomMessage
AgentRoomCursor

Work
Result
Artifact
Decision

A2A task/message bindings where required
```

Restart không được mất:

```text
Room chat history
membership
agent Room cursors
active Work ownership
A2A task bindings
```

---

# 24. Recovery

Sau restart:

```text
1. load Room
2. load Room members
3. load RoomMessage history
4. restore AgentRoomCursor
5. restore active Work
6. restore runtime bindings
7. resume/recreate ACP sessions
8. reconcile unfinished A2A interactions
```

Implementation (Phase 10): runtime startup atomically marks interrupted Room activations (`claimed`/`sent`) as `failed` and retires their bindings as `lost`. Completed activations, Room history/membership, cursors, Work/Result and A2A bindings remain unchanged. Inspection-only opens do not reconcile live runtime state.

Recovery is lazy on a new explicit Room message: reuse a resumable session, or create one durable replacement generation after a `lost` binding or a definitive ACP `SessionLost` response. Closed sessions remain terminal. An uncertain resume/send error never retries the old message. Persisted messages are not automatically dispatched at startup.

A recreated session receives at most 50 preceding shared messages and 20 relevant unfinished Work references (including task IDs). History is context, not a request to repeat interrupted work. Normal resumes retain cursor-based incremental context. The cursor advances only after successful completion of the new turn.

Không replay toàn bộ Room history vào mọi agent.

---

# 25. Private transcript isolation

Required invariant:

```text
ACP/model transcript
!=
RoomMessage
```

Agent có thể thực hiện hàng chục tool calls nhưng Room chỉ nhận:

```text
explicit shared message
Result
Artifact
useful lifecycle status
```

Không publish raw runtime transcript.

---

# 26. Slash command impact

Normal collaboration:

```text
/room vna

@cashpoint @pay check refund flow
```

sau đó agents tự trao đổi bằng mentions.

Không cần:

```text
/thread new
/work new
/delegate
/handoff
/send
```

để bắt đầu làm việc.

Slash commands chỉ dùng cho:

```text
navigation
inspection
workspace control
```

---

# 27. Thread

Thread không còn là prerequisite.

Target model:

```text
Room
├── Chat
└── Work
```

Nếu Thread vẫn tồn tại trong storage/domain:

```text
do not delete immediately
```

Nhưng A2A mới:

```text
must not require Thread
must not route through Thread
must not require user to enter Thread
```

Audit/xóa Thread là refactor riêng sau khi Room chat ổn định.

---

# 28. Migration khỏi A2A plan cũ

## REMOVE / REPLACE

Không tiếp tục dùng primary goal:

```text
A2A = external agent interoperability
```

Không tiếp tục architecture:

```text
AgentAdapter
├── ACP
├── SDK
└── A2A
```

nếu nó có nghĩa:

```text
A2A = runtime alternative to ACP
```

Không dùng external-agent registration/discovery/authentication làm DoD của phase này.

Replace bằng:

```text
ACP
→ runtime execution

A2A
→ internal Room agent communication
```

---

# 29. Phần từ plan cũ cần KEEP

Giữ nếu implementation hiện tại đã đúng:

```text
logical Agent identity
runtime-independent Agent identity
ACP runtime adapter
AgentRegistry
Work ownership
Result
Artifact
Decision
runtime binding
error normalization
persistence/recovery
private transcript isolation
bounded deliberation
result propagation
```

Không rewrite các abstraction tốt chỉ vì đổi hướng A2A.

---

# 30. External A2A interoperability

Deferred.

Sau khi internal Room flow hoạt động:

```text
July-managed agent
       ↕ A2A
external agent
```

mới là extension phase tiếp theo.

Có thể reuse:

```text
A2A Message mapping
A2A Task mapping
Artifact mapping
Agent Card
```

Nhưng external agent support không nằm trong acceptance criteria của plan này.

---

# 31. Implementation sequence

## Phase 1 — Audit implementation A2A hiện tại

Search:

```text
A2AAdapter implements AgentAdapter
adapter = a2a
external_agent
remote_task_id
AgentCard discovery
A2A server/client

RoomMessage
Room persistence
mentions
Room membership validation
```

Classify từng phần:

```text
KEEP
MOVE
MODIFY
REMOVE
DEFER
```

Mục đích là tránh rewrite phần đã đúng.

---

## Phase 2 — Durable Room chat

Implement/verify:

```text
RoomMessage
sender identity
mentions
reply_to
persistence
Room history loading
rendering
```

Room trở thành canonical chat ledger.

---

## Phase 3 — Mention router

Implement deterministic:

```text
parse mentions
resolve AgentRegistry
validate Room membership
persist message
activate target(s)
```

Không LLM.

---

## Phase 4 — AgentRoomCursor

Implement incremental Room synchronization:

```text
agent_id
room_id
last_seen_message_id
```

Không full history replay.

---

## Phase 5 — Agent-facing Room messaging API

Cho runtime gửi shared message tới Room member khác.

Ví dụ conceptual:

```text
send_room_message(
    to = "pay",
    body = "..."
)
```

July giữ responsibility routing/membership.

---

## Phase 6 — Internal A2A bridge

Implement mapping:

```text
RoomMessage / collaboration intent
          ↕
A2A Message / Task
```

Target trước tiên là **July-managed agent**.

Không external agent requirement.

---

## Phase 7 — ACP recipient delivery

A2A target logical agent:

```text
pay
```

được resolve thành:

```text
pay AgentSession
      ↓
ACP
      ↓
Claude/Codex
```

A2A không thay ACP.

---

## Phase 8 — Shared response normalization

Agent response intended for shared collaboration:

```text
runtime output
     ↓
RoomMessage
```

Không dump raw ACP transcript.

---

## Phase 9 — Structured Work integration

Chỉ interaction có work semantics mới tạo:

```text
Work
+
A2A Task binding
```

Simple conversation không tạo Work.

Implementation (Phase 9):

- `WorkItem.scope` is one `WorkScope::Conversation` or `WorkScope::Room`; Room Work does not create a Thread.
- `send_room_message` accepts optional `work`: `{"action":"create","title":"...","goal":"..."}` or `{"action":"bind","work_id":"..."}`. Structured delegation requires `request_id` and exactly one owner target. Omit `work` for chat.
- Message, Work, task binding, and retry intent commit atomically. The authenticated sender becomes requester. Existing bindings can be referenced by their requester; only the owner can first bind previously unbound Room Work. Bound task scope and owner remain fixed.
- A2A message correlation carries `taskId` and July Work ID. Task snapshots derive status and artifacts from canonical Work/Result through existing Work APIs; ACP turn completion does not complete Work. `ready` remains nonterminal until July accepts it as `done`. Blocked Work retains its exact July status in metadata without claiming user input is required.
- This is a pre-release schema replacement: use a fresh workspace database. Old Work schema is rejected without converting or deleting data; no legacy compatibility layer is maintained.

---

## Phase 10 — Recovery

Persist và recover:

```text
Room history
cursors
runtime bindings
Work
A2A bindings
```

---

## Phase 11 — Cleanup old direction

Remove/deprecate:

```text
A2A-as-runtime assumptions
external-first DoD
obsolete docs
external-only tests
Thread-required A2A flows
```

Không xóa code reusable như mapping/normalization nếu có thể move sang communication layer.

Implementation (Phase 11): README and architecture/command documentation describe
A2A as Room communication and ACP as runtime execution. The older preparation
plan is explicitly historical. Internal A2A Message/Task mappings remain reusable;
there is no external A2A runtime to remove. The CLI test rejecting A2A as a runtime
configuration remains a boundary check. Existing Thread features are separate
from the Room flow and are not removed by this phase.

The deterministic acceptance test is
`tests/cli_repl.rs::room_a2a_complete_demo_keeps_two_agent_question_and_answer_in_shared_room`.
It runs the July CLI with ACP/MCP subprocess fixtures: both mentioned agents
publish initial replies, cashpoint asks pay, pay replies to cashpoint, and a
final untargeted publication ends the exchange. It checks shared output,
session reuse, idle-member isolation and no Conversation/Work creation.
This verifies local protocol integration, not a live Codex/Claude provider run.

---

# 32. Required tests

## Test 1 — User mentions one agent

```text
user → @cashpoint
```

Verify:

```text
RoomMessage persisted
cashpoint activated
pay not activated
cashpoint reply visible in Room
```

---

## Test 2 — User mentions multiple agents

```text
user → @cashpoint @pay
```

Verify:

```text
one RoomMessage persisted
cashpoint activated
pay activated
both replies visible
no mandatory Work creation
```

---

## Test 3 — Agent mentions another member

```text
cashpoint → @pay
```

Verify:

```text
sender membership valid
target membership valid
message persisted
A2A delivery succeeds
pay activated
pay response appears in Room
```

---

## Test 4 — Target is not Room member

```text
cashpoint → @infra
```

Verify:

```text
rejected
no DM
no cross-room delivery
no silent auto-add
```

---

## Test 5 — Message without mention

```text
user:
payment flow đang lỗi
```

Verify:

```text
message persisted
message displayed
no agent activated by default
```

---

## Test 6 — Private transcript

Verify internal ACP/tool transcript never becomes RoomMessage.

---

## Test 7 — Incremental context

Verify mentioned agent receives bounded new Room context, not entire Room history.

---

## Test 8 — Structured delegation

```text
cashpoint:
@pay implement X
```

Verify:

```text
RoomMessage
+
optional Work
+
A2A Task binding
```

---

## Test 9 — Restart

Verify:

```text
Room history intact
membership intact
AgentRoomCursor intact
unfinished Work intact
A2A binding reconciled
```

---

## Test 10 — Runtime independence

Change:

```text
pay: Codex → Claude Code
```

Room/A2A collaboration behavior không thay đổi.

---

# 33. Acceptance criteria

```text
[ ] Room is a durable shared chat
[ ] Room messages display user/agent sender identity
[ ] @mention provides deterministic routing
[ ] @mention provides deterministic activation
[ ] user can mention one Room agent
[ ] user can mention multiple Room agents
[ ] agent can mention another Room member
[ ] agent cannot message agent outside the current Room
[ ] no agent-agent DM outside Room
[ ] unrelated Room agents are not woken
[ ] RoomMessage is canonical shared conversation state
[ ] A2A is communication/delivery layer
[ ] ACP remains runtime execution layer
[ ] A2A is not modeled as an ACP replacement
[ ] private runtime transcript never leaks into Room
[ ] simple conversation does not require Work
[ ] structured collaboration can map Work ↔ A2A Task
[ ] Room collaboration survives restart
[ ] external A2A agents are not required for phase completion
```

---

# 34. Definition of Done

Demo end-to-end:

```text
Room VNA

Members:
- user
- cashpoint
- pay
```

User:

```text
@cashpoint @pay kiểm tra refund flow.
```

Expected:

```text
cashpoint activated
pay activated
both replies appear in VNA Room chat
```

Sau đó cashpoint:

```text
@pay bên cashpoint đang gửi payment_ref.
Bên pay expect field nào?
```

Expected internal flow:

```text
persist RoomMessage
validate cashpoint membership
validate pay membership
route interaction through A2A
activate/resume pay through ACP
```

Pay:

```text
@cashpoint bên tôi expect reference_id.
```

Response phải xuất hiện trong cùng Room.

Toàn flow không yêu cầu:

```text
Thread
agent-agent DM
external agent
supervisor LLM routing
private transcript sharing
```

Đây là baseline hoàn thành của A2A trong July.

---

# 35. Architecture statement cần đưa vào docs

> **July is a shared workspace where project agents collaborate like teammates inside Rooms.**
>
> **Rooms own the shared human-visible conversation.**
>
> **Mentions determine agent attention and routing.**
>
> **A2A carries agent-to-agent interactions.**
>
> **ACP runs each agent runtime.**
>
> **July bridges identity, routing, persistence, recovery and Room membership without acting as a supervisor brain.**
>
> **Agents may collaborate only with agents that share the same Room.**
>
> **Shared Room messages and explicit Results/Artifacts may cross agent boundaries; private runtime transcripts do not.**

---

# 36. Deferred

Không làm trong plan này:

```text
external A2A marketplace
public remote-agent directory
remote discovery as primary flow
remote authentication system
distributed agent mesh
cross-room agent messaging
agent-agent DM outside Room
automatic Room membership mutation
broadcast waking every Room member
generic A2A gateway product
full Thread storage migration/deletion
```

Chỉ xem xét các phần này sau khi internal Room collaboration hoạt động ổn định.