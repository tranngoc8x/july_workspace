# Đánh giá: merge Thread vào Work ở domain/storage

Ngày: 29/08/2026 · Người thực hiện: Engineering
Bối cảnh: `docs/22-JULY-WORK-CENTRIC-UX-SIMPLIFICATION-PLAN.md` §2.2, §19 —
sau khi UX work-centric chạy, audit xem `Work ↔ Thread` có 1:1 không.

## Kết luận

**Không merge.** Quan hệ không phải 1:1 theo thiết kế, và `Conversation` là
nền chung của cả DM lẫn Thread. Merge sẽ đổi tên chứ không đơn giản hoá:
hoặc phải dựng lại một tầng sub-work bên trong `work_items`, hoặc phải bịa
work item cho DM.

Thay vào đó: đổi tên phần user-facing (đã làm phần lớn ở phase 5-6) và giữ
nguyên domain/storage.

## Dữ kiện

### Cardinality

| Chiều | Thực tế |
|---|---|
| Thread → Work | 1:N. Storage ép mỗi Thread có đúng một primary Work lúc tạo (`create_thread_with_primary_work`), các Work sau là non-primary. |
| Work → Thread | N:1, luôn có. `work_items.conversation_id` là NOT NULL. |
| Work → Conversation kiểu DM | Không có đường nào trong `src/` tạo work item cho DM. |

Nguồn Work non-primary duy nhất: `convert_decision_to_work` — một Decision
sinh ra nhiều Work trong cùng Thread. `DeliberationService` hiện **chưa được
khởi tạo ở bất kỳ surface nào trong `src/`** (chỉ có trong test), nên trên
thực tế hôm nay mọi Thread đều đúng một Work.

Nói cách khác: 1:1 *tình cờ*, 1:N *theo thiết kế*. Merge là chốt vĩnh viễn
cái tình cờ đó.

### Work là đơn vị của bốn cơ chế khác

`work_id` là khoá của: `work_results`, `work_dependencies` (upstream +
downstream), `handoffs`, `decision_work_items`. `handoffs` giữ **cả**
`thread_id` lẫn `work_id` — bằng chứng trực tiếp rằng Work được thiết kế là
đơn vị con của Thread, không phải bí danh của nó.

Cả bốn đều có test riêng (`work_results.rs`, `work_dependencies.rs`,
`handoff_storage.rs`, `decision_work.rs`). Merge buộc phải hoặc bỏ chúng,
hoặc dựng lại tầng con dưới tên khác.

### Conversation là nền chung

15 khai báo FK trỏ về `conversations(id)` qua các migration:
`conversation_members`, `messages`, `work_items`, `publishes` (source +
target), `session_bindings`, `checkpoints`, `memories`, `handoffs`, và hai
self-FK. DM dùng gần hết trong số đó.

Nếu Work nuốt Thread, mọi bảng trên phải trỏ sang `work_items` — và DM khi
đó cần một work item giả để có chỗ treo message và session binding.

### Chi phí kỹ thuật

- SQLite không `ALTER` được FK: phải rebuild ~7 bảng.
- 6 trigger trong `0006`, `0009`, `0010` join `work_items ↔ conversations`,
  phải viết lại (ví dụ `publishes_source_insert_guard` khẳng định publish
  source khớp conversation của work).
- `src/`: 417 chỗ dùng `conversation_id`, 184 `ConversationId`, 106
  `WorkItemId`. `tests/`: 186 chỗ.
- Không có migration nào thu nhỏ được — chỉ có một migration lớn, rủi ro cao,
  đổi lấy việc bớt một bảng.

### Cái merge thực sự tiết kiệm

Một hàng `work_items` và một id cho mỗi Thread, cộng với việc `title`/`goal`
không còn bị copy sang primary Work lúc tạo. Bản copy đó hiện không lệch được
vì không có đường nào đổi tên Thread sau khi tạo.

## Việc nên làm thay thế

1. **Đổi tên surface, giữ domain.** `/thread` đã ẩn khỏi `/help`. Còn lại
   `july thread create|list|member` ở CLI quản trị — thêm bí danh
   `july work ...` và để `thread` lại như alias tương thích. Không đụng storage.
2. **Gỡ chồng nghĩa của `/work`.** Hiện `/work` trong Room liệt kê Work, còn
   trong một Work lại liệt kê work items của nó. Vì hôm nay luôn có đúng một
   item, output bên trong gần như vô nghĩa. Đề xuất: bên trong một Work,
   `/work` liệt kê các Work anh em cùng Room; work item con để dành cho khi
   Deliberation được nối vào.
3. **Nối hoặc gỡ Deliberation.** `DeliberationService` được viết, được test,
   không ai gọi. Đây mới là câu hỏi kiến trúc thật: nếu Decision → nhiều Work
   không bao giờ được dùng, thì tầng sub-work mới là thứ nên bỏ — và *lúc đó*
   merge Thread/Work mới trở thành đơn giản hoá thật sự.

## Điều kiện để xét lại

Merge chỉ đáng làm nếu cả ba điều sau cùng đúng:

```text
[ ] Deliberation bị gỡ, hoặc Decision không còn sinh nhiều Work trong một Thread
[ ] Handoff không còn cần phân biệt thread_id với work_id
[ ] DM có một chỗ treo message/session không cần đi qua work_items
```

Chưa điều nào đúng.
