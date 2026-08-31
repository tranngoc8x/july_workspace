# Hướng dẫn sử dụng July Workspace

Tài liệu này hướng dẫn cách cài đặt và sử dụng **July Workspace 0.1.0** theo
đúng command surface đang được triển khai trong repository hiện tại.

Sau khi hoàn thành phần Quick start, bạn sẽ có:

- một ACP adapter đã được cài và xác minh;
- một project agent tồn tại bền vững trong SQLite;
- một Room có agent tham gia;
- một phiên làm việc mở bằng `@agent` chạy qua TUI/REPL;
- dữ liệu workspace được giữ lại giữa các lần chạy July.

## 1. July Workspace là gì?

July là workspace local-first để phối hợp nhiều coding agent trên nhiều project.

Bạn chỉ cần nhớ ba khái niệm:

```text
Agent   ai làm
Room    nhóm / khu vực
Work    việc đang làm
```

và hai ký tự:

```text
@   chọn agent
/   chạy command
```

Bên dưới, July còn giữ `Result` (kết quả có cấu trúc của Work) và `Session`
(phiên runtime có thể resume hoặc thay thế). `Thread`, `Conversation` và
`Session` là chi tiết triển khai — bạn không cần tạo hay đặt tên chúng.

Điểm cần nhớ:

- `Agent` không phải là một process Codex/Claude đang chạy.
- Thêm Agent không tự khởi động model session.
- Room là nơi khởi động việc, không phải một shared prompt chứa toàn bộ lịch sử.
- Work là ranh giới ngữ cảnh; transcript không tự chảy sang Work khác.
- Result có thể được publish qua ranh giới; transcript thì không.

## 2. Yêu cầu hệ thống

### Bắt buộc

- Git để clone repository.
- Rust `1.96.0` và Cargo.
- Node.js và npm nếu dùng `codex`, `claude` hoặc `deepseek`. Luồng onboarding
  mặc định chọn `codex,claude`, nên máy chỉ có Rust chưa đủ cho mặc định này.
- Một terminal hỗ trợ chế độ tương tác.
- Quyền ghi vào `~/.july`. Nếu đổi vị trí dữ liệu, đặt `JULY_HOME` cho
  adapter/state và `JULY_WORKSPACE_DB` cho SQLite database.
- Coding-agent provider đã được đăng nhập sẵn. ACP handshake thành công không
  thay thế bước xác thực tài khoản Codex, Claude hoặc provider tương ứng.

Trước khi dùng July, hãy mở CLI/runtime của provider ít nhất một lần và hoàn
tất login theo hướng dẫn chính thức của provider:

- Codex: chạy `codex`, sau đó chọn phương thức sign-in khi được hỏi; xem
  [Codex CLI](https://learn.chatgpt.com/docs/codex/cli).
- Claude Code: chạy `claude` và hoàn tất authentication flow; xem
  [Claude Code setup](https://docs.anthropic.com/en/docs/claude-code/getting-started).

July cài ACP adapter nhưng không quản lý credential tài khoản model.

### Theo adapter

| Adapter | Runtime | Công cụ cài đặt |
|---|---|---|
| `codex` | Codex qua `@agentclientprotocol/codex-acp` | npm |
| `claude` | Claude Code qua `@agentclientprotocol/claude-agent-acp` | npm |
| `claude-rust` | Claude Code qua `claude-code-acp-rs` | Cargo |
| `deepseek` | DeepSeek Harness, experimental | npm |

`codex` và `claude` là hai adapter core được chọn mặc định trong màn hình
onboarding. Các phiên bản package được July pin trong catalog của release.
Repository không áp một minimum version riêng cho Node/npm; trước khi onboard
adapter npm, kiểm tra các binary cần thiết có trên `PATH`:

```bash
node --version
npm --version
codex --version   # nếu dùng Codex
claude --version  # nếu dùng Claude Code
```

## 3. Cài đặt

### Build và chạy từ source

```bash
git clone https://github.com/tranngoc8x/july_workspace.git
cd july_workspace
cargo build --release
./target/release/july --version
```

### Cài binary vào Cargo bin directory

```bash
cargo install --path .
july --version
```

Kết quả phiên bản hiện tại:

```text
july 0.1.0
```

> `july --help` chưa phải command help tổng quát. Để xem help tương tác, chạy
> `july`, sau đó dùng `/help` hoặc `/help <command>`.

## 4. Dữ liệu và biến môi trường

Mặc định July sử dụng:

```text
~/.july/
├── workspace.db       SQLite database của workspace
├── adapters/
│   ├── ...             package và binary ACP adapter
│   └── identities.json danh tính adapter đã xác minh
└── state/             state directory riêng của agent runtime
```

Hai biến môi trường hữu ích:

| Biến | Tác dụng |
|---|---|
| `JULY_HOME` | Đổi thư mục chứa adapter, identity và runtime state |
| `JULY_WORKSPACE_DB` | Đổi chính xác đường dẫn SQLite database |

Ví dụ tạo một môi trường thử nghiệm tách khỏi dữ liệu thật:

```bash
export JULY_HOME=/tmp/july-demo-home
export JULY_WORKSPACE_DB=/tmp/july-demo-workspace.db
```

Không đặt hai biến này nếu bạn muốn dùng dữ liệu mặc định trong `~/.july`.

## 5. Quick start

### Bước 1: Cài và xác minh adapter

Chạy onboarding tương tác:

```bash
july setup
```

Trong màn hình chọn adapter:

- `↑` / `↓`: di chuyển;
- `Space`: chọn hoặc bỏ chọn;
- `Enter`: xác nhận;
- `q` hoặc `Ctrl-C`: thoát.

Để chạy không tương tác hoặc chỉ cài một adapter:

```bash
july setup --adapters codex
```

Nhiều adapter được phân tách bằng dấu phẩy:

```bash
july setup --adapters codex,claude
```

Nếu stdin không phải TTY và không có `--adapters`, July mặc định chọn
`codex,claude`.

Một adapter thành công sẽ có dòng xác minh, sau đó July in hướng dẫn tạo Agent:

```text
Đang cài codex (@agentclientprotocol/codex-acp <version>)
  đã xác minh: <agent-name> <agent-version>
Xong. Tạo agent cho thư mục hiện tại bằng: july init
```

### Bước 2: Thêm project agent

```bash
cd /absolute/path/to/cashpoint
july init
```

July hiển thị tên mặc định lấy từ tên thư mục hiện tại. Chữ Latin Unicode được
chuyển về không dấu và khoảng trắng thành `_`; ví dụ `Dự án Thanh Toán` thành
`Du_an_Thanh_Toan`. Nhấn `Enter` để dùng tên mặc định, hoặc nhập tên khác. Sau
đó dùng `↑` / `↓` và `Enter` để chọn một adapter đã cài.

Cho script hoặc automation, dùng dạng đầy đủ không tương tác:

```bash
july agent add cashpoint \
  --project /absolute/path/to/cashpoint \
  --adapter codex
```

`--runtime` là metadata tùy chọn, không thay thế `--adapter`:

```bash
july agent add cashpoint \
  --project /absolute/path/to/cashpoint \
  --adapter codex \
  --runtime codex
```

Kiểm tra agent:

```bash
july agent list
july agent show cashpoint
```

`agent add` và `agent show` in một dòng tab-separated gồm:

```text
<agent-id>  <name>  <project-root>  <transport>  <runtime>  <status>
```

### Bước 3: Chat trực tiếp với agent

Cách ngắn nhất là mở workspace rồi gọi tên agent:

```bash
july
```

```text
> @cashpoint fix callback retry
```

July mở việc trực tiếp với `cashpoint` và gửi luôn prompt. Gõ tiếp là đi vào
cùng ngữ cảnh đó:

```text
> also add regression tests
```

Gọi lại `@cashpoint` sau này sẽ nối tiếp đúng transcript cũ.

Nếu chỉ cần một stream DM độc lập, không qua workspace shell:

```bash
july dm cashpoint
```

Nhập prompt rồi nhấn `Enter`. Khi đang ở prompt idle, dùng `/exit`, `/quit`, EOF
hoặc `Ctrl-C` để đóng phiên CLI. Khi turn hoặc permission đang chạy, lần
`Ctrl-C` đầu tiên chỉ gửi cancel; July chờ turn kết thúc rồi trở lại prompt.
Trong `july dm`, các chuỗi như `/status` không phải command July; ngoại trừ
`/exit` và `/quit`, chúng được gửi nguyên văn cho agent.

### Bước 4: Tạo Room

```bash
july room create VNA --description "VNA product collaboration"
july room member add VNA cashpoint
july room members VNA
```

### Bước 5: Mở workspace tương tác

```bash
july
```

Nếu stdin và stdout đều là TTY, July mở full-screen TUI. Nếu một trong hai bị
pipe hoặc redirect, July dùng line REPL tương thích script.

### Bước 6: Tạo Work nhiều agent

Vào Room rồi gọi tên các agent cần làm việc cùng nhau:

```text
/room VNA
@cashpoint @pay implement refund flow
```

July tạo Work mới, đưa bạn vào luôn, và gửi prompt:

```text
work	<work-id>	implement refund flow
```

Gõ tiếp là đi vào cùng Work đó:

```text
> support partial refund too
```

Nếu một agent chưa phải thành viên Room, mention sẽ thêm nó và báo:

```text
member	pay	<room-id>
```

Không cần tạo Thread thủ công, không cần `/thread` trước khi gõ prompt.

## 6. Quản lý adapter và Agent

### Danh sách command Agent

```bash
july agent add <name> --project <path> --adapter <id> [--runtime <runtime>]
july agent add <name> --project <path> --transport <type> --config <file> [--runtime <runtime>]
july agent update <agent> --adapter <id>
july agent update <agent> --config <file>
july agent list
july agent show <agent>
july agent remove <agent>
```

Các command hữu hạn hỗ trợ `--json`; nên đặt ở đầu hoặc cuối command:

```bash
july --json agent list
july agent show cashpoint --json
```

Chỉ dùng `--json` một lần.

### Ý nghĩa của `agent remove`

`agent remove` chuyển Agent sang trạng thái inactive. Command này:

- không xóa Room;
- không xóa Thread hoặc transcript;
- không giải phóng tên Agent để tạo lại;
- không dùng để sửa cấu hình sai.

Để sửa adapter/config nhưng giữ nguyên logical Agent, dùng `agent update`.

### Cập nhật Agent sau khi nâng cấp adapter

Chạy lại `july setup` có thể nâng package adapter, nhưng không tự thay
`transport_config` đã lưu trong Agent. Sau khi nâng adapter, đồng bộ từng Agent:

```bash
july agent update cashpoint --adapter codex
```

Nếu bỏ qua bước này, ACP handshake có thể báo version hoặc identity mismatch.

### Dùng custom transport config

Đường `--adapter` là lựa chọn thông thường. Chỉ dùng `--transport` + `--config`
khi cần tự kiểm soát transport config.

ACP config JSON phải có đủ sáu field sau:

```json
{
  "executable": "/absolute/path/to/acp-adapter",
  "arguments": [],
  "environment": {},
  "state_directory": "/absolute/path/to/agent-state",
  "expected_agent_name": "expected-name",
  "expected_agent_version": "expected-version"
}
```

Tạo Agent từ config:

```bash
july agent add custom-agent \
  --project /absolute/path/to/project \
  --transport acp \
  --config /absolute/path/to/acp.json
```

Không kết hợp `--adapter` với `--transport` hoặc `--config`.

Custom ACP config không chấp nhận field ngoài sáu field trên. Ngoài ra:

- `executable`, project path và `state_directory` phải là absolute path tồn tại;
- `state_directory` phải ghi được;
- `arguments` chỉ chứa string và không được dùng moving specifier như
  `@latest`;
- `environment` chỉ chứa cặp key/value dạng string.

## 7. Quản lý Room

```bash
july room create <name> [--description <text>]
july room list
july room members <room>
july room member add <room> <agent>
july room member remove <room> <agent>
```

`<room>` nhận exact case-sensitive name hoặc canonical Room ID. `<agent>` nhận
exact case-sensitive name hoặc canonical Agent ID.

Ví dụ:

```bash
july room create Payments --description "Payment workstream"
july room member add Payments cashpoint
july room list --json
july room members Payments --json
```

Room membership và Work membership là hai trạng thái riêng. Agent ở trong Room
không tự động trở thành thành viên của mọi Work trong Room. Mention `@agent`
lo cả hai: agent chưa ở trong Room sẽ được thêm vào Room rồi vào Work.

## 8. Quản lý Thread (admin surface)

Thread là tên nội bộ của Work. Trong TUI/REPL bạn dùng `@agent` và `/work`;
phần dưới đây là command quản trị, dùng khi cần script hoá hoặc dựng sẵn
Work trước khi vào làm.

```bash
july thread create <title> --room <room> [--goal <text>] [--member <agent>]...
july thread list --room <room>
july thread members <thread-id>
july thread member add <thread-id> <agent>
july thread member remove <thread-id> <agent>
july thread open <thread-id> --agent <agent>
```

Ví dụ tạo Thread với nhiều Agent:

```bash
# Hai Agent phải tồn tại và đã là thành viên của Room.
july room member add Payments cashpoint
july room member add Payments pay

july thread create "Payment callback" \
  --room Payments \
  --goal "Chốt callback contract và triển khai" \
  --member cashpoint \
  --member pay
```

Điều kiện để mở Thread chat:

- Agent active;
- Room active;
- Thread open;
- Agent là active member của Room;
- Agent là active member của Thread.

Mở trực tiếp một Thread mà không qua workspace shell:

```bash
july thread open <thread-id> --agent cashpoint
```

`thread open` là interactive stream nên không hỗ trợ `--json`.
Nó dùng cùng input loop với `july dm`: `/exit`, `/quit`, EOF hoặc `Ctrl-C` khi
idle sẽ thoát; `Ctrl-C` trong active turn/permission chỉ gửi cancel rồi trở lại
prompt sau khi turn kết thúc.

`/thread` và `/thread new` vẫn chạy trong TUI/REPL nhưng đã bị ẩn khỏi `/help`:
`/thread new` chỉ tạo Thread với local user, không tự thêm Agent và không tự
chuyển context. Cách ngắn nhất để bắt đầu làm việc là `@agent` trong Room —
xem mục 5, bước 6.

## 9. Sử dụng TUI và REPL

### Chọn giao diện

| Cách chạy | Kết quả |
|---|---|
| `july` trong terminal | Full-screen TUI |
| `printf '/status\n/quit\n' \| july` | Line REPL |
| `july dm <agent>` | DM stream độc lập |
| `july thread open ...` | Work stream độc lập |
| Command quản trị | Output hữu hạn rồi thoát |

TUI và line REPL dùng cùng context model và cùng slash-command registry.

### Phím TUI

| Phím | Hành vi |
|---|---|
| `Enter` | Gửi input hiện tại |
| `Tab` | Hoàn thành tên agent đang gõ sau `@` |
| `Alt+Enter` | Xuống dòng trong editor |
| `PageUp` / `PageDown` | Cuộn transcript |
| `End` | Trở lại cuối transcript và bật follow-tail |
| `Esc` | Hủy thao tác theo context; không thoát July |
| `Ctrl-D` | Thoát khi turn idle và input trống |
| `Ctrl-C` khi input có chữ | Xóa input |
| `Ctrl-C` khi idle và input trống | Thoát TUI |
| `Ctrl-C` khi turn đang chạy | Gửi cancel |
| `Ctrl-C` lần nữa khi đang cancel | Thoát |

Khi permission modal xuất hiện:

- `↑` / `↓`: chọn option;
- `Enter`: chấp thuận option đang chọn;
- `Esc`: từ chối/cancel permission;
- `PageUp` / `PageDown`: cuộn nội dung modal;
- `Ctrl-C`: cancel turn đang active; nếu cancellation đã pending hoặc được xác
  nhận thì thoát TUI.

### Context model

```text
Root
├── Room
│   └── Work
└── Direct work (một agent)
```

`/back` chỉ quay lại context trước trong history. Nó không xóa membership,
không kết thúc Work và không xóa conversation.

### Gọi agent bằng `@`

`@` chỉ có nghĩa khi đứng ở **đầu** dòng. Phần còn lại của dòng là prompt.

| Bạn gõ | July làm gì |
|---|---|
| `@cashpoint fix callback retry` | Mở/nối việc trực tiếp với `cashpoint`, vào luôn, gửi prompt |
| `@cashpoint` | Vào việc trực tiếp, không gửi gì |
| `@cashpoint @pay implement refund flow` | Trong Room: tạo Work mới với cả hai, vào luôn, gửi prompt |
| `@pay @codex ...` khi đang ở đúng Work đó | Không tạo gì, prompt đi tiếp vào Work hiện tại |
| `@nobody hi` | Báo `agent nobody does not exist`, giữ nguyên context |

Quy tắc bổ sung:

- Thứ tự mention không quan trọng: `@a @b` và `@b @a` là cùng một tập agent.
- Mention lại đúng tập agent của context đang mở là **tiếp tục**, không tạo mới.
- Từ Room, mention luôn tạo Work mới — July không đoán rằng việc mới thuộc
  Work cũ.
- Nhiều agent bắt buộc phải ở trong Room. Ở Root sẽ báo
  `work with several agents needs a room; enter one with /room <room>`.
- Agent chưa là thành viên Room sẽ được thêm, và July in dòng `member ...`.
- Agent đầu tiên trong mention nhận turn; các agent còn lại vào Work làm
  thành viên.
- Text không bắt đầu bằng `@` hay `/`, khi đang ở trong một việc, được gửi
  nguyên văn cho agent.

### Slash commands

| Command | Context hợp lệ | Mục đích |
|---|---|---|
| `/room <room>` | mọi context | Vào Room |
| `/work` | Room | Liệt kê Work của Room |
| `/work <work-id> [--agent <agent>]` | Room, Work | Vào một Work |
| `/work` | Work | Liệt kê work item bên trong Work hiện tại |
| `/dm <agent>` | mọi context | Mở việc trực tiếp (tương đương `@agent` không prompt) |
| `/back` | mọi context | Quay lại context trước |
| `/rooms` | mọi context | Liệt kê Room |
| `/agents` | mọi context | Liệt kê Agent |
| `/members` | Room, Work | Liệt kê thành viên active |
| `/results` | Work | Liệt kê Result |
| `/status` | mọi context | Xem context hiện tại |
| `/publish <result> [--to <work>]` | Work | Publish Result |
| `/restart` | việc trực tiếp, Work | Đóng rồi mở lại binding của context hiện tại |
| `/help [command]` | mọi context | Xem help theo context |
| `/exit`, `/quit` | mọi context | Thoát July |

`/work <id>` xác nhận bằng `work\t<work-id>\t<agent>`; `/status` trong một Work
in `work\t<work-id>\t<agent>\t<binding-id>\t<trạng thái>`.

Hai command legacy vẫn chạy nhưng không còn xuất hiện trong `/help`:
`/thread <id> [--agent <agent>]` (tương đương `/work <id>`) và
`/thread new <title> [--goal <goal>]`. `/help thread` vẫn giải thích chúng.

### Quy tắc gửi message

- Trong một việc, input không khớp slash command và không mở đầu bằng `@`
  được gửi nguyên văn cho agent.
- Ở Room, text thường **không** bị từ chối: July liệt kê các agent trong Room
  kèm chính prompt đó đã gắn `@`, bạn chọn bằng cách gõ lại một dòng.

  ```text
  [vna] > investigate refund issue
  who should work on this?
    @cashpoint investigate refund issue
    @pay investigate refund issue
  ```

- Ở Root, text thường vẫn bị từ chối vì chưa có live conversation; hãy dùng
  `@agent` hoặc `/room`.
- Command dùng sai context trả lỗi rõ ràng, không tự đổi nghĩa.
- `/work <id>` không đoán agent nếu Work có zero hoặc nhiều active Agent;
  khi đó hãy thêm `--agent <name>`.

## 10. Work, Result và Publish

Chú ý `/work` có hai nghĩa theo context:

| Ở đâu | `/work` in ra |
|---|---|
| Trong Room | `<work-id>  <status>  <title>` — danh sách Work để vào |
| Trong một Work | `<item-id>  <status>  <title>  <owner>` — work item bên trong |

Hai ID này khác nhau: `/work <work-id>` dùng ID ở cột đầu của bảng trên,
không dùng item-id.

Mỗi Work được tạo cùng một primary work item; khi tạo bằng CLI,
`july thread create` trả về cả `<work-id>` lẫn `<primary-work-id>`. Trong
command surface hiện tại:

- `/work` chỉ đọc, không tạo và không đổi trạng thái;
- `/results` chỉ đọc Result;
- state transition của Work và việc tạo Result hiện chỉ có ở Rust application
  API; command surface và ACP/chat event surface hiện tại chưa expose mutation;
- chưa có top-level command để người dùng tạo Work hoặc Result thủ công.

Publish một Result từ CLI:

```bash
july publish <result-id> --to <target-thread-id>
```

Hoặc trong một Work:

```text
/publish <result-id>
```

Khi bỏ `--to` trong REPL:

- đúng một downstream Work được liên kết bởi Work dependency: tự chọn;
- không có downstream target: báo lỗi;
- nhiều target: bắt buộc dùng `--to <thread-id>`.

July không dùng transcript hoặc model call để đoán publish target.

## 11. Session, memory và recovery

July giữ logical Agent, conversation, message, Work, Result, checkpoint và
recovery metadata trong SQLite. Runtime session có thể bị disconnect hoặc mất
mà không làm mất logical identity.

Trong giao diện người dùng hiện tại:

- `/restart` detach rồi mở lại việc hiện tại; July ưu tiên resume binding và
  remote session đang có, nên command này không đảm bảo tạo provider session
  mới;
- mở lại một việc sẽ reuse hoặc resume binding khi có thể;
- khi remote session bị mất, July tạo replacement binding và gửi recovery
  capsule trước khi tiếp tục;
- recovery capsule được giao theo at-least-once, không cam kết exactly-once;
- July không replay toàn bộ transcript vào replacement session.

Memory promotion và checkpoint là capability của collaboration/application
layer, chưa có top-level `july memory ...`, `july checkpoint ...` hoặc
`july session ...` trong binary hiện tại.

## 12. Dùng `--json` cho automation

`--json` hỗ trợ các command hữu hạn sau:

- `--version`;
- `agent add/list/show/remove/update`;
- `room create/list/members/member add/member remove`;
- `thread create/list/members/member add/member remove`;
- `publish`.

Parser chấp nhận đúng một `--json` ở bất kỳ vị trí nào, nhưng nên đặt nhất quán
ngay sau `july` hoặc ở cuối command để script dễ đọc.

Ví dụ:

```bash
july --json agent list
july room list --json
july thread list --room Payments --json
july publish <result-id> --to <thread-id> --json
```

Các output chính:

```json
{"agent_id":"...","name":"cashpoint","project_root":"/path/to/cashpoint","transport_type":"acp","runtime":"codex","status":"active","created_at":"...","updated_at":"..."}
```

```json
{"room_id":"..."}
```

```json
{"thread_id":"...","primary_work_id":"..."}
```

Khi thành công, JSON được ghi ra stdout và process exit `0`. Khi lỗi, process
exit `1`, JSON error được ghi ra stderr theo dạng:

```json
{"error":{"code":"room_not_found","message":"..."}}
```

Shape để viết script:

| Command | JSON success |
|---|---|
| `--version` | object: `name`, `version` |
| `agent add/show/update/remove` | Agent object như ví dụ trên |
| `agent list` | array của Agent object |
| `room create` | object: `room_id` |
| `room list` | array: `room_id`, `name`, `description`, `status`, `created_at`, `updated_at` |
| `room members` | array: `room_id`, `agent_id`, `role`, `generation`, `joined_at`, `left_at`, `state` |
| `room member add/remove` | object: `state`, `changed` |
| `thread create` | object: `thread_id`, `primary_work_id` |
| `thread list` | array: `thread_id`, `room_id`, `title`, `goal`, `status`, `created_at`, `updated_at` |
| `thread members` | array: `thread_id`, `member_type`, `member_id`, `generation`, `joined_at`, `left_at`, `state` |
| `thread member add/remove` | object: `state`, `changed` |
| `publish` | object: `publish_id`, `result_id`, `source_conversation_id`, `target_conversation_id`, `published_at` |

Không dùng `--json` với:

- `july init`;
- `july setup`;
- `july` không đối số;
- `july dm`;
- `july thread open`.

## 13. Troubleshooting

### `usage: july dm <agent>` khi chạy `july --help`

Binary hiện chưa có top-level help. Chạy:

```bash
july
```

Sau đó dùng:

```text
/help
/help work
```

### `agent dùng transport acp cần --adapter ...`

Bạn mới truyền `--runtime` hoặc thiếu adapter config. Chạy:

```bash
july setup --adapters codex
july agent add <name> --project <path> --adapter codex
```

### Adapter chưa được cài hoặc chưa xác minh

Chạy lại:

```bash
july setup --adapters <adapter-id>
```

Kiểm tra npm có trên `PATH` với adapter npm, hoặc Cargo có trên `PATH` với
`claude-rust`.

### `authentication required`

Đăng nhập vào provider/runtime tương ứng ngoài July, sau đó mở lại việc.
`july setup` xác minh ACP identity và protocol; nó không đăng nhập tài khoản model
thay bạn.

### Adapter identity/version mismatch

Nếu vừa chạy lại `july setup`, cập nhật Agent đang dùng adapter đó:

```bash
july agent update <agent> --adapter <adapter-id>
```

### Agent inactive

`agent remove` là retire, không phải delete có thể hoàn tác. Tạo Agent khác với
tên mới, hoặc khôi phục dữ liệu bằng quy trình quản trị ngoài command surface
hiện tại.

### Không vào được Work

Kiểm tra theo thứ tự:

```bash
july room members <room>
july thread members <work-id>
```

Nếu thiếu membership:

```bash
july room member add <room> <agent>
july thread member add <work-id> <agent>
```

Trong workspace shell, phải vào đúng Room trước:

```text
/room <room>
/work <work-id> --agent <agent>
```

Với Work tạo bằng `@agent`, membership đã được lo sẵn.

### Command hợp lệ nhưng báo unavailable

Slash command bị giới hạn theo context. Dùng `/status` để xem context và
`/help` để xem command hợp lệ tại đó.

### TUI không mở

TUI chỉ mở khi cả stdin và stdout đều là TTY. Pipe hoặc redirect sẽ chọn line
REPL. Dùng `july` trực tiếp trong terminal để mở TUI.

### Cần kiểm tra database khác mà không đụng dữ liệu thật

```bash
JULY_HOME=/tmp/july-test-home \
JULY_WORKSPACE_DB=/tmp/july-test.db \
july agent list --json
```

## 14. Giới hạn của phiên bản hiện tại

Command surface hiện tại không có:

- daemon/background service;
- mouse, theme, plugin hoặc syntax highlighting trong TUI;
- semantic/vector memory;
- full-transcript recovery;
- top-level `work`, `result`, `memory`, `checkpoint` hoặc `session` commands;
- tự động publish hoặc tự đoán target bằng LLM;
- shared transcript giữa các Work.

Các giới hạn này là chủ ý của phiên bản hiện tại, không phải bước cấu hình còn
thiếu.

Một giới hạn của giao diện, không phải chủ ý: plain prompt trong Room in ra
danh sách gợi ý chứ chưa phải selector bấm chọn được; bạn chọn bằng cách gõ
lại một dòng.

## 15. Cheat sheet

```bash
# Onboarding
july setup --adapters codex
july agent add cashpoint --project /path/to/cashpoint --adapter codex

# Agent
july agent list
july agent show cashpoint
july agent update cashpoint --adapter codex

# Room
july room create VNA --description "VNA collaboration"
july room member add VNA cashpoint

# Work (admin surface)
july thread create "Refund flow" --room VNA --member cashpoint
july thread list --room VNA
july thread members <work-id>

# Chat
july
july dm cashpoint
july thread open <work-id> --agent cashpoint

# Automation
july --json agent list
july room list --json
july thread list --room VNA --json
```

```text
# Trong TUI/REPL — việc thường ngày
@cashpoint fix callback retry              một agent
/room VNA
@cashpoint @pay implement refund flow      nhiều agent, tạo Work
support partial refund too                 gõ tiếp vào Work đó

# Điều hướng và tra cứu
/help
/agents
/rooms
/room VNA
/work                                      list Work của Room
/work <work-id>                            vào một Work
/members
/results
/status
/restart
/back
/exit
```
