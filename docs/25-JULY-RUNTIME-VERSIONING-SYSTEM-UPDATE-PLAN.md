# JULY WORKSPACE — RUNTIME VERSIONING & SYSTEM UPDATE PLAN

## 1. Mục tiêu

Bổ sung hai capability liên quan nhưng tách trách nhiệm rõ ràng:

1. **SemVer-aware runtime/adapter version management**
   - July reuse adapter đã có trên máy nếu version hiện tại thỏa requirement.
   - July không downgrade hoặc reinstall vô ích một compatible version.
   - Khi cần cài/upgrade, July dùng một `install_version` đã được July release đó test.

2. **`july update` — system update command**
   - Cập nhật July lên latest stable release.
   - Chạy migration/config reconciliation cần thiết.
   - Load runtime/adapter specs mới từ phiên bản July mới.
   - Reconcile adapter/runtime versions theo SemVer.
   - Preserve user-owned configuration và workspace state.

Primary principle:

> **July release defines the compatible system state.**
>
> User-installed compatible runtimes should be reused.
>
> Incompatible or too-old managed components should be reconciled to a July-tested version.

---

## 2. Scope

Plan này xử lý:

```text
Adapter/runtime version requirements
SemVer comparison
existing installation reuse
managed install/upgrade
July self-update
GitHub Releases discovery
release asset selection
download verification
atomic binary replacement
schema/config migration
system reconciliation
runtime/adapter reconciliation
update status/reporting
```

Không xử lý:

```text
A2A collaboration semantics
Room/Work redesign
agent prompt changes
model routing
plugin marketplace
arbitrary dependency auto-update
beta/nightly channels trong MVP
```

---

## 3. Không giả định vị trí hiện tại của `AdapterSpec`

Hiện đã biết repo có một definition dạng:

```rust
AdapterSpec {
    id: "codex",
    package: "@agentclientprotocol/codex-acp",
    version: "1.6.2",
    bin: "codex-acp",
    installer: Npm,
    tier: Core,
    summary: "Codex qua @agentclientprotocol/codex-acp",
}
```

Nhưng plan này **không giả định file/module cụ thể**.

Implementation phải bắt đầu bằng audit.

Tìm trong repo:

```text
AdapterSpec
"@agentclientprotocol/codex-acp"
version: "1.6.2"
installer: Npm
tier: Core
runtime install
adapter install
setup
Command::new
codex-acp
```

Xác định:

```text
1. AdapterSpec được khai báo ở đâu?
2. Nó là compile-time static data hay load từ config/file?
3. Installer đọc spec từ đâu?
4. Setup command dùng spec nào?
5. Runtime launcher resolve binary ra sao?
6. Có managed install directory riêng hay dùng system PATH?
7. Existing version detection hiện hoạt động thế nào?
```

Không refactor trước khi trả lời được 7 câu hỏi này.

---

## 4. Target `AdapterSpec`

Chỉ cần hai version fields:

```rust
struct AdapterSpec {
    id: ...,
    package: ...,

    version_req: ...,
    install_version: ...,

    bin: ...,
    installer: ...,
    tier: ...,
    summary: ...,
}
```

Ví dụ Codex:

```rust
AdapterSpec {
    id: "codex",
    package: "@agentclientprotocol/codex-acp",

    version_req: ">=1.10.0, <2.0.0",
    install_version: "1.10.0",

    bin: "codex-acp",
    installer: Npm,
    tier: Core,
    summary: "Codex qua @agentclientprotocol/codex-acp",
}
```

Tên type cụ thể có thể thay đổi theo codebase hiện tại.

Không tạo abstraction mới nếu existing runtime spec có thể mở rộng trực tiếp.

---

## 5. Ý nghĩa `version_req`

`version_req` trả lời:

> **Version đã có trên máy có được July chấp nhận không?**

Ví dụ:

```text
version_req = >=1.10.0 <2.0.0
```

Behavior:

```text
installed 1.10.0
→ compatible
→ reuse

installed 1.12.4
→ compatible
→ reuse

installed 1.9.9
→ incompatible / too old
→ reconcile

installed 2.0.0
→ incompatible
→ do not silently downgrade
```

July không được dùng exact equality để quyết định runtime usable.

---

## 6. Ý nghĩa `install_version`

`install_version` trả lời:

> **Nếu July phải cài hoặc upgrade component này, version nào July release hiện tại muốn cài?**

Ví dụ:

```text
install_version = 1.10.0
```

Policy:

```text
missing
→ install 1.10.0

installed 1.6.2
→ install/upgrade to 1.10.0

installed 1.12.0
→ keep 1.12.0 because it satisfies version_req
```

Không dùng:

```text
npm install package@latest
```

mặc định.

Lý do:

```text
latest dependency
!=
version tested with current July release
```

---

## 7. SemVer implementation

Rust dependency:

```toml
semver = "1"
```

Conceptual:

```rust
use semver::{Version, VersionReq};

let req = VersionReq::parse(spec.version_req)?;
let installed = Version::parse(detected_version)?;

if req.matches(&installed) {
    // keep existing installation
} else {
    // reconcile
}
```

Version parsing cần normalize output của từng binary.

Ví dụ:

```text
@agentclientprotocol/codex-acp 1.10.0
→ 1.10.0

codex-cli 0.153.4
→ 0.153.4
```

Không parse bằng string lexical comparison.

---

## 8. Existing installation detection

Source of truth phải là executable July thực sự sẽ launch.

Flow:

```text
resolve binary
    ↓
run `<bin> --version`
    ↓
parse semantic version
```

Không dùng duy nhất:

```text
npm list -g
brew list
package-manager database
```

vì PATH và package-manager prefix có thể khác nhau.

Case đã gặp:

```text
npm package location A
PATH binary location B
```

Do đó:

> **Resolved executable + its reported version is canonical runtime detection.**

Probe phải giới hạn thời gian và kích thước stdout/stderr; phân biệt missing, command failure và unparseable output. Không chạy probe không giới hạn mỗi lần UI redraw.

Package SemVer từ `--version` và identity từ ACP initialize là hai contract riêng. Giữ kiểm tra ACP identity/protocol. Existing agents lưu `expected_agent_name`/`expected_agent_version` trong transport config; chỉ cập nhật `identities.json` không cập nhật các expectations đó. Reconciliation phải xử lý expectations được July quản lý và kiểm chứng agent launch sau upgrade, không ghi đè custom config.

---

## 9. Binary resolution policy

Precedence cho executable:

```text
explicit executable trong agent config
→ nếu không có explicit config: managed binary khi tồn tại
→ nếu managed binary không tồn tại: PATH lookup
```

Explicit configured executable luôn được giữ; lỗi probe không cho phép tự chuyển sang binary khác. Với onboarding, detect/probe, record identity và launch phải dùng cùng executable đã chọn. Managed candidate bị broken/incompatible phải được report/reconcile theo ownership policy, không âm thầm đổi sang PATH candidate. Reuse resolver hiện có nếu đáp ứng contract này.

Ví dụ:

```text
codex-acp
→ /opt/homebrew/bin/codex-acp
```

Detection record conceptual:

```rust
DetectedAdapter {
    executable,
    version,
}
```

Không hardcode:

```text
/opt/homebrew
~/.nvm
/usr/local
```

---

## 10. Reconciliation states

Adapter check cần normalize về states đơn giản:

```text
Missing
Compatible
TooOld
Incompatible
Broken
UnknownVersion
```

Examples:

```text
no executable
→ Missing

1.12.0 satisfies >=1.10 <2
→ Compatible

1.6.2 does not satisfy >=1.10 <2
→ TooOld

2.0.0 does not satisfy <2
→ Incompatible

binary exists but --version fails
→ Broken

version output cannot parse
→ UnknownVersion
```

Không tự mutate với `Incompatible` major version nếu policy chưa rõ.

---

## 11. Reconciliation policy

Recommended MVP:

```text
Missing
→ install install_version

Compatible
→ keep

TooOld
→ upgrade to install_version

Incompatible
→ report and require explicit resolution

Broken
→ reinstall install_version if July manages it;
   otherwise report

UnknownVersion
→ report, do not blindly overwrite
```

Nếu existing implementation phân biệt managed/unmanaged install, giữ distinction đó.

Nếu chưa có, không bắt buộc thêm ownership model phức tạp trong phase đầu. Tuy nhiên, `--adapter` và `--config` hiện cùng lưu vào `transport_config` mà không có ownership marker: không suy ra quyền overwrite từ JSON shape hoặc đường dẫn. Khi không chứng minh được field do July quản lý, preserve và report để xử lý rõ ràng.

---

## 12. `july update` product contract

Command:

```bash
july update
```

Ý nghĩa:

> **Bring the local July installation and its managed system components into the compatible state defined by the latest stable July release.**

Không chỉ update binary.

Target flow:

```text
july update
    ↓
discover latest stable July release
    ↓
compare current July version
    ↓
update July binary if needed
    ↓
run migrations
    ↓
load new built-in system specs
    ↓
reconcile adapters/runtimes
    ↓
verify
    ↓
report
```

---

## 13. GitHub Releases là update source

MVP source:

```text
GitHub Releases
```

Target repository:

```text
July Workspace GitHub repository
```

Use latest stable published release.

Ignore by default:

```text
draft releases
prereleases
branch HEAD
main HEAD
nightly builds
```

Do not update directly from GitHub `main`.

---

## 14. Release discovery

`july update` cần:

```text
current_version
latest_stable_version
release assets
```

Conceptual response:

```text
Current: 0.8.1
Latest:  0.9.0
```

Compare bằng SemVer.

Cases:

```text
latest > current
→ self-update

latest == current
→ skip binary update
→ still reconcile system components

latest < current
→ current may be development/newer build
→ do not downgrade automatically
```

Điểm quan trọng:

> **`july update` vẫn chạy runtime reconciliation ngay cả khi July đã là latest.**

Đây là cách fix những case adapter environment bị stale.

---

## 15. Release asset strategy

Packaging hiện tại ở `scripts/release.sh` dùng Cargo version không có tiền tố `v` trong tên asset, mặc định hai target macOS:

```text
july-0.9.0-aarch64-apple-darwin.tar.gz
july-0.9.0-x86_64-apple-darwin.tar.gz
SHA256SUMS
```

Tag release có thể dùng `v`; normalize tag riêng, không thêm `v` vào asset filename. Linux/target khác chỉ được hỗ trợ khi producer tạo và kiểm chứng artifact tương ứng. Repo hiện chưa có workflow publish release; publish stable release với assets/checksums đúng contract là prerequisite cho live updater, không coi local packaging là bằng chứng release đã tồn tại.

Updater detect:

```text
OS
architecture
```

rồi chọn đúng asset.

Không hardcode một binary asset duy nhất cho mọi platform.

---

## 16. Download verification

Không replace executable trước khi verify.

Minimum:

```text
download
→ compute SHA-256
→ compare expected digest/checksum
```

Release nên có:

```text
SHA256SUMS
```

hoặc dùng release asset digest nếu update implementation/API hiện tại hỗ trợ đủ tin cậy.

Mismatch:

```text
abort update
keep old installation
```

---

## 17. Atomic self-update

Không overwrite executable đang chạy trực tiếp theo cách có thể để lại binary hỏng.

Conceptual:

```text
download July.new
    ↓
verify
    ↓
prepare replacement
    ↓
atomic rename/swap
```

Platform-specific details được implement theo OS.

Failure phải để current July usable.

---

## 18. Self-update bootstrap problem

Một binary đang chạy tự replace có platform differences.

Audit current installation methods:

```text
cargo install?
release binary?
homebrew?
npm wrapper?
manual copy?
```

Không assume một installation mode.

MVP recommendation nếu July hiện chủ yếu là standalone binary:

```text
self-update standalone release binary
```

Nếu detected installation thuộc package manager:

```text
Homebrew / cargo-installed / other managed installation
```

thì có thể:

```text
report recommended package-manager update command
```

thay vì tự overwrite file mà package manager sở hữu.

Nếu repo hiện đã có canonical install method, ưu tiên method đó.

---

## 19. New July release owns new AdapterSpecs

Preferred design:

```text
AdapterSpecs compile/load as part of July release
```

Không cần một remote mutable adapter manifest ở MVP.

Ví dụ:

```text
July 0.8.0
codex:
  version_req >=1.6 <2
  install_version 1.6.2
```

Release sau:

```text
July 0.9.0
codex:
  version_req >=1.10 <2
  install_version 1.10.0
```

Flow:

```text
july update
→ install July 0.9.0
→ new binary/spec data becomes active
→ reconcile codex
```

Implementation audit phải xác định `AdapterSpec` hiện là compile-time hay runtime config.

Nếu compile-time static:

```text
keep it simple
```

Không chuyển sang remote config chỉ để làm updater.

---

## 20. Update order

Recommended:

```text
1. discover
2. plan
3. download
4. verify
5. install new July
6. hand control to new July binary
7. run supported schema/config migration
8. load new system specs
9. reconcile adapters/runtimes
10. verify
11. report
```

Handoff cần một internal invocation contract ổn định giữa old/new binary, giữ update lock xuyên handoff và báo chính xác partial failure. Thay file executable không thay specs đang nằm trong old process. Already-latest vẫn chạy migration/reconciliation bằng binary hiện tại; không cần self-replacement.

Không update adapters theo **old specs** rồi mới update July.

New July release phải quyết định compatible component state.

---

## 21. Configuration ownership

Update không được overwrite user config tùy tiện.

Tách:

```text
System-owned defaults/specs
User-owned configuration
Workspace-owned state
```

System-owned có thể update:

```text
AdapterSpec
default runtime launcher behavior
protocol capability defaults
system schema
migration rules
```

User-owned phải preserve:

```text
agent name
project path
runtime preference
model override
custom instructions
user UI preferences
```

Workspace state preserve:

```text
Rooms
Room membership
Work
Results
Artifacts
Decisions
history
```

---

## 22. Config migration

July hiện chưa release; dữ liệu development cũ là test data có thể bỏ theo quyết định của Tony. Không xây historical/legacy migration cho các schema development đã bị loại, gồm pre-Phase9 Work schema. Nếu gặp DB development không được hỗ trợ, report rõ boundary và reset đúng DB test chỉ khi cần, với thông báo reset; không tự xóa dữ liệu trong slice đổi spec/plan này. Quyết định này không áp dụng cho dữ liệu người dùng sau release.

Sau release, giữ explicit migration cho các schema/config được hỗ trợ, ownership preservation và failure reporting. Migration hiện tại commit từng version riêng; lỗi ở version sau không rollback các version đã commit.

Nếu config schema thay đổi:

```text
schema N → N+1
```

phải có explicit migration.

Không:

```text
replace config file with new default
```

Pattern:

```text
read old config
→ migrate known fields
→ preserve unknown/user values where valid
→ write new schema safely
```

Backup trước destructive migration nếu cần.

---

## 23. Agent configuration update

User yêu cầu `july update` có thể cập nhật "cấu hình mới của các agent nếu có".

Phải phân biệt:

### System-managed agent integration

July có thể thay đổi:

```text
adapter requirement
launcher env
runtime flags
capability defaults
system-generated runtime config
```

Ví dụ future:

```text
Codex ACP launcher now sets CODEX_PATH
```

Đây là system behavior và có thể update.

### User-owned agent configuration

Không tự thay:

```text
cashpoint project path
cashpoint chosen model
cashpoint custom instructions
room membership
```

Nếu format thay đổi, migrate structure nhưng preserve intent/value.

---

## 24. Runtime reconciliation after update

Pseudo-flow:

```text
for spec in adapter_specs:

    detected = detect(spec.bin)

    match detected:
        Missing:
            install(spec.install_version)

        Compatible:
            keep

        TooOld:
            upgrade(spec.install_version)

        Incompatible:
            report

        Broken:
            repair if managed / report otherwise

        UnknownVersion:
            report
```

Sau mutation:

```text
detect again
verify version_req
```

Không assume install succeeded.

---

## 25. `july update` output UX

Example:

```text
July Update

Current July    0.8.1
Latest stable   0.9.0

Updating July
✓ Downloaded 0.9.0
✓ Verified release
✓ Installed July 0.9.0

Migrating
✓ Workspace schema 3 → 4
✓ Configuration reconciled

Runtimes
✓ codex-acp 1.12.0   compatible
↑ claude-acp 0.6.1 → 0.8.2
✓ other-runtime 1.4.0

July 0.9.0 is ready.
```

If no July update:

```text
July 0.9.0 is already up to date.

Reconciling runtimes
✓ codex-acp 1.12.0
✓ claude-acp 0.8.2

System is up to date.
```

---

## 26. Build an internal `UpdatePlan`

Updater nên xây plan trước mutation.

Conceptual:

```rust
UpdatePlan {
    july_update,
    migrations,
    adapter_actions,
}
```

Example:

```text
July:
  0.8.1 → 0.9.0

Adapters:
  codex   keep 1.12.0
  claude  0.6.1 → 0.8.2
```

Không nhất thiết expose `--dry-run` trong MVP, nhưng internal plan giúp:

```text
testing
logging
failure handling
deterministic execution
```

Optional future:

```bash
july update --check
```

---

## 27. Failure handling

System update không được biến partial failure thành silent success.

Example:

```text
July binary updated ✓
Migration ✓
codex-acp ✓
claude adapter failed ✗
```

Report:

```text
July was updated to 0.9.0.
One runtime requires attention:
- claude: installation failed
```

Không rollback toàn bộ July chỉ vì optional adapter failed nếu core vẫn usable.

Nhưng migration failure sau binary switch phải được xử lý cẩn thận.

---

## 28. Update locking

Không cho hai update process chạy cùng lúc.

Implement simple update lock:

```text
July update lock
```

Nếu already running:

```text
Another July update is in progress.
```

Không cần distributed lock.

---

## 29. Network behavior

`july update` là explicit network action do user gọi.

Nếu GitHub unavailable:

```text
Unable to check for July updates.
Current installation was not changed.
```

Adapter reconciliation local có thể vẫn chạy nếu implementation tách được an toàn.

Không background auto-update trong plan này.

---

## 30. Version source cho July itself

Build phải expose canonical current version.

Prefer:

```rust
env!("CARGO_PKG_VERSION")
```

hoặc existing build metadata.

Không duplicate version string ở nhiều nơi.

GitHub release tag normalize:

```text
v0.9.0
→ 0.9.0
```

rồi SemVer compare.

---

## 31. Release compatibility policy

MVP:

```text
stable releases only
```

No:

```text
beta
nightly
main
```

Future:

```text
july update --channel beta
```

có thể thêm sau nhưng không cần thiết hiện tại.

---

## 32. Security guardrails

Required:

```text
HTTPS only
expected GitHub repository only
stable published release only
asset verification
no arbitrary URL execution
no shell interpolation from release metadata
```

Installer arguments phải được truyền dạng process args, không concatenate untrusted shell command strings.

---

## 33. Implementation workstreams

### Workstream A — Audit current runtime spec architecture

Find:

```text
AdapterSpec
runtime registry
runtime installer
setup
adapter detection
version parsing
managed install paths
```

Deliver:

```text
documented current flow
exact files/modules to modify
```

### Workstream B — AdapterSpec SemVer model

Change:

```text
version
```

to:

```text
version_req
install_version
```

Update all specs.

Giữ nguyên install pins hiện tại. Theo policy conservative cho 0.x đã được Tony duyệt, Part 2 dùng các requirement sau (từ pin hiện tại đến trước minor kế tiếp):

| Adapter | version_req | install_version |
| --- | --- | --- |
| Codex | `>=1.10.0, <2.0.0` | `1.10.0` |
| Claude | `>=0.70.0, <0.71.0` | `0.70.0` |
| Claude Rust | `>=0.1.22, <0.2.0` | `0.1.22` |
| DeepSeek | `>=0.4.26, <0.5.0` | `0.4.26` |

Validate mỗi install_version thỏa version_req bằng SemVer; không coi mọi version 0.x tương thích. Các version cũ trong ví dụ minh họa không thay thế bảng pin này.

### Workstream C — Version detection

Implement/normalize:

```text
resolve executable
execute --version
parse SemVer
```

### Workstream D — Compatibility checker

Implement:

```text
VersionReq.matches(Version)
```

and normalized states.

### Workstream E — Runtime reconciler

Implement:

```text
detect
compare
keep/install/upgrade/report
verify
```

### Workstream F — Update command

Add:

```text
july update
```

to CLI registry/parser/help.

### Workstream G — GitHub release provider

Implement:

```text
latest stable release lookup
version normalization
asset selection
```

### Workstream H — Download + verification

Implement platform asset download and SHA-256 verification.

### Workstream I — Self-update

Implement safe replacement for supported install mode.

### Workstream J — Migration runner

New binary runs supported schema/config migrations after handoff. Không triển khai historical migration cho disposable pre-release test DB; áp dụng boundary/reset reporting ở section 22 khi cần. Future post-release data/config vẫn phải được preserve qua supported migrations.

### Workstream K — System reconciliation

New July version invokes runtime reconciliation against its new specs.

### Workstream L — UX and reporting

Render concise update progress/results.

---

## 34. Required tests — SemVer

```text
req >=1.10 <2
installed 1.10.0
→ Compatible

installed 1.10.1
→ Compatible

installed 1.99.0
→ Compatible

installed 1.9.9
→ TooOld

installed 2.0.0
→ Incompatible
```

Also test:

```text
pre-release versions
malformed output
missing binary
version command failure
```

---

## 35. Required tests — existing install

Given:

```text
PATH codex-acp = 1.12.0
install_version = 1.10.0
version_req >=1.10 <2
```

Verify:

```text
no install
no downgrade
reuse 1.12.0
```

This is a core acceptance case.

---

## 36. Required tests — July update

### New release exists

```text
current 0.8.0
latest 0.9.0
```

Verify:

```text
download
verify
install
migrate
reconcile
```

### Already latest

```text
current 0.9.0
latest 0.9.0
```

Verify:

```text
skip binary update
still reconcile runtimes
```

### Current newer

```text
current 0.10.0
latest 0.9.0
```

Verify:

```text
no downgrade
```

### Network failure

Verify:

```text
no local binary mutation
clear error
```

### Checksum mismatch

Verify:

```text
abort
old July remains usable
```

---

## 37. Required tests — config ownership

Update must preserve:

```text
agent definitions
project paths
model preferences
Room memberships
custom user config
workspace data
```

while allowing migration of:

```text
system schema
built-in runtime specs
system-managed launcher configuration
```

---

## 38. Acceptance criteria

```text
[ ] AdapterSpec no longer relies on exact installed version equality
[ ] AdapterSpec has version_req
[ ] AdapterSpec has install_version
[ ] SemVer parser is used
[ ] compatible newer installed version is reused
[ ] old incompatible version is upgraded to install_version
[ ] incompatible future major is not silently downgraded
[ ] runtime detection checks the executable July actually resolves
[ ] july update exists
[ ] july update checks latest stable GitHub Release
[ ] GitHub release tags are SemVer compared
[ ] correct platform asset is selected
[ ] downloaded release is verified
[ ] self-update does not corrupt current install on failure
[ ] new July specs drive post-update reconciliation
[ ] runtime reconciliation runs even when July itself is already latest
[ ] user-owned config is preserved
[ ] workspace state is preserved
[ ] migrations are explicit
[ ] runtime versions are verified again after update/install
```

---

## 39. Definition of Done

Starting state:

```text
July 0.8.0

codex-acp installed:
1.6.2

July 0.8.0 spec:
version_req = >=1.6 <2
install_version = 1.6.2
```

GitHub publishes:

```text
July 0.9.0
```

New built-in spec:

```text
codex-acp
version_req = >=1.10 <2
install_version = 1.10.0
```

User runs:

```bash
july update
```

Expected:

```text
July 0.8.0 → 0.9.0
config/schema migration succeeds

codex-acp 1.6.2
does not satisfy >=1.10 <2
→ upgraded to 1.10.0

post-update verification succeeds
```

Alternative machine:

```text
codex-acp already 1.12.0
```

Expected:

```text
July updates to 0.9.0
codex-acp 1.12.0 satisfies requirement
→ preserved
→ no downgrade
→ no reinstall
```

That behavior is the canonical Definition of Done.

---

## 40. Recommended final architecture

```text
                       JULY RELEASE

                 Built-in System Specs
                         │
              ┌──────────┴──────────┐
              │                     │
       AdapterSpec              Migrations
              │
      version_req
      install_version
              │
              ▼
      Runtime Reconciler
              │
       detect / SemVer
              │
      ┌───────┴────────┐
      ▼                ▼
 existing          install/upgrade
 compatible        tested version
 runtime

                       ▲
                       │
                  july update
                       │
             GitHub latest stable
                       │
         download / verify / install
                       │
              new July release
```

Core rules:

> **Update July first, then reconcile the system according to the compatibility rules shipped by that July release.**

> **Keep a compatible runtime already installed on the machine; install the July-tested version only when reconciliation is required.**
