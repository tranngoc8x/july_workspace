# Lessons

- Khi Sếp nói các tính năng đang làm chỉ là feature bổ sung và A2A thuộc phase khác, phải dừng mọi RoomMessage/A2A implementation, rollback đúng các hunk vừa tạo và quay lại Beads non-A2A đang sẵn sàng; không dùng tên roadmap cũ để suy diễn scope hiện tại.
- Trước khi kết luận một feature chưa được implement hoặc rollback feature đó, phải audit cả `git log --all`, branch/ref liên quan và từng hunk chưa commit; không suy ra từ semantic search hay `git status` tổng quát khi worktree đang chứa nhiều slice trộn lẫn.
- Khi thiết kế A2A cho July, phải khóa product boundary trước: A2A là kênh agent↔agent giữa các July-managed agents cùng Room; không tự suy diễn thành external-agent interoperability hay runtime adapter thay ACP.
- Khi thiết kế autocomplete trong editor mà `Enter` hiện đang submit, phải chốt precedence rõ: lần đầu accept suggestion và thêm space, lần sau mới submit; đồng thời quyết định riêng trường hợp nhiều candidate để không nuốt `Enter` vô ích.
- Khi một thuật ngữ gần giống từ khóa sản phẩm có thể dẫn tới hai luồng UX khác nhau (ví dụ `motion`/`mention`), phải xác nhận nghĩa trước khi chốt plan; sau khi xác nhận thì sửa ngay task scope, không giữ assumption cũ.
- Khi Sếp nói “làm tiếp” sau khi phase trước đã được báo hoàn tất, phải kiểm tra Git/Beads và thực hiện task kế tiếp theo thứ tự; không tự suy diễn thiết kế ghi “future Phase” thành lý do defer nếu prerequisite đã hoàn tất.
- Khi thêm aggregate/lifecycle transaction, phải tìm và xóa hoặc thu hẹp mọi public primitive cũ có thể ghi từng phần; đồng thời chuyển toàn bộ caller và fixture sang invariant-preserving path trước khi báo hoàn tất.
- Khi đổi hướng hoặc thay dependency sau một prototype, phải gỡ import, call site, test và lockfile của prototype trong cùng một bước rồi chạy `cargo check`; không để worktree ở trạng thái nửa migration.
- Khi Sếp yêu cầu “hướng dẫn sử dụng” trong một repository cụ thể, mặc định đối tượng là sản phẩm của repository đó; phải đọc ngữ cảnh workspace trước, không hỏi lệch sang công cụ nội bộ như Beads hay Ponytail.
- Với phím modifier trong TUI, unit test tạo `KeyEvent` thủ công không chứng minh terminal thật phát ra event đó; phải kiểm tra protocol/capability và có verification qua PTY hoặc event capture trước khi báo hỗ trợ.
- Với editor tự giãn, không tự biến giới hạn UX tạm thời thành giới hạn sản phẩm; nếu Sếp yêu cầu bỏ max cố định thì chỉ giữ giới hạn vật lý của viewport và để widget cuộn nội bộ khi hết chỗ.
- Khi đang ở approval gate, phải nói rõ thay đổi chưa được áp dụng; không để Sếp hiểu bản thiết kế vừa trình bày là trạng thái UI đã chạy.
- Khi Sếp yêu cầu bỏ `Esc` để chỉ thoát bằng lệnh nhưng sau đó giữ các compatibility exit (`/quit`, `Ctrl-D`, `Ctrl-C`, EOF), phải giới hạn thay đổi vào global `Esc`; không diễn giải thành xóa mọi đường thoát không phải `/exit`.
- Khi bổ sung onboarding command trong CLI đã có `init`, phải chốt rõ command mapping trước khi thiết kế: command cũ có thể được đổi tên và `init` được tái sử dụng cho project-local setup, không tự mặc định nhét wizard vào `agent add`.
- Khi sinh agent name từ tên thư mục Unicode, “remove Unicode” không đồng nghĩa xóa cả chữ: phải chuyển chữ Latin có dấu về ASCII không dấu (ví dụ `Dự án` → `Du_an`), chỉ loại ký tự không có dạng Latin tương đương.
