# July — Work-Centric UX Simplification Plan

## 1. Mục tiêu

Chuyển mental model user-facing từ:

```text
Agent
Room
Thread
Work
DM
Conversation
Session
```

sang:

```text
Agent = ai làm
Room  = nhóm / khu vực
Work  = việc đang làm
```

`Conversation`, `Thread`, `Session` trở thành implementation detail.

Mục tiêu UX:

```text
> @cashpoint fix callback retry
```

→ mở/resume direct work với `cashpoint`

và:

```text
[vna] > @cashpoint @pay implement refund flow
```

→ tạo một Work collaboration mới trong Room `vna`

→ auto-enter Work đó

→ user gõ tiếp bằng natural language.

---

## 2. Quyết định kiến trúc

### 2.1 Không xóa Thread ngay

Phase này:

```text
User model:
Room → Work
```

nhưng internal tạm thời vẫn có thể là:

```text
Room
 └── Thread
      └── Work
```

July tự tạo/quản lý Thread bên dưới.

User không cần biết Thread tồn tại.

### 2.2 Chỉ merge domain sau khi quan sát

Sau khi UX mới chạy ổn, audit:

```text
Work ↔ Thread
```

Nếu gần như 1:1 thì mới cân nhắc merge Thread vào Work ở domain/storage.

Không làm migration lớn trong phase này.

---

## 3. Target UX

### Single agent

```text
> @cashpoint fix callback retry
```

July:

```text
Opening cashpoint...

[cashpoint] >
```

Prompt tiếp theo:

```text
> also add regression tests
```

đi tiếp vào cùng context.

### Multi-agent trong Room

```text
[vna] > @cashpoint @pay implement refund flow
```

July:

```text
Created work: Refund flow
Agents: cashpoint, pay

[vna/refund-flow] >
```

Prompt tiếp theo:

```text
> support partial refund too
```

đi vào Work hiện tại.

### Plain prompt trong Work

```text
[vna/refund-flow] > check retry behavior
```

→ gửi vào Work hiện tại.

### Plain prompt trong Room

```text
[vna] > investigate refund issue
```

Không báo lỗi kỹ thuật.

Hiển thị picker:

```text
Who should work on this?

› cashpoint
  pay
  cashpoint + pay
```

Sau khi chọn, July tạo đúng context và dispatch prompt.

---

## 4. Input routing rules

Dispatcher chỉ cần 4 rule deterministic.

### Rule A — current Work + no mention

```text
current Work
+ plain text
→ continue Work
```

### Rule B — one `@agent`

```text
@cashpoint ...
→ resolve agent
→ create/resume direct context
→ auto-enter
→ send prompt
```

### Rule C — multiple `@agent`

```text
@cashpoint @pay ...
→ create/resume collaboration Work
→ ensure participants
→ create internal Thread if current architecture requires it
→ auto-enter
→ send prompt
```

### Rule D — no usable context + no target

```text
plain prompt
→ show target picker
→ dispatch after selection
```

Không dùng LLM để quyết định routing.

---

## 5. Bỏ Thread khỏi REPL UX

Deprecate/hide:

```text
/thread
/thread new
```

Không cần xóa handler ngay.

Có thể giữ chúng ở:

```text
administrative CLI
debug mode
compatibility layer
```

nhưng `/help` và command palette mặc định không hiển thị nữa.

---

## 6. Work trở thành navigation concept chính

`/work` từ inspection command nâng thành entry point chính.

Target:

```text
/work
```

hiện:

```text
Works

› Refund flow        active
  Callback retry     blocked
  Voucher pending    done
```

Enter vào Work:

```text
[vna/refund-flow] >
```

Không cần:

```text
/thread refund-flow
```

### Optional subcommands

Chỉ nếu cần:

```text
/work new
/work open <id>
```

Nhưng common path vẫn là natural language + `@agent`.

Không biến `/work` thành CRUD-heavy command.

---

## 7. Room semantics mới

Room chỉ là:

```text
team
namespace
workstream
member pool
```

Room không phải conversation.

Target:

```text
Room VNA
├── members
├── active Works
├── completed Works
└── activity
```

Room input là **work launcher**.

Không persist Room-level chat transcript.

---

## 8. Direct work / DM simplification

User-facing có thể vẫn hiện:

```text
[cashpoint] >
```

nhưng không nhất thiết phải expose khái niệm `DM`.

Internally:

```text
DirectContext
or Conversation
```

vẫn giữ nếu đang cần.

Về sau `/dm` có thể deprecate vì:

```text
@cashpoint
```

đã đủ.

Phase này chưa cần xóa `/dm`.

---

## 9. `@` completion

Reuse completion infrastructure:

```text
/
→ command discovery

@
→ agent discovery
```

Example:

```text
> @ca
```

render:

```text
cashpoint
```

Multiple mentions được hỗ trợ:

```text
@cashpoint @pay ...
```

---

## 10. Auto-enter semantics

Bất kỳ action nào tạo usable work context phải auto-enter.

Ví dụ:

```text
@cashpoint ...
```

→ enter direct context.

```text
@cashpoint @pay ...
```

→ enter new/existing Work.

Không bao giờ:

```text
create context
→ remain in Room
→ next prompt errors
```

---

## 11. Internal mapping

Để giảm risk, dùng adapter/mapping layer:

```text
User Work Context
        ↓
WorkContextResolver
        ↓
existing Work
+ existing Thread/Conversation
```

Không rewrite domain ngay.

Conceptual:

```rust
struct ActiveWorkContext {
    room_id: Option<RoomId>,
    work_id: WorkId,
    conversation_id: ConversationId,
    thread_id: Option<ThreadId>,
    participants: Vec<AgentId>,
}
```

Tên struct chỉ minh họa; không cần tạo abstraction mới nếu code hiện tại đã có equivalent.

---

## 12. Work creation

Khi multiple agents được target:

```text
@cashpoint @pay implement refund flow
```

July nên:

```text
1. resolve agents
2. resolve current Room if available
3. create Work
4. create internal Thread if required
5. attach participants
6. persist initial user prompt
7. choose initial agent/owner using existing collaboration semantics
8. enter Work
9. dispatch
```

Không broadcast full prompt blindly tới tất cả agents nếu collaboration layer hiện tại có owner/request semantics.

---

## 13. Existing Room members

Multiple targeted agents trong Room:

```text
[vna] > @cashpoint @pay ...
```

nên require các agent là Room members.

Nếu một agent chưa là member:

```text
pay is not a member of VNA.

Add pay and continue?
› Yes
  No
```

Không silently mutate membership trừ khi bạn chủ động muốn policy auto-add.

---

## 14. Slash command surface sau simplification

Target user-facing commands:

```text
Navigation / inspection
  /room
  /work
  /agents
  /rooms
  /members
  /back

Control
  /publish
  /restart

System
  /help
  /quit
```

Có thể vẫn giữ `/dm` trong transitional period.

Ẩn:

```text
/thread
/thread new
```

---

## 15. Help rewrite

`/help` không nên dạy user workflow kiểu:

```text
1. enter room
2. create thread
3. enter thread
4. type prompt
```

Thay bằng:

```text
Work with one agent:
  @cashpoint fix callback retry

Work with multiple agents:
  @cashpoint @pay implement refund flow

Commands:
  /room
  /work
  /agents
  /back
```

Goal:

> user chỉ cần nhớ `@` và `/`.

---

## 16. Implementation sequence

### Step 1 — Audit current Thread dependencies

Inventory:

```text
Thread creation
Thread navigation
Work ↔ Thread relation
conversation lookup
session lookup
room membership
current-context state
```

Không thay schema.

### Step 2 — Add target parsing

Parse:

```text
@agent
@agent1 @agent2
```

Không dùng LLM.

### Step 3 — Add unified input dispatcher

Implement 4 routing rules.

### Step 4 — Add auto-context creation

One agent:

```text
direct context
```

Multiple agents:

```text
Work + internal Thread if required
```

### Step 5 — Auto-enter

Context creation/switch phải cập nhật active context ngay.

### Step 6 — Upgrade `/work`

Cho phép list/select/open Work.

### Step 7 — Hide Thread UX

Remove `/thread` khỏi normal help/palette.

Không xóa underlying domain/storage.

### Step 8 — Add Room target picker

Plain prompt ở Room → participant selector.

### Step 9 — Update help/discovery

Add `@` discovery.

### Step 10 — Regression tests

Verify old collaboration/runtime behavior vẫn hoạt động.

---

## 17. Tests

### Single-agent routing

```text
@cashpoint prompt
```

Verify:

```text
agent resolved
direct context created/resumed
context becomes active
prompt delivered once
```

### Multi-agent routing

```text
@cashpoint @pay prompt
```

Verify:

```text
Work created/resumed
participants correct
internal Thread created if required
active context switched
collaboration starts once
```

### Current Work

Plain prompt:

```text
> continue
```

Verify it remains in same Work.

### Room prompt

Plain text from Room:

```text
> investigate refund
```

Verify picker opens instead of command/context error.

### Auto-enter

After new Work:

```text
next prompt
```

must succeed without `/thread`.

### Isolation

Verify:

```text
Work A transcript
does not leak into
Work B
```

### Compatibility

Existing:

```text
Work
Result
Decision
Dependency
collaboration protocol
session recovery
```

must remain unchanged.

---

## 18. Acceptance criteria

```text
[ ] user can start single-agent work with @agent prompt
[ ] user can start multi-agent work with multiple @mentions
[ ] new work contexts auto-enter
[ ] user never needs /thread before sending a prompt
[ ] Room is not treated as shared conversation context
[ ] plain Room prompt produces target picker instead of error
[ ] /work lists and opens work contexts
[ ] Thread is hidden from default help/palette
[ ] Thread domain/storage is not removed in this phase
[ ] @ completion uses configured agents
[ ] routing is deterministic
[ ] no supervisor LLM is introduced
[ ] results/context isolation invariants remain intact
```

---

## 19. Deferred

Không làm trong phase này:

```text
delete Thread table
schema migration
merge Work + Thread domain models
remove /dm entirely
semantic LLM routing
automatic Room inference using LLM
A2A changes
large collaboration rewrite
```

Sau khi UX mới chạy ổn, mới audit:

```text
Is Thread still independently useful?
```

Nếu không:

```text
Thread → merge into Work
```

ở một refactor riêng.

---

## 20. Recommended scope

Vì token AI còn hạn chế, nên làm MVP:

```text
1. @agent parser
2. multiple @agents
3. unified dispatcher
4. auto-create/resume context
5. auto-enter
6. /work navigation
7. hide /thread from normal UX
8. update /help
9. basic tests
```

Hoãn toàn bộ domain migration.

Đây là cách lấy phần lớn UX improvement với ít thay đổi kiến trúc nhất.
