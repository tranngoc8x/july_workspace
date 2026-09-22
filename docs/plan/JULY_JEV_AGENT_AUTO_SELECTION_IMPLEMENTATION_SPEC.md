# July + JEV Agent Auto-Selection — Implementation Spec

## 1. Mục tiêu

Tích hợp JEV vào July để hỗ trợ **tự động lựa chọn agent phù hợp khi user không mention agent cụ thể**.

Nguyên tắc routing:

```text
explicit agent > explicit auto mode > automatic decision
```

Behavior mong muốn:

```text
@backend fix Redis timeout
→ route trực tiếp tới @backend
→ không gọi JEV

@auto fix Redis timeout
→ dùng JEV để chọn agent

fix Redis timeout
→ giai đoạn đầu giữ behavior cũ
→ sau khi eval đủ tốt mới bật auto-routing mặc định
```

Mục tiêu kiến trúc:

- Không phá explicit routing hiện tại.
- JEV chỉ là một implementation của `DecisionEngine`.
- July giữ quyền quyết định cuối cùng thông qua `DecisionPolicy`.
- Không để JEV trực tiếp assign agent.
- Những gì có thể xác định bằng code thì xử lý deterministic trước.
- Có fallback rõ ràng khi JEV unavailable hoặc confidence thấp.
- Không để coding agent tự suy diễn hoặc tự bịa JEV API.

---

## 2. JEV là gì

JEV([typesafe.ai](https://typesafe.ai/)) là một **decision/judgment engine** dùng để hỗ trợ các quyết định có cấu trúc trên một tập lựa chọn hữu hạn.

Trong project này, JEV:

- KHÔNG phải coding agent.
- KHÔNG thay thế Claude/Codex reasoning.
- KHÔNG trực tiếp thực thi Work.
- KHÔNG sở hữu routing policy của July.
- KHÔNG được quyền tự assign agent mà không đi qua July policy.

JEV được dùng để thực hiện các judgment kiểu:

```text
Which candidate is most suitable?

How should candidates be ranked?

Does this candidate satisfy a fuzzy criterion?

Is the decision stable enough to trust?
```

Các operation/tool JEV hiện có liên quan tới UC này:

```text
filter_items
rank_items
choose_one
stability_check
```

Vai trò đề xuất:

```text
filter_items
→ optional fuzzy pre-filtering

rank_items
→ xếp hạng agent candidates

choose_one
→ chọn candidate cuối cùng

stability_check
→ kiểm tra decision có ổn định không
```

UC1 có thể bắt đầu chỉ với:

```text
rank_items
choose_one
```

Sau khi ổn định mới thêm:

```text
stability_check
```

---

## 3. JEV nằm ở đâu trong kiến trúc July

JEV phải nằm sau application abstraction.

```text
                    User
                     │
              "fix redis issue"
                     │
                     ▼
                Message Router
                     │
          ┌──────────┴───────────┐
          │                      │
       @mention               no mention
          │                      │
          ▼                      ▼
   Explicit Routing       CandidateResolver
                                 │
                                 ▼
                           DecisionEngine
                                 │
                                 ▼
                         JevDecisionEngine
                                 │
                                 ▼
                               JEV
                                 │
                                 ▼
                           DecisionPolicy
                                 │
                  ┌──────────────┼─────────────┐
                  ▼              ▼             ▼
               assign         suggest       unresolved
                  │
                  └──────────────┬─────────────┘
                                 ▼
                              Work
                                 │
                                 ▼
                              Agent
```

Quan trọng:

```text
JEV judges.
July decides.
```

Không được implement:

```text
JEV
 ↓
direct assign agent
```

Phải là:

```text
JEV
 ↓
structured decision
 ↓
DecisionPolicy
 ↓
July action
```

---

## 4. Coding-time tool khác với July runtime integration

Coding agent cần phân biệt hai khái niệm sau.

### Coding-time

Claude/Codex có thể đang nhìn thấy JEV dưới dạng MCP/tool:

```text
Claude / Codex
    ↓
JEV MCP
    ↓
rank_items / choose_one / ...
```

Đây chỉ là tool có sẵn trong development environment.

### July runtime

July binary cần có đường kết nối thực tế tới JEV:

```text
July
 ↓
JevDecisionEngine
 ↓
JEV runtime integration
 ↓
decision
```

Hai thứ này KHÔNG mặc định là một.

Coding agent tuyệt đối không được giả định rằng:

```text
MCP tool hiện diện trong coding session
=
July runtime có thể gọi tool đó trực tiếp
```

---

## 5. Runtime integration boundary

Trước khi implement `JevDecisionEngine`, coding agent phải xác định JEV runtime hiện có theo một trong các dạng:

```text
A. HTTP/API service

B. local process / CLI

C. MCP server accessible from July runtime

D. local SDK/library

E. adapter/service nội bộ khác
```

### Bắt buộc

Coding agent phải:

1. Inspect project/config/environment hiện tại.
2. Xác định interface JEV thực tế.
3. Dùng interface thật.
4. Không tự bịa endpoint/schema/protocol.

Nếu chưa tìm thấy runtime integration thật:

```text
Implement:
- DecisionEngine trait
- MockDecisionEngine
- JevDecisionEngine interface boundary

Do NOT invent:
- fake REST endpoint
- fake MCP JSON schema
- fake SDK method
```

Có thể để adapter chưa hoàn thiện bằng explicit error:

```rust
return Err(DecisionError::ProviderNotConfigured);
```

thay vì tự chế protocol.

---

## 6. JEV Contract cho Agent Selection

### Input logic

JEV chỉ nên nhận dữ liệu cần thiết cho routing:

```text
task description

candidate agents

skills

domains

languages

tools

tags

optional lightweight workload metadata
```

### Không gửi

Không gửi sang JEV:

```text
full conversation transcript

ACP session logs

full Room history

private chain-of-thought

raw terminal logs

unrelated artifacts

secrets

API keys

credentials
```

---

## 7. Agent Capabilities

Trước khi JEV có thể chọn agent, July cần metadata rõ ràng về năng lực agent.

Đề xuất:

```rust
pub struct AgentCapabilities {
    pub skills: Vec<String>,
    pub domains: Vec<String>,
    pub languages: Vec<String>,
    pub tools: Vec<String>,
    pub tags: Vec<String>,
}
```

Ví dụ:

```yaml
agent: backend

capabilities:
  skills:
    - api-development
    - debugging
    - database

  domains:
    - backend

  languages:
    - rust
    - typescript

  tools:
    - postgres
    - redis
```

Không nên để JEV tự suy luận capability từ tên:

```text
backend_agent
payment_agent
infra_agent
```

Metadata phải explicit.

---

## 8. AgentCandidate

Tạo representation riêng phục vụ routing.

```rust
pub struct AgentCandidate {
    pub agent_id: AgentId,
    pub capabilities: AgentCapabilities,
    pub active_work_count: usize,
}
```

Có thể mở rộng sau:

```rust
pub struct AgentCandidate {
    pub agent_id: AgentId,
    pub capabilities: AgentCapabilities,
    pub active_work_count: usize,
    pub runtime_available: bool,
    pub enabled: bool,
}
```

---

## 9. CandidateResolver

Không gửi toàn bộ agents sang JEV.

Trước hết lọc bằng deterministic rules:

```text
all room agents
      ↓
enabled?
      ↓
runtime available?
      ↓
allowed in current room?
      ↓
not suspended?
      ↓
candidate list
```

Ví dụ:

```text
20 agents
 ↓ enabled
15
 ↓ runtime available
11
 ↓ room permission
7 candidates
```

Chỉ candidate cuối mới sang JEV.

Đề xuất:

```rust
pub trait CandidateResolver {
    async fn resolve_agents(
        &self,
        context: &RoutingContext,
    ) -> Result<Vec<AgentCandidate>>;
}
```

Nguyên tắc:

```text
Deterministic before probabilistic.
```

Nếu code biết chắc thì không hỏi JEV.

---

## 10. DecisionEngine

Không gọi JEV trực tiếp từ:

```text
message handler

TUI

CLI

WorkService

RoomService
```

Tạo abstraction:

```rust
#[async_trait]
pub trait DecisionEngine {
    async fn choose_agent(
        &self,
        request: AgentSelectionRequest,
    ) -> Result<AgentSelectionDecision>;
}
```

Request:

```rust
pub struct AgentSelectionRequest {
    pub task: String,
    pub candidates: Vec<AgentCandidate>,
}
```

Response:

```rust
pub struct AgentSelectionDecision {
    pub selected: Option<AgentId>,
    pub confidence: f32,
    pub candidates: Vec<CandidateScore>,
}
```

Candidate score:

```rust
pub struct CandidateScore {
    pub agent_id: AgentId,
    pub score: f32,
}
```

Future implementations:

```text
DecisionEngine
├── JevDecisionEngine
├── RuleDecisionEngine
├── MockDecisionEngine
└── LlmDecisionEngine
```

Application layer chỉ biết `DecisionEngine`.

---

## 11. JEV Adapter structure

Đề xuất:

```text
src/
  application/
    decision/
      mod.rs
      engine.rs
      policy.rs
      types.rs

  adapter/
    jev/
      mod.rs
      client.rs
      mapper.rs
      engine.rs
```

Responsibilities:

```text
client.rs
→ giao tiếp với JEV runtime thật

mapper.rs
→ map July types ↔ JEV types

engine.rs
→ implement DecisionEngine
```

Không để JEV-specific schema leak ra application layer.

---

## 12. State gửi sang JEV

Conceptual example:

```json
{
  "task": {
    "text": "Implement Redis caching for payment API"
  },
  "agents": [
    {
      "id": "backend",
      "skills": ["api-development", "database"],
      "domains": ["backend"],
      "languages": ["rust"],
      "tools": ["redis"],
      "active_work": 2
    },
    {
      "id": "infra",
      "skills": ["redis", "docker"],
      "domains": ["infrastructure"],
      "languages": ["rust"],
      "tools": ["redis", "docker"],
      "active_work": 1
    }
  ]
}
```

Đây là conceptual schema.

Coding agent phải map sang **JEV runtime contract thật** sau khi inspect implementation/API hiện tại.

Không được xem JSON trên là wire-format chính thức nếu chưa xác minh.

---

## 13. Judgment strategy

Không nên chỉ hỏi:

```text
Which agent is best?
```

Có hai hướng.

### MVP

Dùng:

```text
rank_items
→ choose_one
```

### Advanced

Tách signal:

```text
task_match
domain_match
tool_match
```

Trong đó:

```text
semantic task match      → JEV
domain match             → JEV
tool suitability         → JEV hoặc deterministic

runtime availability     → CODE
permission               → CODE
room membership          → CODE
enabled status           → CODE
workload count           → CODE
```

---

## 14. Conceptual JEV flow

Pseudo flow:

```text
AgentSelectionRequest
        ↓
rank_items(candidates, task suitability)
        ↓
ranked candidates
        ↓
choose_one(top candidates)
        ↓
selected candidate
        ↓
optional stability_check
        ↓
AgentSelectionDecision
```

Coding agent phải dùng actual JEV operation signatures.

Không copy pseudo call thành production code nếu runtime API khác.

---

## 15. DecisionPolicy

JEV không được assign agent trực tiếp.

Đề xuất:

```rust
pub struct AgentSelectionPolicy {
    pub auto_assign_threshold: f32,
    pub suggest_threshold: f32,
}
```

Routing output:

```rust
pub enum RoutingDecision {
    Explicit(AgentId),

    AutoSelected {
        agent: AgentId,
        confidence: f32,
    },

    Suggested {
        agent: AgentId,
        confidence: f32,
    },

    Unresolved,
}
```

Baseline để test:

```text
confidence >= 0.85
→ auto assign

0.65 <= confidence < 0.85
→ suggest

confidence < 0.65
→ unresolved
```

Các threshold này KHÔNG phải final truth.

Cần calibrate bằng eval thực tế.

---

## 16. Score composition

Nếu dùng multiple signals:

```rust
score =
    semantic_match * 0.50
  + domain_match   * 0.25
  + availability   * 0.15
  + workload       * 0.10;
```

Config:

```toml
[decision.agent_selection]
semantic_weight = 0.50
domain_weight = 0.25
availability_weight = 0.15
workload_weight = 0.10
```

Không để JEV sở hữu business policy.

---

## 17. Router integration

Flow:

```rust
match explicit_mention {
    Some(agent) => {
        route_to(agent).await
    }

    None => {
        auto_route(request).await
    }
}
```

`auto_route()`:

```text
Room Agents
    ↓
CandidateResolver
    ↓
DecisionEngine
    ↓
DecisionPolicy
    ↓
RoutingDecision
```

Không implement:

```text
message parser
 ↓
JEV
 ↓
ACP runtime
```

---

## 18. UX rollout

Không bật automatic routing ngay.

### Stage 1 — Existing explicit routing

```text
@backend fix Redis timeout
```

Result:

```text
→ backend
```

JEV không chạy.

---

### Stage 2 — `/route`

Add:

```text
/route "implement Redis caching"
```

Output ví dụ:

```text
Recommended: @infra

Candidates:
@infra    0.91
@backend  0.72
@payment  0.31
```

Mục đích:

```text
debug
benchmark
collect eval cases
verify candidate metadata
```

---

### Stage 3 — `@auto`

```text
@auto implement Redis caching
```

Flow:

```text
@auto
 ↓
CandidateResolver
 ↓
DecisionEngine
 ↓
DecisionPolicy
 ↓
Agent
```

---

### Stage 4 — Default automatic mode

Sau khi eval đủ tốt:

```text
fix Redis timeout
```

→

```text
no mention
 ↓
CandidateResolver
 ↓
JEV
 ↓
DecisionPolicy
 ↓
Agent
```

---

## 19. Routing modes

Config:

```toml
[decision.agent_selection]
enabled = true
mode = "suggest"
```

Supported modes:

```text
disabled
→ behavior July cũ

suggest
→ JEV đề xuất agent
→ user quyết định

automatic
→ đủ confidence thì tự assign
```

---

## 20. Audit record

Nên lưu lại routing decision.

```rust
pub struct AgentRoutingRecord {
    pub work_id: WorkId,
    pub selected_agent: AgentId,
    pub source: DecisionSource,
    pub confidence: Option<f32>,
    pub candidate_ids: Vec<AgentId>,
    pub created_at: DateTime<Utc>,
}
```

Decision source:

```rust
pub enum DecisionSource {
    Human,
    ExplicitMention,
    Jev,
    Rule,
}
```

Optional metadata:

```text
decision_engine = jev
policy_version = agent-routing-v1
```

Không cần lưu:

```text
full prompt

private reasoning

full transcript
```

---

## 21. Failure handling

JEV không được thành single point of failure.

Các failure cần xử lý:

```text
timeout

rate limit

API down

invalid response

selected unknown agent

zero candidates

runtime not configured
```

Fallback:

```text
0 candidates
→ unresolved

1 candidate
→ assign luôn

>1 candidates + JEV failure
→ explicit selection / unresolved
```

Không random select.

Config:

```toml
[decision.agent_selection]
fallback = "ask_user"
```

---

## 22. Important implementation rules for coding agents

### Rule 1 — Do not invent JEV API

Coding agent phải inspect actual integration trước.

Không được tự tạo:

```text
POST /jev/rank

POST /jev/choose

fake JSON-RPC

fake SDK methods
```

nếu project/runtime chưa định nghĩa chúng.

### Rule 2 — MCP availability is not runtime availability

Việc coding agent có tool:

```text
rank_items
choose_one
```

không có nghĩa July binary tự gọi được chúng.

### Rule 3 — Keep provider boundary clean

JEV-specific code chỉ nằm sau:

```text
DecisionEngine
```

### Rule 4 — Explicit routing must bypass JEV

```text
@agent
→ no JEV call
```

### Rule 5 — Deterministic rules first

Không hỏi JEV về:

```text
enabled?

runtime online?

agent belongs to room?

permission allowed?
```

### Rule 6 — JEV output is advisory until policy accepts it

```text
JEV result
→ DecisionPolicy
→ action
```

---

## 23. Tests

### Unit tests

```text
explicit mention bypasses JEV

disabled agent excluded

unavailable agent excluded

agent outside room excluded

one candidate bypasses JEV

low confidence does not auto assign

high confidence returns AutoSelected
```

### Mock DecisionEngine

```text
DecisionEngine returns backend 0.91
→ backend selected
```

### Adapter tests

Nếu JEV runtime có HTTP/MCP/SDK contract rõ:

```text
valid response mapping

invalid payload

timeout

unknown candidate id

empty ranking

missing confidence
```

### Regression tests

Behavior sau phải giữ nguyên:

```text
@backend <task>
```

---

## 24. PR Plan

### PR1 — Agent capabilities

Implement:

```text
AgentCapabilities

AgentCandidate

CandidateResolver

tests
```

Không có JEV.

---

### PR2 — Decision abstraction

Implement:

```text
DecisionEngine

MockDecisionEngine

DecisionPolicy

Decision types
```

Chưa cần real JEV runtime adapter.

---

### PR3 — JEV runtime adapter

Trước tiên:

```text
inspect actual JEV runtime integration
```

Sau đó implement:

```text
JevClient

JevMapper

JevDecisionEngine
```

Không invent protocol.

---

### PR4 — `/route`

Implement:

```text
/route
```

Chỉ recommendation/debug.

Không auto assign.

---

### PR5 — `@auto`

Implement:

```text
@auto <task>
```

Cho phép explicit opt-in.

---

### PR6 — default auto-routing

Chỉ sau khi eval đủ tốt:

```text
no mention
→ automatic routing
```

---

## 25. Thứ tự code đề xuất

```text
1. AgentCapabilities

2. AgentCandidate

3. CandidateResolver

4. DecisionEngine trait

5. Decision types

6. MockDecisionEngine

7. DecisionPolicy

8. Inspect actual JEV runtime

9. JevClient

10. JevDecisionEngine

11. /route

12. @auto

13. eval

14. automatic routing
```

---

## 26. Evaluation plan

Tạo dataset các task thực tế.

Ví dụ:

```text
"fix Redis timeout"

"update payment webhook verification"

"optimize PostgreSQL query"

"update TUI rendering"

"debug Docker deployment"

"add Rust integration test"
```

Với mỗi task lưu expected candidates:

```yaml
task: fix Redis timeout

acceptable:
  - infra
  - backend

bad:
  - frontend
```

Đo:

```text
top-1 acceptable accuracy

top-3 recall

unresolved rate

wrong-auto-assignment rate

JEV failure rate

average routing latency
```

Metric quan trọng nhất:

```text
wrong-auto-assignment rate
```

Không nên bật automatic mode nếu metric này chưa đủ an toàn.

---

## 27. Definition of Done

UC1 hoàn thành khi:

- Explicit routing không thay đổi.
- `@auto` có thể chọn agent.
- `/route` có thể preview decision.
- Disabled/unavailable agent không được gửi hoặc chọn.
- JEV failure không làm hỏng Work flow.
- Confidence thấp không auto assign.
- Có audit record.
- Có MockDecisionEngine.
- Có config bật/tắt routing.
- Application layer không phụ thuộc trực tiếp JEV.
- JEV adapter dùng runtime contract thật.
- Coding agent không phải đoán JEV API.
- Có eval dataset cơ bản.
- Có regression tests.

---

## 28. Future use cases

Sau UC1, `DecisionEngine` có thể reuse cho:

```text
Result selection

Deliberation → Decision

Reviewer selection

Retry / stop / change agent

Decision conflict detection

Work prioritization

Context ranking
```

Mục tiêu dài hạn:

```text
July owns orchestration and policy.

JEV provides bounded judgments.

Agents perform execution.
```
