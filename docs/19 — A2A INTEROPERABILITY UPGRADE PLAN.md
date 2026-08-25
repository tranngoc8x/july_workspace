# JULY WORKSPACE — A2A INTEROPERABILITY UPGRADE PLAN

## 1. Mục tiêu

Bổ sung khả năng để July Workspace kết nối và cộng tác với các agent bên ngoài thông qua A2A protocol.

A2A chỉ là một interoperability adapter của July.

Nguyên tắc kiến trúc:

> Task Manager owns collaboration state.
> A2A transports collaboration across workspace boundaries.

July Task Manager tiếp tục là source of truth.

A2A Task không thay thế July Task.

---

# 2. Kiến trúc mục tiêu

```text
                     JULY WORKSPACE

                         Room
                          │
                        Thread
                          │
                          ▼
                    Task Manager
                          │
                 Collaboration Layer
                          │
              ┌───────────┼───────────┐
              │           │           │
             ACP         SDK         A2A
              │           │           │
              ▼           ▼           ▼
        Internal Agent Internal   External Agent
```

A2A chỉ xuất hiện tại Agent Runtime / Adapter boundary.

Không đưa A2A concepts vào domain core nếu không cần thiết.

---

# 3. Nguyên tắc bắt buộc

## 3.1 July Task là canonical

Ví dụ:

```text
JulyTask
{
    id: "task-123"
    owner_agent_id: "pay"
    requester_agent_id: "cashpoint"
    status: "working"
}
```

Nếu chạy qua A2A:

```text
RuntimeBinding
{
    task_id: "task-123"
    adapter: "a2a"
    remote_task_id: "remote-task-892"
}
```

`remote_task_id` không được trở thành primary identity của task trong July.

---

## 3.2 Không leak A2A vào Task Manager

Task Manager không được chứa logic như:

```text
if protocol == A2A ...
```

Thay vào đó:

```text
Task Manager
    ↓
Agent Adapter interface
    ↓
A2A Adapter
```

Tất cả protocol-specific logic nằm trong adapter.

---

## 3.3 Results cross boundaries, transcripts don't

External agent có thể trả:

- Message
- Result
- Artifact
- Status

Nhưng private runtime transcript của external agent không trở thành context mặc định của agent khác.

---

## 3.4 Agent runtime independence

Agent gọi agent khác không cần biết:

- remote URL
- protocol version
- authentication method
- model
- framework
- session ID
- provider

Agent chỉ cần biết:

```text
target_agent = "pay"
```

July chịu trách nhiệm resolution.

---

# 4. Workstream A — A2A Agent Adapter

Implement một adapter mới:

```text
AgentAdapter
    ├── ACPAdapter
    ├── SDKAdapter
    └── A2AAdapter
```

A2AAdapter chịu trách nhiệm:

- connect remote agent
- send messages
- create/continue remote tasks
- receive status
- receive artifacts
- normalize errors
- map remote task state về July state

Task Manager không trực tiếp gọi A2A SDK/API.

---

# 5. Workstream B — Agent Card / Discovery

Bổ sung metadata cho external agents.

Ví dụ logical model:

```text
AgentDefinition
{
    id
    name
    project
    adapter
    capabilities
    connection
}
```

External A2A agent:

```text
agent:
  id: external-pay
  adapter: a2a

connection:
  endpoint: ...
```

Agent Card của A2A được adapter đọc và normalize thành internal `AgentDefinition`.

Task Manager chỉ nhìn thấy internal representation.

---

# 6. Workstream C — Task Mapping

Implement mapping:

```text
July Task
    ↕
A2A Task
```

Mapping cần lưu:

```text
task_id
adapter
remote_agent_id
remote_task_id
remote_context_id
created_at
updated_at
```

Không đưa các field này trực tiếp vào core Task nếu có thể tránh.

Sử dụng runtime binding / external binding riêng.

---

# 7. Workstream D — Message Mapping

Mapping:

```text
July Message
      ↕
A2A Message
```

Internal message cần giữ semantics độc lập protocol:

```text
Message
{
    id
    task_id
    from_agent
    to_agent
    content
    created_at
}
```

A2AAdapter chịu trách nhiệm serialization/deserialization.

---

# 8. Workstream E — Result & Artifact Mapping

Mapping:

```text
A2A Artifact
      ↓
July Artifact
```

và:

```text
A2A task completion
      ↓
July Result
```

Result được publish về parent task/thread.

Ví dụ:

```text
cashpoint
    │
 Task #123
    │
    ▼
Task Manager
    │
 A2A Adapter
    │
    ▼
External pay agent
    │
 Artifact
    ▼
A2A Adapter
    │
 Result
    ▼
Task #123
    │
    ▼
cashpoint
```

---

# 9. Workstream F — Lifecycle Mapping

Xây mapping rõ ràng giữa remote state và July state.

Ví dụ:

```text
remote submitted    → pending
remote working      → working
remote completed    → completed
remote failed       → failed
remote canceled     → canceled
```

Không expose trực tiếp protocol-specific states vào toàn bộ July.

Unknown state phải được xử lý an toàn.

---

# 10. Workstream G — Error Handling

Các lỗi remote cần normalize:

```text
AgentUnavailable
AuthenticationFailed
RemoteTaskFailed
ProtocolError
Timeout
UnsupportedCapability
```

Task Manager nhận domain error.

Không nhận raw transport exception.

---

# 11. Workstream H — Persistence & Recovery

July cần có khả năng restart mà không mất mapping:

```text
July Task
↔
Remote A2A Task
```

Sau restart:

1. load task
2. load runtime binding
3. reconnect adapter
4. query/resume remote task nếu hỗ trợ
5. reconcile trạng thái
6. tiếp tục workflow

---

# 12. Workstream I — Capability Validation

Trước khi dispatch:

```text
Task Manager
    ↓
Agent Registry
    ↓
capabilities
```

Nếu agent không hỗ trợ capability cần thiết:

```text
UnsupportedCapability
```

Không dispatch rồi mới phát hiện nếu metadata đã đủ để quyết định trước.

---

# 13. Workstream J — Security Boundary

External agent phải được xem là trust boundary khác.

Không gửi mặc định:

- full thread transcript
- private agent transcript
- toàn bộ project context
- credentials
- unrelated artifacts

Chỉ gửi context cần thiết cho task.

Principle:

> Minimum required collaboration context.

---

# 14. Testing

## Unit tests

- Task ↔ A2A Task mapping
- Message mapping
- Artifact mapping
- status mapping
- error normalization
- capability parsing

## Integration tests

```text
July
 ↓
A2A Adapter
 ↓
Mock A2A Agent
```

Test:

- create task
- message exchange
- completed task
- failed task
- artifact returned
- timeout
- reconnect

## Cross-agent test

```text
Internal Agent A
      ↓
July Task Manager
      ↓
A2A
      ↓
External Agent B
      ↓
Result
      ↓
Agent A
```

---

# 15. Không làm trong phase này

Không biến July thành:

- generic A2A server framework
- A2A gateway product
- distributed agent mesh
- service discovery platform
- protocol proxy

Không support tất cả A2A features chỉ vì protocol có chúng.

Chỉ implement những gì July collaboration model thực sự cần.

---

# 16. Definition of Done

Phase hoàn thành khi:

1. Một external A2A agent có thể đăng ký vào July.
2. July có thể dispatch một Task tới agent đó.
3. Message có thể đi qua adapter.
4. Remote task status được map về July.
5. Artifact/result được đưa trở lại July Thread.
6. Restart July không làm mất remote task binding.
7. Agent nội bộ không cần biết task được xử lý qua A2A.
8. Task Manager không chứa A2A-specific business logic.
9. Transcript isolation vẫn được giữ.
10. Existing ACP/SDK/internal agents hoạt động như trước.

---

# 17. Architecture invariant

Sau khi hoàn thành phase:

```text
Task Manager
     │
     ▼
Agent Adapter
     │
     ├── Internal
     ├── ACP
     ├── SDK
     └── A2A
```

A2A là một khả năng mới của July.

Nó không trở thành nền móng mới của July.
