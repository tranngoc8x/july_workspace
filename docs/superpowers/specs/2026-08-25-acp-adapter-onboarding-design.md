# Thiết kế: Onboarding cài đặt ACP adapter

- Ngày: 25/08/2026
- Phụ trách: Tony (thang.tn@urbox.vn)
- Trạng thái: đã chốt qua brainstorming, chờ lập kế hoạch thực thi

## 1. Vấn đề

`july agent add` cho phép tạo agent với `transport_type = "acp"` mà `transport_config` rỗng
(`{}`, src/cli/mod.rs:1693). Đường đọc lại đòi đủ sáu field (`parse_acp_config`,
src/runtime/direct_message.rs:381). Kết quả: agent lưu được nhưng `july dm <agent>` chết với

```
invalid ACP transport configuration: field `executable` must be a non-empty string
```

Ba lỗ hổng cộng lại làm tình trạng không thoát ra được:

1. Nơi ghi và nơi đọc `transport_config` không cùng một hợp đồng.
2. Không có `agent update`, nên config sai là không sửa được qua CLI.
3. `agent remove` chỉ chuyển `status = "inactive"` (src/application/collaboration.rs:503).
   Hàng vẫn nằm trong bảng nên tên vẫn bị chiếm, tạo lại cùng tên là `AgentNameConflict`.

Gốc rễ sâu hơn: july chưa bao giờ giúp người dùng có được một ACP adapter. Người dùng mới
không có đường nào để biết cần cài gì, cài ở đâu, và điền gì vào `transport_config`.

## 2. Mục tiêu

- `july init`: màn hình onboarding cho phép chọn và cài các ACP adapter được hỗ trợ.
- Sau `init`, tạo agent chỉ cần trỏ tên adapter; july tự sinh `transport_config` đầy đủ.
- Không bao giờ lưu được một agent `acp` có config không dùng được.
- Thêm adapter mới về sau = thêm một entry trong danh mục tĩnh.

Ngoài phạm vi: quản lý phiên bản nhiều bản song song của cùng một adapter, gỡ adapter,
adapter không nói ACP, hỗ trợ Windows cho màn hình tương tác.

## 3. Kiến trúc

Ba unit mới, mỗi unit một trách nhiệm:

### `src/adapter/catalog.rs`

Danh mục tĩnh, theo đúng pattern `src/cli/registry.rs`:

```rust
pub enum Installer { Npm, Cargo }
pub enum Tier { Core, Optional }

pub struct AdapterSpec {
    pub id: &'static str,
    pub package: &'static str,
    pub version: &'static str,
    pub bin: &'static str,
    pub installer: Installer,
    pub tier: Tier,
    pub summary: &'static str,
}
```

Danh mục ban đầu:

| id | package | version | installer | tier |
|---|---|---|---|---|
| `codex` | `@agentclientprotocol/codex-acp` | 1.6.2 | Npm | Core |
| `claude` | `@agentclientprotocol/claude-agent-acp` | 0.70.0 | Npm | Core |
| `claude-rust` | `claude-code-acp-rs` | 0.1.22 | Cargo | Optional |
| `deepseek` | `@openma/deepseek-harness-acp` | 0.4.26 | Npm | Optional |

Hai adapter Core cùng org với crate `agent-client-protocol` mà july đang pin `=2.0.0`, nên
đồng bộ handshake tốt nhất. `claude-rust` dành cho máy không có node, đánh đổi là biên dịch
từ source. `deepseek` do bên thứ ba (openma-ai) maintain, còn ở 0.4.x nên để Optional và
không tick sẵn.

Catalog không biết filesystem.

### `src/adapter/store.rs`

Quản lý `~/.july/adapters`:

- `installed() -> BTreeMap<&str, Version>` - phiên bản đang cài, đọc từ metadata của chính
  trình cài, không dựng thêm state file nào cho việc này:
  - `Npm`: đọc `adapters/node_modules/<package>/package.json`
  - `Cargo`: đọc `adapters/.crates2.json`
- `install(spec)`:
  - `Npm`: `npm install --prefix ~/.july/adapters <package>@<version>`
  - `Cargo`: `cargo install <crate> --version <version> --root ~/.july/adapters`
- Việc hỏi danh tính adapter gọi sang `transport::probe_agent_identity(executable, arguments)`
  - spawn adapter, gửi `initialize`, đọc `agent_info`, dừng. Hàm này nằm trong `transport`
  chứ không nằm trong `store`, để giữ bất biến "chỉ tầng transport nói ACP".
- `config_for(id, agent_name) -> serde_json::Value` - sinh `transport_config` đầy đủ.

Store không biết stdin.

### `src/cli/init.rs`

Màn hình onboarding: vẽ danh sách, đọc phím, gọi store, in kết quả. Không biết npm hay cargo.

### Bất biến

`src/transport/acp.rs` không đổi một dòng. Nó vẫn chỉ nhận một `AcpAgentConfig` đã đầy đủ.
Adapter luôn là subprocess nói ACP qua stdio; không có đường in-process nào được mở.

Quyết định liên quan: `claude-code-acp-rs` được đưa vào danh mục như một adapter cài bằng
`cargo install`, **không** link vào binary july. Link vào binary sẽ nhân đôi tầng transport,
buộc chu kỳ release của july vào một crate 0.1.x một maintainer, và vẫn không gỡ được phụ
thuộc vào Claude Code CLI cùng phần xác thực của nó.

## 4. Màn hình onboarding

```
July chưa có ACP adapter nào. Chọn adapter để cài:

  ❯ [x] codex        @agentclientprotocol/codex-acp 1.6.2
    [x] claude       @agentclientprotocol/claude-agent-acp 0.70.0
    [ ] claude-rust  bản Rust, không cần node (biên dịch ~2 phút)
    [ ] deepseek     DeepSeek Harness (experimental 0.4.x)

  ↑↓ di chuyển · space chọn/bỏ · enter xác nhận · q thoát
  Đã chọn: 2 adapter
```

- Trigger: tự chạy khi `~/.july/adapters` chưa có adapter nào, và chạy chủ động bằng
  `july init`.
- Adapter `Core` được tick sẵn, bỏ tick được.
- Dòng hướng dẫn phím luôn hiển thị ngay dưới danh sách, cùng dòng đếm số đã chọn cập nhật
  theo từng lần toggle.
- Adapter đã cài mà danh mục pin phiên bản mới hơn: hiện
  `[đã cài 1.1.13 → có 1.6.2, space để cập nhật]`.
- Bỏ tick hết rồi Enter: chặn, không cài rỗng.

### Raw mode

Dùng `libc` + `termios` tự viết, thêm đúng một dependency không có transitive, giữ kỷ luật
pin `=` của Cargo.toml. Ánh xạ phím: `\x1b[A` Up, `\x1b[B` Down, `0x20` Space, `\r`/`\n`
Enter, `0x03` Interrupt, `q` Quit. Một RAII guard restore termios trên `Drop`, để panic hay
thoát giữa dòng cũng không để terminal ở trạng thái raw.

Hệ quả: màn hình tương tác chỉ chạy trên unix.

### Khi stdin không phải TTY

Pipe, CI, hoặc `july init < /dev/null`: không vào raw mode. In danh sách, cài mặc định Core,
kèm dòng

```
stdin không phải terminal, dùng mặc định: codex, claude.
Chỉ định khác bằng july init --adapters <ids>
```

Không treo, không crash.

## 5. Verify sau khi cài

`AcpAgentConfig` bắt buộc `expected_agent_name` và `expected_agent_version`, và
`validate_handshake` (src/transport/acp.rs:463) so khớp chính xác với `agent_info` mà adapter
tự khai. Hai giá trị đó không suy ra được từ metadata package, nên `init` phải hỏi chính
adapter.

Mỗi adapter đi qua ba bước: cài → `transport::probe_agent_identity` → ghi
`~/.july/adapters/identities.json`:

```json
{
  "codex": {
    "name": "codex-acp",
    "version": "1.6.2",
    "bin": "/Users/<user>/.july/adapters/node_modules/.bin/codex-acp"
  }
}
```

Đây là state file duy nhất của thiết kế. Lý do tồn tại: `agent add` không phải spawn lại
adapter chỉ để biết nó tự khai tên gì. Adapter nào verify fail thì không được ghi vào file,
tức là `agent add` sẽ từ chối nó - chặn ở `init` thay vì để lộ ra lúc `dm`.

## 6. Thay đổi ở `agent add`

```
july agent add cashpoint --project ~/webroot/cashpoint --adapter codex
```

`store::config_for("codex", "cashpoint")` sinh sáu field:

| Field | Nguồn |
|---|---|
| `executable` | `identities.json`, đường dẫn tuyệt đối |
| `arguments` | `AdapterSpec`, mặc định `[]` |
| `environment` | `{}` |
| `state_directory` | `~/.july/state/<agent-name>`, july `create_dir_all` |
| `expected_agent_name` | `identities.json`, tên thật lấy lúc verify |
| `expected_agent_version` | `identities.json`, version thật lấy lúc verify |

`state_directory` nằm dưới `~/.july/state/` chứ không phải trong repo của người dùng: repo
sạch, july kiểm soát được quyền ghi mà `verify_writable` (src/transport/acp.rs:450) đòi, và
xoá agent thì xoá state gọn.

`--config <file>` vẫn giữ làm đường escape cho ai cần `environment` hoặc `arguments` riêng.
`--adapter` và `--config` loại trừ nhau; đưa cả hai là lỗi usage. Adapter `acp` mà không có
cái nào trong hai thì fail ngay kèm gợi ý `july init` - không bao giờ lưu `{}` nữa.

Thêm `july agent update <ref> --adapter <id>` để sửa được config mà không phải đụng vào
database, vá đúng chỗ kẹt đã mô tả ở mục 1.

## 7. Xử lý lỗi

| Tình huống | Hành xử |
|---|---|
| `npm` hoặc `cargo` không có trên PATH | Fail trước khi cài gì, in adapter nào cần trình cài nào |
| Cài fail giữa danh sách | Giữ adapter đã cài xong, báo rõ cái fail, exit code khác 0, lần sau `init` cài tiếp cái còn thiếu |
| Verify handshake fail hoặc treo | Timeout 30s, không ghi `identities.json`, adapter coi như chưa dùng được |
| `q` hoặc Ctrl-C giữa màn chọn | Guard `Drop` restore termios, không cài gì, exit 0 |
| Adapter đã cài, chạy `init` lần hai | Hiện trạng thái đã cài và bản mới nếu có, space để cập nhật |

## 8. Kiểm chứng

Test nhắm vào các hàm thuần, không cần TTY và không cần mạng:

- Ánh xạ `bytes → Key`.
- Máy trạng thái chọn: biên đầu/cuối danh sách, toggle, Core tick sẵn, chặn khi chọn rỗng.
- Catalog: id không trùng, version không rỗng, mọi Core có installer hợp lệ. Cùng kiểu test
  đang có trong `registry.rs`.
- Round-trip: `store::config_for()` sinh ra `Value` phải được `parse_acp_config()`
  (src/runtime/direct_message.rs:381) nhận đầy đủ. Đây là test khoá lại chính khe hở đã gây
  ra lỗi ở mục 1.
- `install()` và `probe_identity()` đứng sau trait, test bằng fake.

Không test TTY thật, không test cài thật. Hai việc đó kiểm tra bằng tay khi chạy `july init`
lần đầu.

## 9. Việc dọn dẹp còn nợ

Agent `cashpoint` trên máy dev hiện đang mang `transport_config` do người viết tay
(`state_directory` trỏ `/Users/tranngocthang/webroot/cashpoint/.codex`, `expected_agent_*`
đoán theo codex-acp 1.1.13 và chưa được verify). Sau khi `july init` và
`july agent update` chạy được, chuyển agent này sang config sinh tự động và xoá bản backup
`~/.july/workspace.db.bak-*`.
