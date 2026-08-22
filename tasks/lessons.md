# Lessons

- Khi Sếp nói “làm tiếp” sau khi phase trước đã được báo hoàn tất, phải kiểm tra Git/Beads và thực hiện task kế tiếp theo thứ tự; không tự suy diễn thiết kế ghi “future Phase” thành lý do defer nếu prerequisite đã hoàn tất.
- Khi thêm aggregate/lifecycle transaction, phải tìm và xóa hoặc thu hẹp mọi public primitive cũ có thể ghi từng phần; đồng thời chuyển toàn bộ caller và fixture sang invariant-preserving path trước khi báo hoàn tất.
