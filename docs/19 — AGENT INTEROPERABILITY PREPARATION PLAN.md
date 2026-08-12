# JULY WORKSPACE — AGENT INTEROPERABILITY PREPARATION PLAN

## 1. Mục tiêu

Chuẩn bị architecture hiện tại để tương lai có thể thêm A2A hoặc protocol khác mà không phải rewrite Task Manager.

Phase này:

> KHÔNG implement A2A.

Không cần:

- A2A SDK
- A2A Server
- A2A Client
- Agent Card
- remote discovery
- remote authentication
- A2A Task
- network transport

Mục tiêu duy nhất:

> Tạo đúng abstraction boundary ngay từ bây giờ.

---

# 2. Architecture cần đạt sau phase này

```text
                     JULY WORKSPACE

                         Room
                          │
                        Thread
                          │
                          ▼
                    Task Manager
                          │
                          ▼
                  Agent Runtime API
                          │
                   Adapter Boundary
                          │
             ┌────────────┼────────────┐
             ▼            ▼            ▼
          current       current       future
          runtime       runtime
                                      A2A
```

Task Manager tuyệt đối không phụ thuộc runtime cụ thể.

---

# 3. Khóa domain primitives

Core collaboration model của July chỉ dùng:

```text
Task
Message
Result
Artifact
```

Không thêm A2A primitives vào core.

Không tạo:

```text
A2ATask
A2AMessage
AgentCard
```

trong domain layer.

---

# 4. Khóa Task ownership model

Task cần thể hiện rõ:

```text
Task
{
    id
    room_id
    thread_id

    parent_task_id

    requester_agent_id
    owner_agent_id

    status

    dependencies

    created_at
    updated_at
}
```

Quan trọng nhất:

```text
requester_agent_id
owner_agent_id
```

Không sử dụng runtime/session làm ownership.

Không:

```text
owner_session_id
owner_process_id
owner_claude_session
```

---

# 5. Tách runtime identity khỏi Agent identity

Agent:

```text
Agent
{
    id
    project_id
    name
}
```

Runtime:

```text
AgentRuntime
{
    agent_id
    adapter
    runtime_ref
}
```

Ví dụ hiện tại:

```text
Agent:
    cashpoint

Runtime:
    adapter = acp
    runtime_ref = ...
```

Sau này có thể là:

```text
Agent:
    external-pay

Runtime:
    adapter = a2a
    runtime_ref = ...
```

Task Manager không thay đổi.

---

# 6. Tạo Agent Adapter interface

Đây là thay đổi quan trọng nhất của plan.

Task Manager không được gọi trực tiếp:

```text
Claude
Codex
ACP
CLI
process
session
```

Task Manager chỉ gọi một internal interface.

Ví dụ conceptual interface:

```text
AgentAdapter

start(...)
resume(...)
send(...)
cancel(...)
status(...)
```

Tên method cụ thể có thể thay đổi theo implementation.

Điều quan trọng là dependency direction:

```text
Task Manager
     ↓
AgentAdapter
     ↓
Runtime implementation
```

Không được:

```text
Task Manager
     ↓
ACP-specific code
```

---

# 7. Chuẩn hóa runtime response

Runtime không trả object riêng của provider trực tiếp cho Task Manager.

Normalize về một internal response.

Ví dụ:

```text
AgentExecutionResult
{
    status
    messages
    result
    artifacts
    runtime_ref
}
```

Task Manager chỉ hiểu internal model này.

---

# 8. Chuẩn hóa runtime error

Không để exception của provider leak lên domain layer.

Normalize tối thiểu:

```text
AgentUnavailable
ExecutionFailed
Timeout
Canceled
InvalidResponse
```

Sau này A2A có thể map lỗi của nó về cùng error model.

---

# 9. Tách runtime binding khỏi Task

Không nhét runtime-specific fields trực tiếp vào Task.

Không:

```text
Task
{
    claude_session_id
    codex_thread_id
    acp_session
}
```

Thay bằng:

```text
TaskRuntimeBinding
{
    task_id
    agent_id
    adapter
    runtime_ref
}
```

Điều này giúp tương lai thêm:

```text
remote_task_id
```

mà không thay đổi Task schema.

---

# 10. Message phải độc lập transport

Internal Message:

```text
Message
{
    id
    task_id

    from_agent_id
    to_agent_id

    content
    created_at
}
```

Không thêm:

```text
acp_message
claude_message
a2a_message
```

vào domain model.

Adapter chịu trách nhiệm chuyển đổi.

---

# 11. Result boundary

Khóa invariant:

> Results cross boundaries, transcripts don't.

Result cần là object riêng khỏi runtime transcript.

Ví dụ:

```text
Result
{
    task_id
    summary
    artifacts
    decisions
    evidence
}
```

Agent khác chỉ nhận những thứ được publish qua collaboration boundary.

Không tự động inject toàn bộ session transcript của agent owner.

---

# 12. Artifact model

Artifact nên có identity riêng.

Ví dụ:

```text
Artifact
{
    id
    task_id
    producer_agent_id
    type
    reference
    metadata
}
```

Không phụ thuộc artifact nằm:

- local filesystem
- database
- remote A2A agent
- future object store

Artifact reference nên đủ abstraction để thay backend sau này.

---

# 13. Agent Registry boundary

Task Manager nên resolve:

```text
agent_id
```

thông qua Agent Registry/Resolver.

Ví dụ:

```text
cashpoint
pay
gateway
```

Task Manager không tự hard-code cách chạy từng agent.

Flow:

```text
Task
 ↓
owner_agent_id
 ↓
Agent Registry
 ↓
Agent Runtime configuration
 ↓
Adapter
```

---

# 14. Dependency flow

Cross-agent collaboration phải trở thành dependency giữa tasks.

Ví dụ:

```text
Task A
owner: cashpoint

    ↓ needs

Task B
owner: pay
```

Khi Task B hoàn thành:

```text
Result B
    ↓
Task Manager
    ↓
dependency resolved
    ↓
Task A resumes
```

Điều này cần hoạt động hoàn toàn không cần A2A.

Sau này Task B có chạy qua A2A hay không cũng không ảnh hưởng flow này.

---

# 15. Conversation vs Task

Không tạo communication subsystem riêng.

Message thuộc collaboration context/task.

Ví dụ:

```text
Task #123
│
├── Message cashpoint → pay
├── Message pay → cashpoint
│
└── Result
```

Task Manager tiếp tục là communication backbone.

---

# 16. Persistence

Persistence layer cần lưu đủ:

```text
Task
Message
Result
Artifact
TaskRuntimeBinding
```

Sau restart:

```text
Task Manager
    ↓
load task
    ↓
load binding
    ↓
resolve adapter
    ↓
resume runtime
```

Task không được phụ thuộc process memory.

---

# 17. Add architectural extension point

Document architecture phải ghi rõ:

```text
Agent Adapter
    ├── current adapters
    └── future interoperability adapters
```

Có thể ghi A2A như ví dụ:

```text
future:
    A2AAdapter
```

Nhưng không implement.

---

# 18. Thêm architecture invariant tests

Nếu project hiện tại có architecture/unit tests, thêm guardrails để tránh regression.

Ví dụ cần detect:

- Task Manager import trực tiếp ACP implementation.
- Task chứa provider-specific session fields.
- Agent identity phụ thuộc runtime identity.
- transcript được publish như Result.
- cross-agent task bypass Task Manager.

Không nhất thiết phải viết static analyzer phức tạp.

Có thể bắt đầu bằng unit/integration tests.

---

# 19. Test scenario bắt buộc

## Scenario 1 — Agent A giao việc Agent B

```text
cashpoint
    ↓
Task Manager
    ↓
child task owner=pay
    ↓
pay runtime
    ↓
Result
    ↓
cashpoint task resumes
```

Không agent nào gọi runtime của agent còn lại trực tiếp.

---

## Scenario 2 — runtime replacement

Cùng một Agent:

```text
pay
```

đổi runtime adapter A → adapter B.

Task Manager không cần sửa logic.

---

## Scenario 3 — restart

Trong lúc child task đang working:

```text
July stops
   ↓
restart
   ↓
load TaskRuntimeBinding
   ↓
resume/reconcile runtime
```

Không mất ownership/dependency.

---

## Scenario 4 — transcript isolation

Agent B làm task bằng nhiều internal messages/tool calls.

Agent A cuối cùng chỉ nhận:

```text
explicit shared messages
+
Result
+
Artifacts
```

Không nhận private transcript.

---

# 20. Không làm trong phase này

Không:

- implement A2A
- research sâu A2A transport
- expose July thành A2A server
- remote agent discovery
- authentication system cho external agents
- protocol negotiation
- distributed orchestration
- message broker
- agent mesh

Nếu một thay đổi chỉ phục vụ A2A mà không cải thiện abstraction hiện tại:

> defer.

---

# 21. Deliverables

Phase này cần tạo/hoàn thiện:

1. Agent Adapter abstraction.
2. Runtime-independent Agent identity.
3. TaskRuntimeBinding.
4. Internal Message model.
5. Internal Result model.
6. Internal Artifact model.
7. Runtime response normalization.
8. Runtime error normalization.
9. Agent Registry/Resolver boundary.
10. Cross-agent task dependency flow.
11. Transcript isolation rules.
12. Architecture documentation.
13. Tests cho adapter independence.

---

# 22. Definition of Done

Plan được coi là hoàn thành khi:

1. Task Manager không biết agent đang chạy bằng Claude, Codex, ACP hay runtime nào khác.
2. Agent ownership dùng `agent_id`, không dùng session/process.
3. Runtime-specific identifiers nằm ngoài Task.
4. Cross-agent work đi qua Task Manager.
5. Cross-agent dependency hoạt động.
6. Message/Result/Artifact có internal representation.
7. Runtime errors được normalize.
8. Runtime có thể thay adapter mà Task Manager không đổi.
9. Private transcript không vượt agent boundary.
10. Không có A2A runtime dependency nào được thêm.
11. Architecture có extension point rõ ràng cho future A2A adapter.

---

# 23. Architectural note cần thêm ngay

> External agent interoperability is intentionally deferred.
>
> July's Task Manager remains the canonical owner of collaboration state.
>
> Agent execution must pass through a runtime adapter boundary so protocols such as A2A can be introduced later without changing the Task Manager, task ownership model, dependency model, or result propagation semantics.
>
> Results and explicitly shared messages may cross agent boundaries; private runtime transcripts do not.

---

# 24. Kết quả mong muốn

Trước phase:

```text
Task Manager
    ↓
runtime-specific implementation
```

Sau phase:

```text
Task Manager
    ↓
Agent Adapter
    ↓
runtime
```

Và phase tiếp theo chỉ cần:

```text
Agent Adapter
    ↓
A2A Adapter   ← add
```

thay vì:

```text
rewrite Task Manager
rewrite Task
rewrite agent ownership
rewrite dependencies
rewrite messaging
```

Đó là mục tiêu quan trọng nhất của phase chuẩn bị này.