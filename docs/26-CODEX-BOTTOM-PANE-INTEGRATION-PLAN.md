# 26 - Tích hợp Codex bottom-pane làm composer của July

- Ngày: 16/09/2026
- Phụ trách: Engineering (July Workspace)
- Trạng thái: Chờ duyệt

## 1. Bối cảnh

`src/tui/codex_bottom_pane/` là bản copy từ `openai/codex` (`codex-rs/tui/src/bottom_pane/`).
Hiện tại thư mục này **chưa được compile**: `src/tui/mod.rs:28-30` chỉ khai báo `app`, `markdown`,
`ui`. Mục tiêu: thay toàn bộ input hiện tại của July bằng bottom-pane của Codex, bỏ các phần
thuộc về product/runtime riêng của Codex.

### 1.1 Phát hiện quan trọng

Bản copy **thiếu ~20 module hạ tầng** của `codex-rs/tui/src` mà bottom-pane phụ thuộc trực tiếp.
Số lần tham chiếu trong thư mục đã copy:

| Module thiếu | Lượt dùng | Vai trò |
|---|---|---|
| `crate::keymap` | 180 | Phân giải phím runtime (editor, vim, list, chord) |
| `crate::key_hint` | 113 | `KeyBinding`, `ShortcutHint`, render gợi ý phím |
| `crate::render` | 76 | `Renderable`, `Insets`, `RectExt`, `line_utils` |
| `crate::app_event` / `app_event_sender` | 71 | Bus sự kiện của Codex |
| `crate::wrapping` | 20 | Soft wrap có nhận biết hyperlink |
| `crate::terminal_palette` | 19 | Dò color level của terminal |
| `crate::style` | 16 | `accent_style`, `user_message_style` |
| `crate::task_mentions` | 14 | Mention sang thread khác |
| `crate::terminal_hyperlinks` | 13 | OSC 8 hyperlink |
| `crate::line_truncation` | 13 | Cắt dòng + ellipsis |
| `crate::width` | 8 | `display_width` |
| `crate::slash_command` | 7 | Enum ~45 slash command của Codex |
| `crate::text_formatting`, `vim_search`, `color`, `clipboard_paste`, `ui_consts`, `mention_codec`, `terminal_probe` | 2-6 mỗi module | Tiện ích |

Source gốc có sẵn tại `~/Downloads/codex-main/codex-rs` nên port được nguyên bản.

### 1.2 Phụ thuộc crate `codex_*`

| Crate | Item thực sự cần | Xử lý |
|---|---|---|
| `codex_protocol` | `user_input::{TextElement, ByteRange, MAX_USER_INPUT_TEXT_CHARS}`, `ThreadId` | Vendor 2 type nhỏ vào `src/tui/user_input.rs` (bỏ serde/ts-rs); `ThreadId` → `ContextId` của July |
| `codex_utils_fuzzy_match` | `fuzzy_match` | Vendor 163 dòng |
| `codex_file_search` | `FileMatch`, `MatchType` | Vendor struct; walker viết mới cho July (xem Phase 3) |
| `codex_message_history` | `HistoryBatchCursor` | Vendor struct nhỏ |
| `codex_app_server_protocol` | `SkillMetadata`, `ToolRequestUserInputParams`, `RequestId` | Skills bỏ; phần `request_user_input` map sang domain type của July |
| `codex_config`, `codex_features`, `codex_plugin`, `codex_connectors`, `codex_apps`, `codex_home`, `codex_context_fragments` | - | Bỏ hoàn toàn |

### 1.3 Ràng buộc môi trường July

- `ratatui = 0.30.2` với `default-features = false`. `WidgetRef`/`StatefulWidgetRef` nằm sau
  feature `unstable-widget-ref` → phải bật thêm.
- `Cargo.toml` **không có** `[dev-dependencies]`. Test của Codex dùng `insta` + `pretty_assertions`
  → bỏ toàn bộ `tests/` và 283 file `.snap`.
- Cần thêm dependency: `unicode-segmentation`, `textwrap`. (`unicode-width` đã có.)
- Sau khi xong: gỡ `ratatui-textarea` (textarea của Codex là bản tự viết, không dùng crate này).
- July `App` là reducer thuần `reduce(AppEvent) -> Vec<AppCommand>`; `ui::render(frame, &App)`.
  `BottomPane` là state có `&mut` khi xử lý phím → đặt làm field của `App`, render qua `&self`.

### 1.4 Kiểm tra phụ thuộc đã đóng chưa (transitive closure)

Quét toàn bộ 56 file giữ lại, mọi tham chiếu `crate::<module>` ra ngoài `bottom_pane` được phân
thành 3 nhóm. **Không còn nhóm thứ 4 nào bị bỏ sót** - sau khi port nhóm A, các module nhóm A
không kéo thêm module nào của codex-tui nữa (đã kiểm `use` của từng file).

**A. PORT nguyên bản (leaf util, không kéo thêm gì)**

| Module | Item được dùng | Phụ thuộc tiếp theo |
|---|---|---|
| `key_hint` | `KeyBinding`, `ShortcutHint`, `KeyBindingListExt`, `plain`, `ctrl`, `has_ctrl_or_alt`, `is_plain_text_key_event`, `is_altgr` | không |
| `keymap` | `RuntimeKeymap`, `KeymapContext(Set)`, `ListKeymap`, `ListAction`, `EditorKeymap`, `VimNormal/Operator/Search/TextObjectKeymap`, `user_bindings` | `key_hint`, `codex_config::types::{TuiKeymap, KeybindingsSpec, MAX_FUNCTION_KEY}`, `tokio::time::Instant` |
| `render` | `renderable::Renderable`, `Insets`, `RectExt`, `line_utils` | không (**không** port `highlight.rs`) |
| `width` | `display_width` | không |
| `line_truncation` | `truncate_line_with_ellipsis_if_overflow`, `line_width` | `width` |
| `text_formatting` | `truncate_text` | `width` |
| `wrapping` | `RtOptions`, `word_wrap_line(s)`, `wrap_ranges` | `line_truncation`, `render::line_utils`, `width`, crate `textwrap` |
| `terminal_hyperlinks` | `TerminalHyperlink`, `HyperlinkLine`, `web_links_in_text`, `mark_buffer_hyperlinks` | `wrapping`, `line_truncation`, `render::line_utils`, `width` |
| `terminal_palette` | `best_color`, `rgb_color`, `effective_stdout_color_level`, `with_test_default_colors` | `color`, `codex_terminal_detection` |
| `style` | `accent_style`, `user_message_style` | `color`, `terminal_palette` |
| `color` | (qua `style`/`terminal_palette`) | không |
| `terminal_probe` | `DefaultColors` | không |
| `vim_search` | `SearchQuery`, `SearchDirection`, `matching_ranges` | `unicode-segmentation` |
| `ui_consts` | `FOOTER_INDENT_COLS`, `LIVE_PREFIX_COLS` | không (12 dòng) |
| `footer_hint` | `wrap_hint_rows` | không (40 dòng) |
| `clipboard_paste` | `is_probably_wsl`, `pasted_image_format`, `normalize_pasted_path`, `normalize_pasted_search_query` | chỉ copy 4 hàm này, **không** port cả file (tránh dep `tempfile`) |
| `mention_codec` | `is_common_env_var`, `decode_history_mentions_with_at_mentions` | copy kèm 2 hằng sigil, bỏ `codex_utils_plugins` |
| `history_cell` | `sanitize_user_text` | copy đúng 1 hàm |
| `onboarding` | `mark_underlined_hyperlink` | copy đúng 1 hàm |

Hai stub nhỏ phải tự viết:
- `codex_terminal_detection` → chỉ dùng cho `TerminalName::WindowsTerminal`; stub đọc env (~15 dòng).
- `codex_config::types` (keymap) → vendor `tui_keymap.rs` (806 dòng) sau khi bỏ `serde`/`schemars`,
  luôn truyền `TuiKeymap::default()`. Giữ nguyên code keymap đã port, không phải sửa logic.

**B. THAY bằng tương đương của July**

| Module Codex | Thay bằng |
|---|---|
| `app_event::AppEvent`, `app_event_sender::AppEventSender`, `app_command::AppCommand` | `Vec<AppCommand>` / enum effect nội bộ của July |
| `app_event::{HistoryLookupResponse, HistoryBatchEntryResponse}` | callback trực tiếp vào `ChatComposerHistory` (July giữ history trong process) |
| `app_event::ConnectorsSnapshot` | bỏ (connectors đã cắt) |
| `tui::FrameRequester` | cờ `dirty` trong `run_app` (`src/tui/mod.rs:271-292`) |
| `slash_command::{SlashCommand, built_in_slash_commands}` | `Context::commands: Vec<String>` (`app.rs:48`) |
| `test_support`, `test_backend` | bỏ (chỉ dùng trong test đã loại) |

**C. CẮT cùng feature Codex tương ứng**

| Module | Lượt dùng | Lý do |
|---|---|---|
| `task_mentions` | 14 | Mention sang thread khác qua app-server của Codex |
| `status_indicator_widget` | 5 | Status line/spinner - đã cắt |
| `app::app_server_requests` | 4 | App-server request plumbing |
| `skills_helpers` | 3 | Skills - đã cắt |
| `theme_picker` | 2 | Theme picker của Codex |
| `status::format_tokens_compact` | 1 | Context-window meter - đã cắt |
| `backend_banners::BackendBanner` | 1 | Banner của Codex |

**Crate ngoài còn lại cần thêm vào `Cargo.toml`:** `unicode-segmentation`, `textwrap`.
Không cần `image`, `tempfile`, `ignore`, `nucleo`, `syntect`, `insta`, `pretty_assertions`,
`itertools`, `strum`, `uuid`, `url`, `serde`.
`itertools` chỉ dùng ở `list_selection_view.rs` (1 chỗ) - thay bằng `std` iterator.
`uuid` chỉ dùng ở `apply_text_suggestion` - đã cắt khỏi trait.

## 2. Phạm vi

### 2.1 Giữ lại (composer generic + view framework)

`chat_composer.rs` + `chat_composer/`, `chat_composer_history.rs` + `chat_composer_history/`,
`textarea.rs` + `textarea/`, `footer.rs`, `command_popup.rs`, `file_search_popup.rs`,
`mentions_v2/`, `paste_burst.rs`, `popup_consts.rs`, `scroll_state.rs`,
`selection_popup_common.rs`, `selection_row_layout.rs`, `selection_tabs.rs`, `prompt_args.rs`,
`slash_commands.rs`, `bottom_pane_view.rs`, `list_selection_view.rs`, `custom_prompt_view.rs`,
`multi_select_picker.rs`, `request_user_input/`, `async_questions/`, `questions.rs`, `mod.rs` (viết lại).

### 2.2 Bỏ

Theo yêu cầu ban đầu:
`approval_overlay.rs`, `pending_thread_approvals.rs`, `mcp_server_elicitation.rs`,
`experimental_features_view.rs`, `memories_settings_view.rs`, `hooks_browser_view.rs`,
`feedback_view.rs`, `effort_ignition.rs`, `effort_ignition_styles.rs`, `effort_status_line.rs`,
`skills_toggle_view.rs`, `unified_exec_footer.rs`, `status_line_setup.rs`,
`status_surface_preview.rs`, `status_line_style.rs`, `actionable_banner.rs`, `app_link_view.rs`,
`apply_patch_header.rs`, `hook_status.rs`, `voice_strip.rs`.

Bổ sung (đã chốt):
`title_setup.rs`, `action_required_title.rs`, `user_verification.rs`, `feedback_note_view.rs`,
`pending_input_preview.rs`, `skill_popup.rs`, `startup.rs`, toàn bộ `tests/` và `snapshots/`,
mọi file `*_tests.rs` dùng `insta`/`pretty_assertions`.

### 2.3 Tính năng composer giữ lại

- Vim mode (normal/insert/operator/text-object/search) + reverse history search.
- `@mention` v2 popup, nguồn dữ liệu đổi sang danh sách agent của July.
- `@file` search popup (walker viết mới cho July).
- Paste burst, multiline, soft wrap, footer hint.

## 3. Kiến trúc đích

```
src/tui/
├── mod.rs                 # thêm: mod support; mod bottom_pane;
├── app.rs                 # App giữ BottomPane thay TextArea + completion state
├── ui.rs                  # render BottomPane qua Renderable
├── user_input.rs          # TextElement, ByteRange (vendor)
├── file_search.rs         # walker + fuzzy cho @file (mới)
├── support/               # hạ tầng port từ codex-rs/tui/src
│   ├── mod.rs
│   ├── width.rs  color.rs  ui_consts.rs  vim_search.rs  clipboard_paste.rs
│   ├── key_hint.rs  line_truncation.rs  text_formatting.rs
│   ├── fuzzy_match.rs  terminal_probe.rs  terminal_palette.rs  style.rs
│   ├── wrapping.rs  terminal_hyperlinks.rs
│   ├── render/{mod.rs, renderable.rs, line_utils.rs}
│   └── keymap/{mod.rs, bindings.rs, chords.rs, vim_search.rs}
└── bottom_pane/           # đổi tên từ codex_bottom_pane, đã prune
```

`AppEventSender` của Codex thay bằng một enum nội bộ `PaneEffect` mà `BottomPane` trả về, hoặc
`Vec<AppCommand>` của July - không dựng lại bus sự kiện. `FrameRequester` thay bằng cờ `dirty`
sẵn có trong vòng lặp `run_app` (`src/tui/mod.rs:271-292`).

`InputResult` rút gọn cho July:

```rust
pub enum InputResult {
    Submitted { text: String, text_elements: Vec<TextElement> },
    Command { name: String, args: String },
    None,
}
```

`PermissionModal` hiện tại (`app.rs:193`) chuyển thành một `BottomPaneView` dựa trên
`ListSelectionView`, bỏ code render modal thủ công trong `ui.rs:99-146`.

## 4. Kế hoạch theo phase

### Phase 0 - Chuẩn bị (xong)

- [x] Bật feature `unstable-widget-ref` cho `ratatui` trong `Cargo.toml`.
- [x] Thêm `unicode-segmentation = "=1.13.3"`, `textwrap = "=0.16.2"`.
- [x] Tạo `src/tui/user_input.rs`: `TextElement`, `ByteRange`, `MAX_USER_INPUT_TEXT_CHARS` (không serde), 3 test.
- [x] `cargo check` xanh.

### Phase 1 - Port hạ tầng (`src/tui/support/`) (xong)

- [x] `width.rs`, `color.rs`, `ui_consts.rs`, `vim_search.rs`, `footer_hint.rs`
- [x] `key_hint.rs`
- [x] `line_truncation.rs`, `text_formatting.rs`
- [x] `fuzzy_match.rs` (vendor từ `codex-rs/utils/fuzzy-match`)
- [x] `render/{mod.rs, renderable.rs, line_utils.rs}` - bỏ `highlight.rs` (syntect)
- [x] `wrapping.rs`
- [x] `terminal_detection.rs` (stub mới), `terminal_palette.rs`, `style.rs`
- [x] `terminal_hyperlinks.rs` + `terminal_hyperlinks/paragraph.rs`
- [x] `tui_keymap.rs` (bỏ serde/schemars), `keymap.rs` + `keymap/{bindings,chords,vim_search}.rs`
- [x] `clipboard_paste.rs` (chỉ 2 hàm không phụ thuộc)
- [x] `cargo check` 0 lỗi; `cargo test --lib tui::support` 202 test xanh

#### Quyết định phát sinh trong Phase 1

| Vấn đề | Quyết định |
|---|---|
| `terminal_probe.rs` (867 dòng + submodule unix/windows) chỉ được dùng cho `DefaultColors` | **Bỏ hẳn.** Nó truy vấn OSC 10/11 trực tiếp trên tty lúc khởi động; July đã tự sở hữu raw-mode bracket và event stream nên probe sẽ tranh chấp. `terminal_palette::default_colors()` giờ đọc từ một ô nhớ do `set_default_colors()` ghi, mặc định `None` → rơi về bảng màu xterm cố định. |
| `terminal_palette` dùng crate `supports_color` | Thay bằng đọc env (`NO_COLOR`, `COLORTERM`, `TERM`) - đúng những biến crate đó đọc. Sai lệch chỉ ảnh hưởng độ trung thực màu. |
| `terminal_hyperlinks` + `wrapping` dùng crate `url` | Crate `url` kéo theo ~20 crate ICU/IDNA. Thay bằng tách scheme/host thủ công. Lớp chống chèn escape (`sanitized_destination`: lọc control char + giới hạn độ dài) giữ nguyên. |
| `terminal_hyperlinks::TrustedWorkspaceFile` (link `file://` tới file trong workspace) | Bỏ - là feature markdown/visualization của Codex, không thuộc composer. `DestinationKind` gộp còn một nhánh web. |
| `keymap` đọc alias người dùng cấu hình qua `serde_json` reflection | `configured_*_alias_is_used()` trả `false` cố định: July luôn resolve từ `TuiKeymap::default()`. Thêm test `built_in_defaults_resolve_without_conflicts` để bắt lỗi nếu default bị xung đột. |
| `wrapping` test dùng `itertools` | Thay `collect_vec`/`join` bằng `std`. |
| Test Codex dùng `insta`, `tempfile`, `VT100Backend`, `custom_terminal` | Xoá 15 test thuộc các đường đã cắt (snapshot render, config overlay, trusted file). Giữ lại 202 test chạy được. |
| `mention_codec`, `history_cell::sanitize_user_text` | Hoãn sang Phase 3 - chỉ có nghĩa khi nhìn call site thật trong `chat_composer`. `mention_codec` decode sigil `$tool`/`@plugin` của Codex nên nhiều khả năng bị cắt. |

`src/tui/support/mod.rs` đang có `#![allow(dead_code)]` vì chưa module nào tiêu thụ. **Gỡ ở Phase 5.**

### Phase 2 - Prune `codex_bottom_pane` → `bottom_pane` (xong)

- [x] Xoá 33 file trong mục 2.2 (gồm `*_tests.rs` tương ứng).
- [x] Xoá `tests/` và 7 thư mục `snapshots/` (283 file `.snap`).
- [x] `mv src/tui/codex_bottom_pane src/tui/bottom_pane` (không dùng lệnh git).
- [x] Rewrite 62 file: `crate::bottom_pane` → `crate::tui::bottom_pane`,
      `crate::<support>` → `crate::tui::support::<...>`,
      `codex_protocol::user_input::` → `crate::tui::user_input::`,
      gỡ mọi `use pretty_assertions::...` (giữ nguyên test, `std::assert_eq` tương thích).

Còn 76 file / ~50k dòng. `mod bottom_pane;` **chưa khai báo** trong `src/tui/mod.rs` - chờ Phase 3
gỡ hết tham chiếu treo rồi mới bật để compiler chỉ đúng chỗ còn thiếu.

Tham chiếu `crate::` còn treo, đúng bằng nhóm B + C ở mục 1.4:

| Tham chiếu | Lượt | Nhóm |
|---|---|---|
| `crate::app_event` | 23 | B |
| `crate::task_mentions` | 14 | C |
| `crate::app_event_sender` | 11 | B |
| `crate::test_support` | 9 | B (bỏ) |
| `crate::slash_command` | 7 | B |
| `crate::status_indicator_widget` | 6 | C |
| `crate::app` | 4 | C |
| `crate::skills_helpers`, `crate::history_cell` | 3 mỗi cái | C / B |
| `crate::theme_picker`, `crate::terminal_probe`, `crate::mention_codec`, `crate::app_command` | 2 mỗi cái | C / C / hoãn / B |
| `crate::test_backend`, `crate::status`, `crate::onboarding`, `crate::backend_banners` | 1 mỗi cái | B / C / B / C |

Còn ~100 call site `insta::assert_snapshot!` nằm rải trong 20 file; xử lý ở Phase 3-4 khi
compiler chỉ đúng từng chỗ.

### Phase 3 - Gỡ phụ thuộc codex khỏi phần giữ lại

- [ ] `textarea.rs`, `textarea/`: đổi `codex_protocol::user_input::*` → `crate::tui::user_input::*`.
- [ ] `chat_composer_history.rs`: vendor `HistoryBatchCursor`; `ThreadId` → `ContextId`.
- [ ] `slash_commands.rs` + `command_popup.rs`: thay enum `SlashCommand` của Codex bằng nguồn
      `Context::commands: Vec<String>` của July (`app.rs:48`).
- [ ] `mentions_v2/search_catalog.rs`: bỏ skills/plugins/connectors, nguồn dữ liệu = agent list July
      (`AppEvent::Agents`, `app.rs:484`).
- [ ] `file_search_popup.rs` + `mentions_v2/filter.rs`: vendor `FileMatch`/`MatchType`.
- [ ] Viết `src/tui/file_search.rs`: walker đệ quy `std::fs`, bỏ `.git`/`target`/`node_modules`,
      giới hạn depth + số kết quả, chấm điểm bằng `fuzzy_match`. Không thêm dep `ignore`/`nucleo`.
- [ ] `chat_composer.rs`: cắt reasoning effort, skills, connectors/apps, plugins, voice, feedback,
      image paste (`image` crate), service tier, collaboration mode, goal status, worktrees,
      side conversation, luna reserve, windows degraded sandbox, IDE context.
- [ ] `footer.rs`: rút `FooterProps` về đúng state July còn dùng.
- [ ] `request_user_input/`, `async_questions/`, `questions.rs`: thay type app-server protocol bằng
      domain type July (`PermissionOption`, câu hỏi của agent). **Rủi ro cao nhất của kế hoạch** -
      nếu chi phí vượt dự kiến sẽ báo lại Sếp trước khi tiếp tục.

### Phase 4 - Viết lại `bottom_pane/mod.rs`

- [ ] `BottomPane` chỉ giữ field CORE: `composer`, `view_stack`, `has_input_focus`,
      `enhanced_keys_supported`, `disable_paste_burst`, `esc_backtrack_hint`, `animations_enabled`,
      `keymap`. Bỏ `app_event_tx`, `frame_requester`, `thread_id`, `status`, `hook_status_message`,
      `inline_banner`, `status_timer`, `unified_exec_footer`, `pending_input_preview`,
      `pending_thread_approvals`, `context_window_*`, `delayed_approval_requests`,
      `last_composer_activity_at`.
- [ ] `BottomPaneParams`: `has_input_focus`, `enhanced_keys_supported`, `placeholder_text`,
      `disable_paste_burst`, `animations_enabled`.
- [ ] `BottomPaneView`: bỏ 8 method codex-only (`try_consume_approval_request`,
      `try_consume_user_input_request`, `try_consume_mcp_server_elicitation_request`,
      `matches_app_server_request`, `dismiss_app_server_request`, `terminal_title_requires_action`,
      `will_interrupt_turn_on_key_event`, `apply_text_suggestion`).
- [ ] Giữ nguyên cơ chế view stack: `push_view`, `pop_active_view_with_completion` (kể cả
      cascade `dismiss_after_child_accept`), `replace_*_if_present`, `dismiss_view_by_id`.
- [ ] `impl Renderable for BottomPane`.

### Phase 5 - Đấu nối vào `App` / `ui.rs`

- [ ] `App`: xoá `input: TextArea`, `completion_selected`, `prompt_history`, `history_index`,
      `history_draft`; thêm `bottom_pane: BottomPane`.
- [ ] `reduce_key` (`app.rs:548`) route: permission → BottomPane → global (Ctrl-C, scroll, Ctrl-D).
      Giữ nguyên thứ tự ưu tiên hiện có cho phần global.
- [ ] `submit()` (`app.rs:856`) nhận `InputResult::Submitted`/`Command`, giữ nguyên
      `AppCommand::Submit` / `AppCommand::Execute` và cơ chế single-flight `pending`.
- [ ] `ui.rs`: thay `app.input_widget()` + block completion thủ công (`ui.rs:47-83`) bằng
      `BottomPane::render`; chiều cao lấy từ `desired_height(width)`; đặt cursor theo `cursor_pos`.
- [ ] `PermissionModal` → `BottomPaneView` trên `ListSelectionView`; xoá `permission_scroll`
      (`ui.rs:150`).
- [ ] Gỡ `ratatui-textarea` khỏi `Cargo.toml`.

### Phase 6 - Test & nghiệm thu

- [ ] Port các test input hiện có của July sang API mới (danh sách trong mục 6).
- [ ] Giữ `tests/tui_terminal.rs` xanh (PTY test không được đổi hành vi vào/ra alternate screen).
- [ ] `cargo test` toàn bộ xanh.
- [ ] `cargo clippy -- -D warnings` (nếu repo đang bật).
- [ ] Chạy thử `july` thật, kiểm tay: gõ, xuống dòng, `/` popup, `@` agent, `@file`, vim mode,
      history search, paste nhiều dòng, resize, permission modal.

## 5. Rủi ro

| Rủi ro | Ảnh hưởng | Giảm thiểu |
|---|---|---|
| `request_user_input/` + `async_questions/` (~5.5k dòng) bám chặt app-server protocol | Cao | Làm sau cùng trong Phase 3; nếu chi phí vượt dự kiến, báo Sếp để cân nhắc bỏ |
| `keymap` của Codex sinh ra từ config TOML | Trung bình | Chỉ giữ default binding, bỏ lớp config |
| Khác biệt API ratatui 0.29 (Codex) vs 0.30.2 (July) | Trung bình | Sửa theo lỗi compile từng module; đã xác nhận `WidgetRef` còn tồn tại sau feature flag |
| Mất test coverage do bỏ 283 snapshot | Trung bình | Viết lại test hành vi (không snapshot) cho composer, popup, view stack |
| `App` reducer thuần vs `BottomPane` có side effect (Cell, timer) | Thấp | Bỏ `FrameRequester`/timer; render vẫn `&self` |

## 6. Test July hiện có phải chuyển đổi

`src/tui/app.rs`: 1431, 1450, 1466, 1478, 1488, 1521, 1537, 1596, 1617, 1685, 1702, 1718, 1790,
1817, 1836, 1849, 1862, 1885, 1895, 1912, 1928, 1939, 1965, 1978, 1992, 2095, 2203, 2237, 2373.

`src/tui/ui.rs`: 192, 287, 320, 345, 383, 400, 428.

### Quyết định phát sinh ở Phase 4-5

| Vấn đề | Quyết định |
|---|---|
| `FrameRequester` của Codex lên lịch redraw theo delay | Bỏ. `run_app` đã vẽ theo nhịp 33ms; `BottomPane::pre_draw_tick` + `flush_paste_burst_if_due` chạy mỗi tick, và `App::take_pane_redraw()` báo cho vòng lặp biết khi nào cần vẽ lại. |
| `PermissionModal` chuyển thành `BottomPaneView` | **Hoãn.** Modal hiện tại vẽ ở giữa màn hình; `ListSelectionView` vẽ trong bottom pane nên chuyển sẽ đổi UX permission. Giữ nguyên, ghi nhận là việc riêng. |
| Paste | Bật `EnableBracketedPaste` trong terminal guard, thêm `AppEvent::Paste` → `BottomPane::handle_paste`, và tắt heuristic paste-burst. Có bracketed paste thì đoán paste theo nhịp gõ vừa thừa vừa làm test phụ thuộc thời gian. |
| Chiều cao input | Composer của Codex có thêm một dòng footer hint, nên `input_height()` cho draft rỗng là 4 thay vì 3. Các test July hard-code vị trí dòng phải cập nhật theo. |
| `ratatui-textarea` | Đã gỡ khỏi `Cargo.toml` - textarea của Codex là bản tự viết. |

## 6a. Sự cố và khôi phục (16/09/2026)

Trong lúc dọn test của Phase 6, một script tự động xoá test theo vị trí lỗi compile đã xoá nhầm cả
hàm production (nó lùi về `fn` gần nhất phía trên dòng lỗi, và với lỗi nằm trong thân hàm production
thì hàm đó bị xoá). `cargo check --lib` từ 0 lỗi thành 49.

Khôi phục: so bộ tên hàm production của từng file với bản gốc `codex-main` để xác định thiệt hại,
rồi chép lại file gốc và replay các thay đổi bằng script (rewrite cơ học + từng slice đã cắt).
Sau khôi phục, mọi hàm production còn thiếu đều đúng bằng các phần cắt có chủ ý; `cargo check --lib`
và `cargo check` trở lại 0 lỗi, 0 cảnh báo. Toàn bộ `*_tests.rs` được chép lại từ bản gốc để Phase 6
làm lại từ đầu một cách có chủ đích.

Bài học ghi vào `tasks/lessons.md`: không xoá code theo vị trí lỗi compile một cách tự động; test và
production nằm chung file nên cần lọc theo phạm vi `#[cfg(test)]` và chạy từng bước có kiểm chứng.

## 7. Review

(Điền sau khi hoàn thành.)
