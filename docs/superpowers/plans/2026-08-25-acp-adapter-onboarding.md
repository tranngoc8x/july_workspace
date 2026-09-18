# ACP Adapter Onboarding Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `july init` cho phép chọn và cài ACP adapter, sau đó `july agent add --adapter <id>` sinh `transport_config` đầy đủ, không bao giờ lưu được agent `acp` với config rỗng.

**Architecture:** Ba unit mới với ranh giới tách bạch - `adapter/catalog.rs` là danh mục tĩnh không biết filesystem, `adapter/store.rs` quản lý `~/.july/adapters` và sinh config nhưng không biết stdin, `cli/init.rs` vẽ màn hình và đọc phím nhưng không biết npm. Adapter luôn là subprocess nói ACP qua stdio; `src/transport/acp.rs` không mở thêm đường in-process nào. Việc hỏi danh tính adapter (`initialize` handshake) nằm trong `transport` để giữ nguyên bất biến "chỉ tầng transport nói ACP".

**Tech Stack:** Rust 2024 edition, rust-version 1.96, tokio, serde_json, agent-client-protocol `=2.0.0`, libc (dep mới duy nhất).

**Spec:** `docs/superpowers/specs/2026-08-25-acp-adapter-onboarding-design.md`

## Global Constraints

- Mọi dependency trong `Cargo.toml` pin tuyệt đối bằng `=`. Dep mới duy nhất được phép thêm: `libc = "=0.2.189"`.
- `src/transport/acp.rs` không được mở đường in-process cho adapter. Adapter luôn là subprocess.
- Module con khai báo `mod x;` private trong `mod.rs` rồi `pub use` những gì cần lộ ra, theo đúng `src/runtime/mod.rs` và `src/transport/mod.rs`.
- Test thuần logic đặt inline trong `mod tests` cùng file (theo `src/cli/registry.rs:333`). Test tích hợp đặt ở `tests/<tên>.rs`.
- Thư mục tạm trong test tạo bằng `std::env::temp_dir().join(format!("july-<scope>-{}", ulid::Ulid::generate()))`, theo `tests/cli_publish.rs:23`.
- Màn hình tương tác chỉ chạy trên unix. Khi stdin không phải TTY thì không vào raw mode.
- Mọi thông điệp hướng tới người dùng viết tiếng Việt, dùng dấu `-` chứ không dùng `—`.
- Gate chất lượng sau mỗi task: `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.

---

## Bảng file

| File                                     | Trách nhiệm                                                                                          |
| ---------------------------------------- | ---------------------------------------------------------------------------------------------------- |
| `src/adapter/mod.rs` (tạo)               | Khai báo submodule, `pub use`, `AdapterError`                                                        |
| `src/adapter/catalog.rs` (tạo)           | `AdapterSpec`, `ADAPTERS`, `find()`. Không biết filesystem                                           |
| `src/adapter/store.rs` (tạo)             | `AdapterStore`: đường dẫn, phiên bản đã cài, `identities.json`, `config_for()`, cài đặt              |
| `src/cli/keys.rs` (tạo)                  | `RawMode` guard qua termios, `decode()` bytes sang `Key`                                             |
| `src/cli/init.rs` (tạo)                  | `Selection` state machine, vẽ màn hình, luồng `july init`                                            |
| `src/lib.rs` (sửa)                       | Thêm `pub mod adapter;`                                                                              |
| `src/transport/mod.rs` (sửa)             | Thêm `AgentIdentity`, `pub use acp::probe_agent_identity`                                            |
| `src/transport/acp.rs` (sửa)             | Thêm `probe_agent_identity()`                                                                        |
| `src/runtime/mod.rs` (sửa)               | `pub(crate) use direct_message::parse_acp_config`                                                    |
| `src/runtime/direct_message.rs` (sửa)    | `parse_acp_config` thành `pub(crate)`                                                                |
| `src/cli/mod.rs` (sửa)                   | `Command::Init`, `AgentOperation::Add.adapter`, `AgentOperation::Update`, `CliError::Adapter`, USAGE |
| `src/application/collaboration.rs` (sửa) | `UpdateAgent` command cho `agent update`                                                             |
| `docs/08-RUNTIME-AND-CLI.md` (sửa)       | Tài liệu `init`, `agent add --adapter`, `agent update`                                               |
| `tests/adapter_probe.rs` (tạo)           | Probe danh tính adapter qua fixture python                                                           |
| `tests/cli_init.rs` (tạo)                | `july init` ở chế độ không TTY                                                                       |

---

### Task 1: Danh mục adapter

**Files:**

- Create: `src/adapter/mod.rs`
- Create: `src/adapter/catalog.rs`
- Modify: `src/lib.rs`

**Interfaces:**

- Consumes: không có.
- Produces: `adapter::{AdapterSpec, Installer, Tier, ADAPTERS, find}`. `AdapterSpec` có các field `id: &'static str`, `package: &'static str`, `version: &'static str`, `bin: &'static str`, `installer: Installer`, `tier: Tier`, `summary: &'static str`. `find(id: &str) -> Option<&'static AdapterSpec>`.

- [ ] **Step 1: Viết test thất bại**

Tạo `src/adapter/catalog.rs` chỉ với phần test:

```rust
//! Static catalog of supported ACP adapters.

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn every_adapter_has_complete_metadata() {
        for spec in ADAPTERS {
            assert!(!spec.id.is_empty(), "adapter without id");
            assert!(!spec.package.trim().is_empty(), "{} lacks package", spec.id);
            assert!(!spec.version.trim().is_empty(), "{} lacks version", spec.id);
            assert!(!spec.bin.trim().is_empty(), "{} lacks bin", spec.id);
            assert!(!spec.summary.trim().is_empty(), "{} lacks summary", spec.id);
        }
    }

    #[test]
    fn adapter_ids_are_unique() {
        let mut seen = HashSet::new();
        for spec in ADAPTERS {
            assert!(seen.insert(spec.id), "duplicate adapter id {}", spec.id);
        }
    }

    #[test]
    fn versions_are_pinned_without_range_operators() {
        for spec in ADAPTERS {
            assert!(
                spec.version
                    .chars()
                    .all(|character| character.is_ascii_digit() || character == '.'),
                "{} version must be an exact pin, got {}",
                spec.id,
                spec.version
            );
        }
    }

    #[test]
    fn core_adapters_exist_and_are_findable() {
        let core: Vec<&str> = ADAPTERS
            .iter()
            .filter(|spec| matches!(spec.tier, Tier::Core))
            .map(|spec| spec.id)
            .collect();
        assert_eq!(core, vec!["codex", "claude"]);
        for id in core {
            assert!(find(id).is_some(), "{id} not findable");
        }
    }

    #[test]
    fn find_rejects_an_unknown_id() {
        assert!(find("acp").is_none());
        assert!(find("").is_none());
    }
}
```

- [ ] **Step 2: Chạy test để xác nhận thất bại**

Run: `cargo test --lib adapter::catalog`
Expected: FAIL khi biên dịch, `cannot find value ADAPTERS in this scope`.

- [ ] **Step 3: Viết implementation nhỏ nhất**

Thêm vào đầu `src/adapter/catalog.rs`, phía trên `mod tests`:

```rust
use Installer::{Cargo, Npm};
use Tier::{Core, Optional};

/// Trình cài đặt phát hành adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Installer {
    Npm,
    Cargo,
}

/// Adapter `Core` được tick sẵn ở màn hình onboarding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Tier {
    Core,
    Optional,
}

/// Một adapter được hỗ trợ. Thêm adapter mới là thêm một entry vào `ADAPTERS`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdapterSpec {
    /// Định danh người dùng gõ, ví dụ `codex`.
    pub id: &'static str,
    /// Tên package trên registry của trình cài.
    pub package: &'static str,
    /// Phiên bản pin chính xác.
    pub version: &'static str,
    /// Tên file thực thi mà trình cài đặt sinh ra.
    pub bin: &'static str,
    pub installer: Installer,
    pub tier: Tier,
    pub summary: &'static str,
}

pub const ADAPTERS: &[AdapterSpec] = &[
    AdapterSpec {
        id: "codex",
        package: "@agentclientprotocol/codex-acp",
        version: "1.6.2",
        bin: "codex-acp",
        installer: Npm,
        tier: Core,
        summary: "Codex qua @agentclientprotocol/codex-acp",
    },
    AdapterSpec {
        id: "claude",
        package: "@agentclientprotocol/claude-agent-acp",
        version: "0.70.0",
        bin: "claude-agent-acp",
        installer: Npm,
        tier: Core,
        summary: "Claude Code qua @agentclientprotocol/claude-agent-acp",
    },
    AdapterSpec {
        id: "claude-rust",
        package: "claude-code-acp-rs",
        version: "0.1.22",
        bin: "claude-code-acp-rs",
        installer: Cargo,
        tier: Optional,
        summary: "Claude Code bản Rust, không cần node (biên dịch ~2 phút)",
    },
    AdapterSpec {
        id: "deepseek",
        package: "@openma/deepseek-harness-acp",
        version: "0.4.26",
        bin: "deepseek-harness-acp",
        installer: Npm,
        tier: Optional,
        summary: "DeepSeek Harness (experimental 0.4.x)",
    },
];

/// Tra adapter theo id người dùng gõ.
pub fn find(id: &str) -> Option<&'static AdapterSpec> {
    ADAPTERS.iter().find(|spec| spec.id == id)
}
```

Tạo `src/adapter/mod.rs`:

```rust
//! Boundary cho danh mục và vòng đời của các ACP adapter cài trên máy.

mod catalog;

pub use catalog::{ADAPTERS, AdapterSpec, Installer, Tier, find};
```

Thêm vào `src/lib.rs`, ngay sau khối `pub mod application;`:

```rust
/// Danh mục và vòng đời của các ACP adapter cài trên máy.
pub mod adapter;
```

- [ ] **Step 4: Chạy test để xác nhận pass**

Run: `cargo test --lib adapter::catalog`
Expected: PASS, 5 test.

- [ ] **Step 5: Gate chất lượng**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings`
Expected: không có cảnh báo.

- [ ] **Step 6: Commit**

```bash
git add src/adapter/mod.rs src/adapter/catalog.rs src/lib.rs
git commit -m "feat(adapter): add static catalog of supported ACP adapters"
```

---

### Task 2: Đường dẫn và phiên bản đã cài

**Files:**

- Create: `src/adapter/store.rs`
- Modify: `src/adapter/mod.rs`

**Interfaces:**

- Consumes: `adapter::{AdapterSpec, Installer, find}` từ Task 1.
- Produces:
  - `AdapterError` với các variant `MissingHome`, `UnknownAdapter(String)`, `NotInstalled { id: String }`, `NotVerified { id: String }`, `ToolMissing { tool: &'static str }`, `InstallFailed { id: String, tool: &'static str, status: String }`, `Probe(String)`, `Io(io::Error)`, `Json(serde_json::Error)`.
  - `AdapterStore::new(home: PathBuf) -> Self`, `AdapterStore::open_default() -> Result<Self, AdapterError>`, `adapters_root(&self) -> PathBuf`, `state_root(&self) -> PathBuf`, `bin_path(&self, spec: &AdapterSpec) -> PathBuf`, `installed_version(&self, spec: &AdapterSpec) -> Option<String>`.

- [ ] **Step 1: Viết test thất bại**

Tạo `src/adapter/store.rs` với phần test:

```rust
//! Quản lý các adapter đã cài dưới `~/.july/adapters`.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::find;

    fn scratch() -> PathBuf {
        let path = std::env::temp_dir().join(format!("july-adapter-store-{}", ulid::Ulid::generate()));
        std::fs::create_dir_all(&path).expect("scratch dir");
        path
    }

    #[test]
    fn npm_bin_path_lives_under_node_modules() {
        let home = scratch();
        let store = AdapterStore::new(home.clone());
        let spec = find("codex").expect("codex in catalog");

        assert_eq!(
            store.bin_path(spec),
            home.join("adapters/node_modules/.bin/codex-acp")
        );
    }

    #[test]
    fn cargo_bin_path_lives_under_bin() {
        let home = scratch();
        let store = AdapterStore::new(home.clone());
        let spec = find("claude-rust").expect("claude-rust in catalog");

        assert_eq!(
            store.bin_path(spec),
            home.join("adapters/bin/claude-code-acp-rs")
        );
    }

    #[test]
    fn state_root_sits_beside_adapters() {
        let home = scratch();
        let store = AdapterStore::new(home.clone());

        assert_eq!(store.state_root(), home.join("state"));
        assert_eq!(store.adapters_root(), home.join("adapters"));
    }

    #[test]
    fn installed_version_is_none_when_nothing_is_installed() {
        let store = AdapterStore::new(scratch());

        assert_eq!(store.installed_version(find("codex").expect("codex")), None);
    }

    #[test]
    fn installed_version_reads_the_npm_package_manifest() {
        let home = scratch();
        let store = AdapterStore::new(home.clone());
        let spec = find("codex").expect("codex");
        let manifest = home.join("adapters/node_modules/@agentclientprotocol/codex-acp");
        std::fs::create_dir_all(&manifest).expect("manifest dir");
        std::fs::write(
            manifest.join("package.json"),
            r#"{"name":"@agentclientprotocol/codex-acp","version":"1.1.13"}"#,
        )
        .expect("write manifest");

        assert_eq!(store.installed_version(spec), Some("1.1.13".into()));
    }

    #[test]
    fn installed_version_reads_the_cargo_root_manifest() {
        let home = scratch();
        let store = AdapterStore::new(home.clone());
        let spec = find("claude-rust").expect("claude-rust");
        let root = home.join("adapters");
        std::fs::create_dir_all(&root).expect("adapters dir");
        std::fs::write(
            root.join(".crates2.json"),
            r#"{"installs":{"claude-code-acp-rs 0.1.22 (registry+https://github.com/rust-lang/crates.io-index)":{"bins":["claude-code-acp-rs"]}}}"#,
        )
        .expect("write crates2");

        assert_eq!(store.installed_version(spec), Some("0.1.22".into()));
    }

    #[test]
    fn installed_version_ignores_a_malformed_manifest() {
        let home = scratch();
        let store = AdapterStore::new(home.clone());
        let spec = find("codex").expect("codex");
        let manifest = home.join("adapters/node_modules/@agentclientprotocol/codex-acp");
        std::fs::create_dir_all(&manifest).expect("manifest dir");
        std::fs::write(manifest.join("package.json"), "{ not json").expect("write manifest");

        assert_eq!(store.installed_version(spec), None);
    }
}
```

- [ ] **Step 2: Chạy test để xác nhận thất bại**

Run: `cargo test --lib adapter::store`
Expected: FAIL khi biên dịch, `cannot find type AdapterStore`.

- [ ] **Step 3: Viết implementation nhỏ nhất**

Thêm phía trên `mod tests` trong `src/adapter/store.rs`:

```rust
use super::{AdapterSpec, Installer};
use serde_json::Value;
use std::io;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("environment variable HOME is not set")]
    MissingHome,
    #[error("adapter `{0}` không có trong danh mục; xem `july init` để biết danh sách")]
    UnknownAdapter(String),
    #[error("adapter `{id}` chưa được cài; chạy `july init` để cài")]
    NotInstalled { id: String },
    #[error("adapter `{id}` đã cài nhưng chưa xác minh được danh tính; chạy lại `july init`")]
    NotVerified { id: String },
    #[error("không tìm thấy `{tool}` trên PATH; cài `{tool}` rồi chạy lại `july init`")]
    ToolMissing { tool: &'static str },
    #[error("cài adapter `{id}` thất bại: `{tool}` kết thúc với {status}")]
    InstallFailed {
        id: String,
        tool: &'static str,
        status: String,
    },
    #[error("không xác minh được danh tính adapter: {0}")]
    Probe(String),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

/// Sở hữu bố cục thư mục `~/.july`: adapter cài ở `adapters/`, state agent ở `state/`.
pub struct AdapterStore {
    home: PathBuf,
}

impl AdapterStore {
    pub fn new(home: PathBuf) -> Self {
        Self { home }
    }

    /// `~/.july`, hoặc `JULY_HOME` nếu được đặt.
    pub fn open_default() -> Result<Self, AdapterError> {
        if let Some(home) = std::env::var_os("JULY_HOME") {
            return Ok(Self::new(PathBuf::from(home)));
        }
        let home = std::env::var_os("HOME").ok_or(AdapterError::MissingHome)?;
        Ok(Self::new(PathBuf::from(home).join(".july")))
    }

    pub fn adapters_root(&self) -> PathBuf {
        self.home.join("adapters")
    }

    pub fn state_root(&self) -> PathBuf {
        self.home.join("state")
    }

    /// Đường dẫn tuyệt đối tới file thực thi mà trình cài sinh ra.
    pub fn bin_path(&self, spec: &AdapterSpec) -> PathBuf {
        match spec.installer {
            Installer::Npm => self
                .adapters_root()
                .join("node_modules/.bin")
                .join(spec.bin),
            Installer::Cargo => self.adapters_root().join("bin").join(spec.bin),
        }
    }

    /// Phiên bản đang cài, đọc từ metadata của chính trình cài. Manifest hỏng coi như chưa cài.
    pub fn installed_version(&self, spec: &AdapterSpec) -> Option<String> {
        match spec.installer {
            Installer::Npm => self.npm_version(spec),
            Installer::Cargo => self.cargo_version(spec),
        }
    }

    fn npm_version(&self, spec: &AdapterSpec) -> Option<String> {
        let manifest = self
            .adapters_root()
            .join("node_modules")
            .join(spec.package)
            .join("package.json");
        let parsed: Value = serde_json::from_str(&std::fs::read_to_string(manifest).ok()?).ok()?;
        Some(parsed.get("version")?.as_str()?.to_owned())
    }

    fn cargo_version(&self, spec: &AdapterSpec) -> Option<String> {
        let manifest = self.adapters_root().join(".crates2.json");
        let parsed: Value = serde_json::from_str(&std::fs::read_to_string(manifest).ok()?).ok()?;
        parsed
            .get("installs")?
            .as_object()?
            .keys()
            .filter_map(|key| {
                let mut parts = key.split_whitespace();
                (parts.next()? == spec.package).then(|| parts.next()?.to_owned())
            })
            .next()
    }
}

/// Thư mục state của một agent, july tự tạo trước khi adapter chạy.
pub fn ensure_state_directory(root: &Path, agent_name: &str) -> Result<PathBuf, AdapterError> {
    let directory = root.join(agent_name);
    std::fs::create_dir_all(&directory)?;
    Ok(directory)
}
```

Cập nhật `src/adapter/mod.rs`:

```rust
//! Boundary cho danh mục và vòng đời của các ACP adapter cài trên máy.

mod catalog;
mod store;

pub use catalog::{ADAPTERS, AdapterSpec, Installer, Tier, find};
pub use store::{AdapterError, AdapterStore, ensure_state_directory};
```

- [ ] **Step 4: Chạy test để xác nhận pass**

Run: `cargo test --lib adapter::store`
Expected: PASS, 7 test.

- [ ] **Step 5: Gate chất lượng**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: không cảnh báo, toàn bộ test pass.

- [ ] **Step 6: Commit**

```bash
git add src/adapter/store.rs src/adapter/mod.rs
git commit -m "feat(adapter): resolve adapter paths and installed versions"
```

---

### Task 3: Hỏi danh tính adapter qua handshake

Adapter tự khai `agentInfo.name` và `agentInfo.version` trong phản hồi `initialize`, và `validate_handshake` (src/transport/acp.rs:463) so khớp chính xác hai giá trị đó. Chúng không suy ra được từ metadata package, nên phải hỏi chính adapter. Hàm này đặt trong `transport` để giữ bất biến "chỉ tầng transport nói ACP".

**Files:**

- Modify: `src/transport/acp.rs`
- Modify: `src/transport/mod.rs`
- Test: `tests/adapter_probe.rs`

**Interfaces:**

- Consumes: không có từ task trước.
- Produces: `transport::AgentIdentity { pub name: String, pub version: String }` và `transport::probe_agent_identity(executable: &Path, arguments: &[String]) -> Result<AgentIdentity, TransportError>`.

- [ ] **Step 1: Viết test thất bại**

Tạo `tests/adapter_probe.rs`:

```rust
use july_workspace::transport::probe_agent_identity;
use std::path::{Path, PathBuf};

fn fixture() -> Vec<String> {
    vec![
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/acp_agent.py")
            .to_string_lossy()
            .into_owned(),
    ]
}

#[tokio::test]
async fn probe_reads_the_identity_the_adapter_declares() {
    let identity = probe_agent_identity(Path::new("/usr/bin/python3"), &fixture())
        .await
        .expect("probe succeeds");

    assert_eq!(identity.name, "test-acp-agent");
    assert_eq!(identity.version, "1.0.0");
}

#[tokio::test]
async fn probe_reads_the_alternate_identity_of_the_same_fixture() {
    let mut arguments = fixture();
    arguments.push("--claude".into());

    let identity = probe_agent_identity(Path::new("/usr/bin/python3"), &arguments)
        .await
        .expect("probe succeeds");

    assert_eq!(identity.name, "claude-test");
}

#[tokio::test]
async fn probe_rejects_a_relative_executable() {
    let error = probe_agent_identity(Path::new("python3"), &fixture())
        .await
        .expect_err("relative executable is rejected");

    assert!(error.to_string().contains("absolute"), "got {error}");
}

#[tokio::test]
async fn probe_fails_when_the_adapter_speaks_the_wrong_protocol() {
    let mut arguments = fixture();
    arguments.push("--protocol-zero".into());

    assert!(
        probe_agent_identity(Path::new("/usr/bin/python3"), &arguments)
            .await
            .is_err()
    );
}
```

- [ ] **Step 2: Chạy test để xác nhận thất bại**

Run: `cargo test --test adapter_probe`
Expected: FAIL khi biên dịch, `unresolved import july_workspace::transport::probe_agent_identity`.

- [ ] **Step 3: Viết implementation nhỏ nhất**

Thêm vào `src/transport/mod.rs`, cạnh các struct công khai khác:

```rust
/// Danh tính một adapter tự khai trong phản hồi `initialize`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentIdentity {
    pub name: String,
    pub version: String,
}
```

và mở rộng dòng `pub use acp::AcpTransport;` thành:

```rust
pub use acp::{AcpTransport, probe_agent_identity};
```

Thêm vào cuối `src/transport/acp.rs`, phía trên `mod tests` nếu có:

```rust
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// Spawn adapter, gửi `initialize`, đọc danh tính nó tự khai, rồi dừng.
///
/// Dùng ở `july init` để `expected_agent_name` và `expected_agent_version`
/// trong `AcpAgentConfig` là giá trị thật chứ không phải phỏng đoán.
pub async fn probe_agent_identity(
    executable: &Path,
    arguments: &[String],
) -> Result<AgentIdentity, TransportError> {
    if !executable.is_absolute() {
        return Err(TransportError::InvalidConfiguration(
            "ACP executable must be an absolute path",
        ));
    }
    if !executable.is_file() {
        return Err(TransportError::InvalidConfiguration(
            "ACP executable must exist and be a file",
        ));
    }

    let sdk_config = agent_client_protocol::AcpAgentConfig::new(executable)
        .args(arguments.to_vec())
        .envs(BTreeMap::new());
    let (identified, identity) = oneshot::channel();

    let connection = agent_client_protocol::Client
        .builder()
        .on_receive_notification(
            async move |_: SessionNotification, _connection| Ok(()),
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            async move |_: RequestPermissionRequest, responder, _connection| {
                responder.respond(RequestPermissionResponse::new(
                    RequestPermissionOutcome::Cancelled,
                ))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(
            AcpAgent::new(sdk_config),
            async move |connection: ConnectionTo<Agent>| {
                let result = connection
                    .send_request(InitializeRequest::new(ProtocolVersion::V1))
                    .block_task()
                    .await;
                let _ = identified.send(result.map_err(map_sdk_error).and_then(read_identity));
                Ok(())
            },
        );

    match tokio::time::timeout(PROBE_TIMEOUT, async {
        let handshake = tokio::spawn(connection);
        let identity = identity.await;
        handshake.abort();
        identity
    })
    .await
    {
        Ok(Ok(identity)) => identity,
        Ok(Err(_)) => Err(TransportError::ChannelClosed),
        Err(_) => Err(TransportError::InvalidConfiguration(
            "adapter không trả lời initialize trong 30 giây",
        )),
    }
}

fn read_identity(
    initialized: agent_client_protocol::schema::v1::InitializeResponse,
) -> Result<AgentIdentity, TransportError> {
    if initialized.protocol_version != ProtocolVersion::V1 {
        return Err(TransportError::UnsupportedProtocol {
            expected: 1,
            actual: initialized.protocol_version.as_u16(),
        });
    }
    let info = initialized
        .agent_info
        .as_ref()
        .ok_or_else(|| TransportError::UnexpectedAgentIdentity {
            expected: "agentInfo".into(),
            actual: "missing agentInfo".into(),
        })?;
    Ok(AgentIdentity {
        name: info.name.clone(),
        version: info.version.clone(),
    })
}
```

Bổ sung import ở đầu `src/transport/acp.rs`: thêm `AgentIdentity` vào khối `use super::{...}` và `use std::collections::BTreeMap;`.

- [ ] **Step 4: Chạy test để xác nhận pass**

Run: `cargo test --test adapter_probe`
Expected: PASS, 4 test.

Nếu builder API của `agent-client-protocol =2.0.0` không nhận cấu hình handler như trên, đối chiếu `run_connection` (src/transport/acp.rs:277-414) - đó là lần dùng cùng API duy nhất trong repo - và giữ đúng thứ tự `.builder().on_receive_notification(..).on_receive_request(..).connect_with(..)`.

- [ ] **Step 5: Gate chất lượng**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: không cảnh báo, toàn bộ test pass.

- [ ] **Step 6: Commit**

```bash
git add src/transport/acp.rs src/transport/mod.rs tests/adapter_probe.rs
git commit -m "feat(transport): probe an adapter for the identity it declares"
```

---

### Task 4: Sinh transport_config và test round-trip

Đây là task khoá lại lỗi gốc: nơi ghi `transport_config` và nơi đọc nó phải cùng một hợp đồng.

**Files:**

- Modify: `src/adapter/store.rs`
- Modify: `src/runtime/direct_message.rs:381`
- Modify: `src/runtime/mod.rs`

**Interfaces:**

- Consumes: `AdapterStore`, `AdapterError`, `ensure_state_directory` từ Task 2; `AgentIdentity` từ Task 3.
- Produces:
  - `AdapterIdentity { pub name: String, pub version: String, pub bin: PathBuf }` với `serde_json` đọc/ghi thủ công qua `Value`.
  - `AdapterStore::identities(&self) -> Result<BTreeMap<String, AdapterIdentity>, AdapterError>`
  - `AdapterStore::record_identity(&self, id: &str, identity: AdapterIdentity) -> Result<(), AdapterError>`
  - `AdapterStore::config_for(&self, id: &str, agent_name: &str) -> Result<Value, AdapterError>`
  - `crate::runtime::parse_acp_config` thành `pub(crate)`.

- [ ] **Step 1: Viết test thất bại**

Thêm vào `mod tests` của `src/adapter/store.rs`:

```rust
    fn identity() -> AdapterIdentity {
        AdapterIdentity {
            name: "codex-acp".into(),
            version: "1.6.2".into(),
            bin: PathBuf::from("/opt/july/adapters/node_modules/.bin/codex-acp"),
        }
    }

    #[test]
    fn identities_start_empty_and_survive_a_round_trip() {
        let store = AdapterStore::new(scratch());
        assert!(store.identities().expect("empty identities").is_empty());

        store
            .record_identity("codex", identity())
            .expect("record identity");

        let stored = store.identities().expect("identities");
        assert_eq!(stored.len(), 1);
        assert_eq!(stored["codex"].name, "codex-acp");
        assert_eq!(stored["codex"].version, "1.6.2");
    }

    #[test]
    fn recording_a_second_adapter_keeps_the_first() {
        let store = AdapterStore::new(scratch());
        store.record_identity("codex", identity()).expect("first");
        store
            .record_identity(
                "claude",
                AdapterIdentity {
                    name: "claude-agent-acp".into(),
                    version: "0.70.0".into(),
                    bin: PathBuf::from("/opt/july/adapters/node_modules/.bin/claude-agent-acp"),
                },
            )
            .expect("second");

        let stored = store.identities().expect("identities");
        assert_eq!(stored.len(), 2);
    }

    #[test]
    fn config_for_rejects_an_unknown_adapter() {
        let store = AdapterStore::new(scratch());

        assert!(matches!(
            store.config_for("nope", "agent_order"),
            Err(AdapterError::UnknownAdapter(id)) if id == "nope"
        ));
    }

    #[test]
    fn config_for_rejects_an_adapter_that_was_never_verified() {
        let store = AdapterStore::new(scratch());

        assert!(matches!(
            store.config_for("codex", "agent_order"),
            Err(AdapterError::NotVerified { id }) if id == "codex"
        ));
    }

    #[test]
    fn config_for_is_accepted_by_the_runtime_parser() {
        let home = scratch();
        let store = AdapterStore::new(home.clone());
        store.record_identity("codex", identity()).expect("record");

        let config = store
            .config_for("codex", "agent_order")
            .expect("config generated");

        // Đây là hợp đồng bị vỡ trước đây: nơi ghi và nơi đọc phải khớp nhau.
        let parsed = crate::runtime::parse_acp_config(&config).expect("runtime accepts the config");
        assert_eq!(parsed.executable, identity().bin);
        assert_eq!(parsed.expected_agent_name, "codex-acp");
        assert_eq!(parsed.expected_agent_version, "1.6.2");
        assert_eq!(parsed.state_directory, home.join("state/agent_order"));
        assert!(parsed.arguments.is_empty());
        assert!(parsed.environment.is_empty());
        assert!(
            parsed.state_directory.is_dir(),
            "state directory phải được tạo trước khi adapter chạy"
        );
    }
```

- [ ] **Step 2: Chạy test để xác nhận thất bại**

Run: `cargo test --lib adapter::store`
Expected: FAIL khi biên dịch, `cannot find type AdapterIdentity` và `parse_acp_config is private`.

- [ ] **Step 3: Viết implementation nhỏ nhất**

Trong `src/runtime/direct_message.rs:381`, đổi chữ ký:

```rust
pub(crate) fn parse_acp_config(value: &Value) -> Result<AcpAgentConfig, DirectMessageBootstrapError> {
```

Trong `src/runtime/mod.rs`, thêm cạnh các `pub use` khác:

```rust
pub(crate) use direct_message::parse_acp_config;
```

Thêm vào `src/adapter/store.rs`:

```rust
const IDENTITIES: &str = "identities.json";

/// Danh tính thật của một adapter, ghi lại sau khi `july init` xác minh handshake.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdapterIdentity {
    pub name: String,
    pub version: String,
    pub bin: PathBuf,
}

impl AdapterStore {
    fn identities_path(&self) -> PathBuf {
        self.adapters_root().join(IDENTITIES)
    }

    /// Các adapter đã cài và đã xác minh handshake. File thiếu coi như rỗng.
    pub fn identities(&self) -> Result<BTreeMap<String, AdapterIdentity>, AdapterError> {
        let Ok(raw) = std::fs::read_to_string(self.identities_path()) else {
            return Ok(BTreeMap::new());
        };
        let parsed: Value = serde_json::from_str(&raw)?;
        let mut identities = BTreeMap::new();
        for (id, entry) in parsed.as_object().into_iter().flatten() {
            let field = |key: &str| entry.get(key).and_then(Value::as_str).map(str::to_owned);
            let (Some(name), Some(version), Some(bin)) =
                (field("name"), field("version"), field("bin"))
            else {
                continue;
            };
            identities.insert(
                id.clone(),
                AdapterIdentity {
                    name,
                    version,
                    bin: PathBuf::from(bin),
                },
            );
        }
        Ok(identities)
    }

    /// Ghi danh tính một adapter, giữ nguyên các adapter đã có.
    pub fn record_identity(
        &self,
        id: &str,
        identity: AdapterIdentity,
    ) -> Result<(), AdapterError> {
        let mut identities = self.identities()?;
        identities.insert(id.to_owned(), identity);
        let document = Value::Object(
            identities
                .into_iter()
                .map(|(id, identity)| {
                    (
                        id,
                        serde_json::json!({
                            "name": identity.name,
                            "version": identity.version,
                            "bin": identity.bin.to_string_lossy(),
                        }),
                    )
                })
                .collect(),
        );
        std::fs::create_dir_all(self.adapters_root())?;
        std::fs::write(
            self.identities_path(),
            format!("{}\n", serde_json::to_string_pretty(&document)?),
        )?;
        Ok(())
    }

    /// `transport_config` đầy đủ cho một agent dùng adapter `id`.
    ///
    /// Danh mục là nguồn sự thật về id hợp lệ; `identities.json` chỉ được tin
    /// sau khi id đã có trong danh mục.
    pub fn config_for(&self, id: &str, agent_name: &str) -> Result<Value, AdapterError> {
        let spec = super::find(id).ok_or_else(|| AdapterError::UnknownAdapter(id.to_owned()))?;
        let identity = self
            .identities()?
            .remove(spec.id)
            .ok_or_else(|| AdapterError::NotVerified {
                id: spec.id.to_owned(),
            })?;
        let state = ensure_state_directory(&self.state_root(), agent_name)?;
        Ok(serde_json::json!({
            "executable": identity.bin.to_string_lossy(),
            "arguments": Vec::<String>::new(),
            "environment": serde_json::Map::new(),
            "state_directory": state.to_string_lossy(),
            "expected_agent_name": identity.name,
            "expected_agent_version": identity.version,
        }))
    }
}
```

Bổ sung `use std::collections::BTreeMap;` ở đầu file.

- [ ] **Step 4: Chạy test để xác nhận pass**

Run: `cargo test --lib adapter::store`
Expected: PASS, 12 test. Test quan trọng nhất là `config_for_is_accepted_by_the_runtime_parser`.

- [ ] **Step 5: Gate chất lượng**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: không cảnh báo, toàn bộ test pass.

- [ ] **Step 6: Commit**

```bash
git add src/adapter/store.rs src/runtime/direct_message.rs src/runtime/mod.rs
git commit -m "feat(adapter): generate a transport_config the runtime parser accepts"
```

---

### Task 5: Raw mode và giải mã phím

**Files:**

- Create: `src/cli/keys.rs`
- Modify: `src/cli/mod.rs`
- Modify: `Cargo.toml`

**Interfaces:**

- Consumes: không có.
- Produces: `Key` enum (`Up`, `Down`, `Space`, `Enter`, `Quit`, `Interrupt`, `Other`), `decode(bytes: &[u8]) -> Option<(Key, usize)>`, `RawMode::enable() -> io::Result<Option<RawMode>>` (trả `None` khi stdin không phải TTY).

- [ ] **Step 1: Viết test thất bại**

Tạo `src/cli/keys.rs` với phần test:

```rust
//! Raw mode và giải mã phím cho màn hình onboarding. Chỉ chạy trên unix.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_the_arrow_keys() {
        assert_eq!(decode(b"\x1b[A"), Some((Key::Up, 3)));
        assert_eq!(decode(b"\x1b[B"), Some((Key::Down, 3)));
    }

    #[test]
    fn decodes_the_action_keys() {
        assert_eq!(decode(b" "), Some((Key::Space, 1)));
        assert_eq!(decode(b"\r"), Some((Key::Enter, 1)));
        assert_eq!(decode(b"\n"), Some((Key::Enter, 1)));
        assert_eq!(decode(b"q"), Some((Key::Quit, 1)));
        assert_eq!(decode(b"Q"), Some((Key::Quit, 1)));
        assert_eq!(decode(b"\x03"), Some((Key::Interrupt, 1)));
    }

    #[test]
    fn waits_for_more_bytes_on_a_partial_escape_sequence() {
        assert_eq!(decode(b""), None);
        assert_eq!(decode(b"\x1b"), None);
        assert_eq!(decode(b"\x1b["), None);
    }

    #[test]
    fn consumes_only_the_first_key_of_a_burst() {
        assert_eq!(decode(b"\x1b[Aq"), Some((Key::Up, 3)));
        assert_eq!(decode(b" \r"), Some((Key::Space, 1)));
    }

    #[test]
    fn maps_an_unhandled_byte_to_other() {
        assert_eq!(decode(b"x"), Some((Key::Other, 1)));
        assert_eq!(decode(b"\x1b[C"), Some((Key::Other, 3)));
    }
}
```

- [ ] **Step 2: Chạy test để xác nhận thất bại**

Run: `cargo test --lib cli::keys`
Expected: FAIL khi biên dịch, `cannot find function decode`.

- [ ] **Step 3: Viết implementation nhỏ nhất**

Thêm `libc = "=0.2.189"` vào `[dependencies]` trong `Cargo.toml`, giữ thứ tự chữ cái (sau `chrono`, trước `rusqlite`).

Thêm phía trên `mod tests` trong `src/cli/keys.rs`:

```rust
use std::io;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Key {
    Up,
    Down,
    Space,
    Enter,
    Quit,
    Interrupt,
    Other,
}

/// Giải mã phím đầu tiên trong buffer, trả kèm số byte đã dùng.
///
/// `None` nghĩa là chuỗi escape còn dở, người gọi cần đọc thêm byte.
pub fn decode(bytes: &[u8]) -> Option<(Key, usize)> {
    match bytes {
        [] | [0x1b] | [0x1b, b'['] => None,
        [0x1b, b'[', b'A', ..] => Some((Key::Up, 3)),
        [0x1b, b'[', b'B', ..] => Some((Key::Down, 3)),
        [0x1b, b'[', _, ..] => Some((Key::Other, 3)),
        [0x1b, ..] => Some((Key::Other, bytes.len())),
        [b' ', ..] => Some((Key::Space, 1)),
        [b'\r' | b'\n', ..] => Some((Key::Enter, 1)),
        [0x03, ..] => Some((Key::Interrupt, 1)),
        [b'q' | b'Q', ..] => Some((Key::Quit, 1)),
        [_, ..] => Some((Key::Other, 1)),
    }
}

/// Đặt stdin vào raw mode và trả về guard restore lại khi `Drop`.
///
/// Guard bảo đảm terminal không bị bỏ ở trạng thái raw kể cả khi panic.
pub struct RawMode {
    descriptor: i32,
    original: libc::termios,
}

impl RawMode {
    /// `Ok(None)` khi stdin không phải terminal - người gọi phải đi đường không tương tác.
    pub fn enable() -> io::Result<Option<Self>> {
        let descriptor = libc::STDIN_FILENO;
        if unsafe { libc::isatty(descriptor) } != 1 {
            return Ok(None);
        }
        let mut original = std::mem::MaybeUninit::<libc::termios>::uninit();
        if unsafe { libc::tcgetattr(descriptor, original.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let original = unsafe { original.assume_init() };
        let mut raw = original;
        unsafe { libc::cfmakeraw(&mut raw) };
        if unsafe { libc::tcsetattr(descriptor, libc::TCSANOW, &raw) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Some(Self {
            descriptor,
            original,
        }))
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        unsafe { libc::tcsetattr(self.descriptor, libc::TCSANOW, &self.original) };
    }
}
```

Thêm `mod keys;` vào đầu `src/cli/mod.rs`, cạnh `mod registry;`.

- [ ] **Step 4: Chạy test để xác nhận pass**

Run: `cargo test --lib cli::keys`
Expected: PASS, 5 test.

- [ ] **Step 5: Gate chất lượng**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: không cảnh báo, toàn bộ test pass.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/cli/keys.rs src/cli/mod.rs
git commit -m "feat(cli): add termios raw mode guard and key decoding"
```

---

### Task 6: Máy trạng thái lựa chọn

**Files:**

- Create: `src/cli/init.rs`
- Modify: `src/cli/mod.rs`

**Interfaces:**

- Consumes: `adapter::{ADAPTERS, AdapterSpec, Tier}` từ Task 1.
- Produces: `Selection::new(items: Vec<&'static AdapterSpec>) -> Self`, `up()`, `down()`, `toggle()`, `cursor() -> usize`, `is_checked(index: usize) -> bool`, `chosen() -> Vec<&'static AdapterSpec>`.

- [ ] **Step 1: Viết test thất bại**

Tạo `src/cli/init.rs` với phần test:

```rust
//! Màn hình onboarding cài ACP adapter.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::find;

    fn catalog() -> Vec<&'static AdapterSpec> {
        ADAPTERS.iter().collect()
    }

    #[test]
    fn core_adapters_start_checked_and_optional_ones_do_not() {
        let selection = Selection::new(catalog());

        let chosen: Vec<&str> = selection.chosen().iter().map(|spec| spec.id).collect();
        assert_eq!(chosen, vec!["codex", "claude"]);
    }

    #[test]
    fn cursor_starts_at_the_first_item() {
        assert_eq!(Selection::new(catalog()).cursor(), 0);
    }

    #[test]
    fn cursor_clamps_at_both_ends() {
        let mut selection = Selection::new(catalog());

        selection.up();
        assert_eq!(selection.cursor(), 0, "không đi lên khỏi đầu danh sách");

        for _ in 0..10 {
            selection.down();
        }
        assert_eq!(
            selection.cursor(),
            catalog().len() - 1,
            "không đi xuống khỏi cuối danh sách"
        );
    }

    #[test]
    fn toggle_flips_the_item_under_the_cursor() {
        let mut selection = Selection::new(catalog());

        selection.toggle();
        assert!(!selection.is_checked(0), "codex bị bỏ tick");

        selection.toggle();
        assert!(selection.is_checked(0), "codex được tick lại");
    }

    #[test]
    fn toggle_can_add_an_optional_adapter() {
        let mut selection = Selection::new(catalog());
        selection.down();
        selection.down();
        selection.toggle();

        let chosen: Vec<&str> = selection.chosen().iter().map(|spec| spec.id).collect();
        assert_eq!(chosen, vec!["codex", "claude", "claude-rust"]);
    }

    #[test]
    fn chosen_is_empty_once_everything_is_unticked() {
        let mut selection = Selection::new(catalog());
        selection.toggle();
        selection.down();
        selection.toggle();

        assert!(selection.chosen().is_empty());
        assert!(find("codex").is_some(), "danh mục không bị thay đổi");
    }
}
```

- [ ] **Step 2: Chạy test để xác nhận thất bại**

Run: `cargo test --lib cli::init`
Expected: FAIL khi biên dịch, `cannot find type Selection`.

- [ ] **Step 3: Viết implementation nhỏ nhất**

Thêm phía trên `mod tests` trong `src/cli/init.rs`:

```rust
use crate::adapter::{ADAPTERS, AdapterSpec, Tier};

/// Trạng thái con trỏ và các ô tick của màn hình onboarding.
pub(crate) struct Selection {
    items: Vec<&'static AdapterSpec>,
    checked: Vec<bool>,
    cursor: usize,
}

impl Selection {
    /// Adapter `Core` được tick sẵn; con trỏ đứng ở dòng đầu.
    pub(crate) fn new(items: Vec<&'static AdapterSpec>) -> Self {
        let checked = items
            .iter()
            .map(|spec| matches!(spec.tier, Tier::Core))
            .collect();
        Self {
            items,
            checked,
            cursor: 0,
        }
    }

    pub(crate) fn items(&self) -> &[&'static AdapterSpec] {
        &self.items
    }

    pub(crate) fn cursor(&self) -> usize {
        self.cursor
    }

    pub(crate) fn is_checked(&self, index: usize) -> bool {
        self.checked.get(index).copied().unwrap_or(false)
    }

    pub(crate) fn up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub(crate) fn down(&mut self) {
        let last = self.items.len().saturating_sub(1);
        self.cursor = (self.cursor + 1).min(last);
    }

    pub(crate) fn toggle(&mut self) {
        if let Some(checked) = self.checked.get_mut(self.cursor) {
            *checked = !*checked;
        }
    }

    pub(crate) fn chosen(&self) -> Vec<&'static AdapterSpec> {
        self.items
            .iter()
            .zip(&self.checked)
            .filter_map(|(spec, checked)| checked.then_some(*spec))
            .collect()
    }
}
```

Thêm `mod init;` vào đầu `src/cli/mod.rs`, cạnh `mod keys;`.

- [ ] **Step 4: Chạy test để xác nhận pass**

Run: `cargo test --lib cli::init`
Expected: PASS, 6 test.

- [ ] **Step 5: Gate chất lượng**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: không cảnh báo, toàn bộ test pass.

- [ ] **Step 6: Commit**

```bash
git add src/cli/init.rs src/cli/mod.rs
git commit -m "feat(cli): add the adapter selection state machine"
```

---

### Task 7: Cài adapter

**Files:**

- Modify: `src/adapter/store.rs`
- Modify: `src/adapter/mod.rs`

**Interfaces:**

- Consumes: `AdapterSpec`, `Installer`, `AdapterError`, `AdapterStore` từ Task 1 và 2.
- Produces: `trait PackageInstaller { fn install(&self, spec: &AdapterSpec, root: &Path) -> Result<(), AdapterError>; }`, `struct SystemInstaller`, `fn install_command(spec: &AdapterSpec, root: &Path) -> (&'static str, Vec<String>)`.

- [ ] **Step 1: Viết test thất bại**

Thêm vào `mod tests` của `src/adapter/store.rs`:

```rust
    #[test]
    fn npm_install_targets_the_july_prefix_with_a_pinned_version() {
        let spec = find("codex").expect("codex");
        let (tool, arguments) = install_command(spec, Path::new("/opt/july/adapters"));

        assert_eq!(tool, "npm");
        assert_eq!(
            arguments,
            vec![
                "install".to_string(),
                "--prefix".to_string(),
                "/opt/july/adapters".to_string(),
                "@agentclientprotocol/codex-acp@1.6.2".to_string(),
            ]
        );
    }

    #[test]
    fn cargo_install_targets_the_july_root_with_a_pinned_version() {
        let spec = find("claude-rust").expect("claude-rust");
        let (tool, arguments) = install_command(spec, Path::new("/opt/july/adapters"));

        assert_eq!(tool, "cargo");
        assert_eq!(
            arguments,
            vec![
                "install".to_string(),
                "claude-code-acp-rs".to_string(),
                "--version".to_string(),
                "0.1.22".to_string(),
                "--root".to_string(),
                "/opt/july/adapters".to_string(),
            ]
        );
    }

    #[test]
    fn no_install_command_uses_a_moving_version_specifier() {
        for spec in crate::adapter::ADAPTERS {
            let (_, arguments) = install_command(spec, Path::new("/opt/july/adapters"));
            assert!(
                !arguments.iter().any(|argument| argument.contains("latest")),
                "{} dùng version di động",
                spec.id
            );
        }
    }
```

- [ ] **Step 2: Chạy test để xác nhận thất bại**

Run: `cargo test --lib adapter::store`
Expected: FAIL khi biên dịch, `cannot find function install_command`.

- [ ] **Step 3: Viết implementation nhỏ nhất**

Thêm vào `src/adapter/store.rs`:

```rust
/// Trình cài và tham số cho một adapter. Tách riêng để test được không cần mạng.
pub fn install_command(spec: &AdapterSpec, root: &Path) -> (&'static str, Vec<String>) {
    let root = root.to_string_lossy().into_owned();
    match spec.installer {
        Installer::Npm => (
            "npm",
            vec![
                "install".into(),
                "--prefix".into(),
                root,
                format!("{}@{}", spec.package, spec.version),
            ],
        ),
        Installer::Cargo => (
            "cargo",
            vec![
                "install".into(),
                spec.package.into(),
                "--version".into(),
                spec.version.into(),
                "--root".into(),
                root,
            ],
        ),
    }
}

/// Chạy trình cài thật. Test dùng implementation khác để không đụng mạng.
pub trait PackageInstaller {
    fn install(&self, spec: &AdapterSpec, root: &Path) -> Result<(), AdapterError>;
}

pub struct SystemInstaller;

impl PackageInstaller for SystemInstaller {
    fn install(&self, spec: &AdapterSpec, root: &Path) -> Result<(), AdapterError> {
        let (tool, arguments) = install_command(spec, root);
        std::fs::create_dir_all(root)?;
        let status = std::process::Command::new(tool)
            .args(&arguments)
            .status()
            .map_err(|error| match error.kind() {
                io::ErrorKind::NotFound => AdapterError::ToolMissing { tool },
                _ => AdapterError::Io(error),
            })?;
        if status.success() {
            return Ok(());
        }
        Err(AdapterError::InstallFailed {
            id: spec.id.to_owned(),
            tool,
            status: status
                .code()
                .map(|code| format!("mã {code}"))
                .unwrap_or_else(|| "tín hiệu dừng".into()),
        })
    }
}
```

Cập nhật `src/adapter/mod.rs`:

```rust
pub use store::{
    AdapterError, AdapterIdentity, AdapterStore, PackageInstaller, SystemInstaller,
    ensure_state_directory, install_command,
};
```

- [ ] **Step 4: Chạy test để xác nhận pass**

Run: `cargo test --lib adapter::store`
Expected: PASS, 15 test.

- [ ] **Step 5: Gate chất lượng**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: không cảnh báo, toàn bộ test pass.

- [ ] **Step 6: Commit**

```bash
git add src/adapter/store.rs src/adapter/mod.rs
git commit -m "feat(adapter): install adapters into the july-owned prefix"
```

---

### Task 8: Lệnh `july init`

**Files:**

- Modify: `src/cli/init.rs`
- Modify: `src/cli/mod.rs`
- Modify: `docs/08-RUNTIME-AND-CLI.md`
- Test: `tests/cli_init.rs`

**Interfaces:**

- Consumes: `Selection` (Task 6), `keys::{Key, RawMode, decode}` (Task 5), `AdapterStore`, `SystemInstaller`, `PackageInstaller`, `AdapterIdentity` (Task 2, 4, 7), `transport::probe_agent_identity` (Task 3).
- Produces: `Command::Init { adapters: Option<Vec<String>> }`, `pub(crate) async fn run_init(adapters: Option<Vec<String>>) -> Result<(), CliError>`, `CliError::{Adapter, NoAdapterSelected}`.

- [ ] **Step 1: Viết test thất bại**

Tạo `tests/cli_init.rs`:

```rust
use std::ffi::OsString;
use std::path::PathBuf;
use ulid::Ulid;

fn scratch_home() -> PathBuf {
    let path = std::env::temp_dir().join(format!("july-cli-init-{}", Ulid::generate()));
    std::fs::create_dir_all(&path).expect("scratch home");
    path
}

fn arguments(rest: &[&str]) -> Vec<OsString> {
    std::iter::once("july".to_string())
        .chain(rest.iter().map(ToString::to_string))
        .map(OsString::from)
        .collect()
}

#[tokio::test]
async fn init_rejects_an_unknown_adapter_id_before_touching_the_network() {
    let home = scratch_home();
    // SAFETY: mỗi test chạy trong process riêng của harness Rust không đảm bảo,
    // nên dùng JULY_HOME riêng và chấp nhận set biến môi trường trong test này.
    unsafe { std::env::set_var("JULY_HOME", &home) };

    let error = july_workspace::cli::run(arguments(&["init", "--adapters", "khong-ton-tai"]))
        .await
        .expect_err("unknown adapter rejected");

    assert!(error.to_string().contains("khong-ton-tai"), "got {error}");
    assert!(
        !home.join("adapters/node_modules").exists(),
        "không được cài gì khi id sai"
    );
}

#[tokio::test]
async fn init_rejects_an_empty_adapter_list() {
    let home = scratch_home();
    unsafe { std::env::set_var("JULY_HOME", &home) };

    assert!(
        july_workspace::cli::run(arguments(&["init", "--adapters", ""]))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn init_usage_error_when_the_flag_has_no_value() {
    let home = scratch_home();
    unsafe { std::env::set_var("JULY_HOME", &home) };

    assert!(
        july_workspace::cli::run(arguments(&["init", "--adapters"]))
            .await
            .is_err()
    );
}
```

- [ ] **Step 2: Chạy test để xác nhận thất bại**

Run: `cargo test --test cli_init`
Expected: FAIL, `invalid command` vì `init` chưa được parse.

- [ ] **Step 3: Viết implementation nhỏ nhất**

Thêm vào `src/cli/init.rs`:

```rust
use super::keys::{Key, RawMode, decode};
use super::CliError;
use crate::adapter::{
    AdapterIdentity, AdapterStore, PackageInstaller, SystemInstaller,
};
use crate::transport::probe_agent_identity;
use std::io::{Read, Write};

/// Chạy màn hình onboarding, hoặc đi đường không tương tác khi được chỉ định.
pub(crate) async fn run_init(adapters: Option<Vec<String>>) -> Result<(), CliError> {
    let store = AdapterStore::open_default()?;
    let chosen = match adapters {
        Some(ids) => resolve_ids(&ids)?,
        None => match RawMode::enable()? {
            Some(guard) => {
                let chosen = interactive_select(&guard)?;
                drop(guard);
                chosen
            }
            None => {
                println!(
                    "stdin không phải terminal, dùng mặc định: codex, claude.\n\
                     Chỉ định khác bằng july init --adapters <ids>"
                );
                resolve_ids(&["codex".into(), "claude".into()])?
            }
        },
    };
    if chosen.is_empty() {
        return Err(CliError::NoAdapterSelected);
    }

    let installer = SystemInstaller;
    let mut failures = Vec::new();
    for spec in chosen {
        println!("Đang cài {} ({} {})", spec.id, spec.package, spec.version);
        if let Err(error) = installer.install(spec, &store.adapters_root()) {
            println!("  thất bại: {error}");
            failures.push(spec.id);
            continue;
        }
        let bin = store.bin_path(spec);
        match probe_agent_identity(&bin, &[]).await {
            Ok(identity) => {
                store.record_identity(
                    spec.id,
                    AdapterIdentity {
                        name: identity.name.clone(),
                        version: identity.version.clone(),
                        bin,
                    },
                )?;
                println!("  đã xác minh: {} {}", identity.name, identity.version);
            }
            Err(error) => {
                println!("  cài xong nhưng không xác minh được danh tính: {error}");
                failures.push(spec.id);
            }
        }
    }

    if failures.is_empty() {
        println!("Xong. Tạo agent bằng: july agent add <tên> --project <đường dẫn> --adapter <id>");
        return Ok(());
    }
    Err(CliError::Runtime(format!(
        "các adapter sau chưa dùng được: {}. Chạy lại july init để thử tiếp",
        failures.join(", ")
    )))
}

fn resolve_ids(ids: &[String]) -> Result<Vec<&'static AdapterSpec>, CliError> {
    ids.iter()
        .map(|id| {
            crate::adapter::find(id.trim())
                .ok_or_else(|| CliError::Adapter(crate::adapter::AdapterError::UnknownAdapter(id.clone())))
        })
        .collect()
}

fn interactive_select(_guard: &RawMode) -> Result<Vec<&'static AdapterSpec>, CliError> {
    let store = AdapterStore::open_default()?;
    let mut selection = Selection::new(ADAPTERS.iter().collect());
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 16];
    let mut stdin = std::io::stdin();
    let mut first = true;

    loop {
        render(&selection, &store, first)?;
        first = false;
        let read = stdin.read(&mut chunk)?;
        if read == 0 {
            return Ok(Vec::new());
        }
        buffer.extend_from_slice(&chunk[..read]);
        while let Some((key, used)) = decode(&buffer) {
            buffer.drain(..used);
            match key {
                Key::Up => selection.up(),
                Key::Down => selection.down(),
                Key::Space => selection.toggle(),
                Key::Enter => return Ok(selection.chosen()),
                Key::Quit | Key::Interrupt => return Ok(Vec::new()),
                Key::Other => {}
            }
        }
    }
}

fn render(selection: &Selection, store: &AdapterStore, first: bool) -> Result<(), CliError> {
    let mut out = std::io::stdout();
    if !first {
        // Kéo con trỏ về đầu khối đã vẽ: tiêu đề, danh sách, hai dòng chú thích.
        write!(out, "\x1b[{}A", selection.items().len() + 4)?;
    }
    write!(out, "\rJuly cần ít nhất một ACP adapter. Chọn adapter để cài:\r\n\r\n")?;
    for (index, spec) in selection.items().iter().enumerate() {
        let pointer = if index == selection.cursor() { "❯" } else { " " };
        let tick = if selection.is_checked(index) { "x" } else { " " };
        let state = match store.installed_version(spec) {
            Some(version) if version == spec.version => format!(" (đã cài {version})"),
            Some(version) => format!(" (đã cài {version} → có {})", spec.version),
            None => String::new(),
        };
        write!(
            out,
            "  {pointer} [{tick}] {:<13}{}{}\r\n",
            spec.id, spec.summary, state
        )?;
    }
    write!(
        out,
        "\r\n  ↑↓ di chuyển · space chọn/bỏ · enter xác nhận · q thoát\r\n"
    )?;
    write!(out, "  Đã chọn: {} adapter\r\n", selection.chosen().len())?;
    out.flush()?;
    Ok(())
}
```

Trong `src/cli/mod.rs`:

1. Thêm hai variant vào `CliError` (sau `InvalidCommand`):

```rust
    #[error(transparent)]
    Adapter(#[from] crate::adapter::AdapterError),
    #[error("chưa chọn adapter nào; chọn ít nhất một để july có thể chạy agent")]
    NoAdapterSelected,
```

2. Thêm variant vào `Command`:

```rust
    Init {
        adapters: Option<Vec<String>>,
    },
```

3. Thêm nhánh vào `parse_command` (src/cli/mod.rs:446), phía trên `Some("agent")`:

```rust
        Some("init") => parse_init(args, json),
```

4. Thêm parser cạnh `parse_agent`:

```rust
fn parse_init(args: Vec<String>, json: bool) -> Result<Command, CliError> {
    if json {
        return Err(CliError::Usage);
    }
    match args.as_slice() {
        [_] => Ok(Command::Init { adapters: None }),
        [_, flag, list] if flag == "--adapters" => Ok(Command::Init {
            adapters: Some(list.split(',').map(str::to_owned).collect()),
        }),
        _ => Err(CliError::Usage),
    }
}
```

5. Thêm nhánh dispatch trong `run` (src/cli/mod.rs:102):

```rust
        Command::Init { adapters } => init::run_init(adapters).await,
```

6. Thêm `Command::Init { .. } => false` vào hàm `Command::json()` nếu hàm đó match từng variant.

7. Bổ sung vào hằng `USAGE`, phía trên dòng của `agent`:

```
  july init [--adapters <ids>]      cài ACP adapter
```

8. Thêm `Adapter` và `NoAdapterSelected` vào `error_code()` với mã `"adapter"` và `"no_adapter_selected"`.

Cập nhật `docs/08-RUNTIME-AND-CLI.md`: thêm `july init` vào bảng lệnh với mô tả "chọn và cài ACP adapter vào `~/.july/adapters`, xác minh handshake, ghi `identities.json`", kèm ghi chú màn hình tương tác chỉ chạy trên unix và đường không tương tác dùng `--adapters`.

- [ ] **Step 4: Chạy test để xác nhận pass**

Run: `cargo test --test cli_init`
Expected: PASS, 3 test.

- [ ] **Step 5: Kiểm tra bằng tay màn hình tương tác**

Run: `JULY_HOME=$(mktemp -d) cargo run --bin july -- init`
Expected: hiện danh sách 4 adapter, codex và claude tick sẵn, mũi lên xuống di chuyển con trỏ, space đổi ô tick, dòng "Đã chọn" đổi theo, `q` thoát mà terminal vẫn gõ được chữ bình thường sau đó.

- [ ] **Step 6: Gate chất lượng**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: không cảnh báo, toàn bộ test pass.

- [ ] **Step 7: Commit**

```bash
git add src/cli/init.rs src/cli/mod.rs tests/cli_init.rs docs/08-RUNTIME-AND-CLI.md
git commit -m "feat(cli): add july init to select, install, and verify adapters"
```

---

### Task 9: `agent add --adapter`

Đổi ý nghĩa flag: `--adapter` từ nay trỏ id trong danh mục, không còn là `transport_type`. Chỗ escape cho cấu hình lạ chuyển sang `--transport <type> --config <file>`. Project ở 0.1.0 chưa phát hành nên rename này chấp nhận được, nhưng phải phản ánh trong USAGE và tài liệu.

**Files:**

- Modify: `src/cli/mod.rs:395-406` (`AgentOperation::Add`), `:592-620` (`parse_agent_add`), `:1683-1708` (`run_agent`)
- Modify: `docs/08-RUNTIME-AND-CLI.md`
- Test: `tests/cli_agent.rs`

**Interfaces:**

- Consumes: `AdapterStore::config_for` (Task 4), `CliError::Adapter` (Task 8).
- Produces: `AgentOperation::Add { name, project, runtime, transport, adapter: Option<String>, config: Option<PathBuf> }`.

- [ ] **Step 1: Viết test thất bại**

Thêm vào `tests/cli_agent.rs` (theo đúng helper đang có trong file đó cho `JULY_WORKSPACE_DB`):

```rust
#[tokio::test]
async fn agent_add_rejects_an_acp_agent_without_an_adapter_or_config() {
    let database = scratch_database();
    unsafe { std::env::set_var("JULY_WORKSPACE_DB", &database) };
    let project = std::env::temp_dir();

    let error = july_workspace::cli::run(arguments(&[
        "agent",
        "add",
        "agent_order",
        "--project",
        &project.to_string_lossy(),
    ]))
    .await
    .expect_err("acp agent without config is rejected");

    assert!(error.to_string().contains("july init"), "got {error}");
}

#[tokio::test]
async fn agent_add_rejects_adapter_and_config_together() {
    let database = scratch_database();
    unsafe { std::env::set_var("JULY_WORKSPACE_DB", &database) };

    assert!(
        july_workspace::cli::run(arguments(&[
            "agent",
            "add",
            "agent_order",
            "--project",
            "/tmp",
            "--adapter",
            "codex",
            "--config",
            "/tmp/config.json",
        ]))
        .await
        .is_err()
    );
}

#[tokio::test]
async fn agent_add_rejects_an_adapter_that_was_never_verified() {
    let database = scratch_database();
    let home = std::env::temp_dir().join(format!("july-agent-add-{}", ulid::Ulid::generate()));
    std::fs::create_dir_all(&home).expect("home");
    unsafe { std::env::set_var("JULY_WORKSPACE_DB", &database) };
    unsafe { std::env::set_var("JULY_HOME", &home) };

    let error = july_workspace::cli::run(arguments(&[
        "agent",
        "add",
        "agent_order",
        "--project",
        "/tmp",
        "--adapter",
        "codex",
    ]))
    .await
    .expect_err("unverified adapter is rejected");

    assert!(error.to_string().contains("july init"), "got {error}");
}
```

- [ ] **Step 2: Chạy test để xác nhận thất bại**

Run: `cargo test --test cli_agent`
Expected: FAIL - test đầu pass sai lý do hoặc `agent add` vẫn lưu `{}`; test thứ hai và ba fail vì `--adapter` còn mang nghĩa cũ.

- [ ] **Step 3: Viết implementation nhỏ nhất**

Trong `src/cli/mod.rs`, đổi `AgentOperation::Add`:

```rust
    Add {
        name: String,
        project: String,
        runtime: Option<String>,
        transport: String,
        adapter: Option<String>,
        config: Option<PathBuf>,
    },
```

Đổi `parse_agent_add`:

```rust
fn parse_agent_add(
    args: &[String],
) -> Result<(String, Option<String>, String, Option<String>, Option<PathBuf>), CliError> {
    let mut project = None;
    let mut runtime = None;
    let mut transport = None;
    let mut adapter = None;
    let mut config = None;
    let mut index = 0;
    while index < args.len() {
        let Some(value) = args.get(index + 1).filter(|value| !value.starts_with("--")) else {
            return Err(CliError::Usage);
        };
        match args[index].as_str() {
            "--project" if project.is_none() => project = Some(value.clone()),
            "--runtime" if runtime.is_none() => runtime = Some(value.clone()),
            "--transport" if transport.is_none() => transport = Some(value.clone()),
            "--adapter" if adapter.is_none() => adapter = Some(value.clone()),
            "--config" if config.is_none() => config = Some(PathBuf::from(value)),
            _ => return Err(CliError::Usage),
        }
        index += 2;
    }
    // `--adapter` sinh config tự động; `--config` là đường escape. Hai cái loại trừ nhau.
    if adapter.is_some() && config.is_some() {
        return Err(CliError::Usage);
    }
    let project = project.ok_or(CliError::Usage)?;
    Ok((
        project,
        runtime,
        transport.unwrap_or_else(|| "acp".into()),
        adapter,
        config,
    ))
}
```

Cập nhật nhánh gọi trong `parse_agent` (src/cli/mod.rs:568) cho khớp tuple năm phần tử và truyền `adapter` vào `AgentOperation::Add`.

Trong `run_agent`, thay khối tính `transport_config` (src/cli/mod.rs:1690-1694):

```rust
            AgentOperation::Add {
                name,
                project,
                runtime,
                transport,
                adapter,
                config,
            } => {
                let transport_config = match (adapter, config) {
                    (Some(id), None) => {
                        AdapterStore::open_default()?.config_for(&id, &name)?
                    }
                    (None, Some(path)) => serde_json::from_str(&std::fs::read_to_string(path)?)
                        .map_err(|error| CliError::Runtime(error.to_string()))?,
                    // Không bao giờ lưu một agent `acp` với config rỗng: đó là lỗi
                    // khiến `july dm` chết sau khi `agent add` đã báo thành công.
                    (None, None) if transport == "acp" => {
                        return Err(CliError::Adapter(
                            crate::adapter::AdapterError::NotInstalled { id: "acp".into() },
                        ));
                    }
                    (None, None) => json!({}),
                    (Some(_), Some(_)) => return Err(CliError::Usage),
                };
```

Bổ sung `use crate::adapter::AdapterStore;` vào đầu `src/cli/mod.rs`.

Cập nhật `USAGE`:

```
  july agent add <tên> --project <đường dẫn> --adapter <id> [--runtime <tên>]
```

Cập nhật `docs/08-RUNTIME-AND-CLI.md`: mô tả `--adapter` trỏ id danh mục, `--transport` cộng `--config` là đường escape, và hai cái loại trừ nhau.

- [ ] **Step 4: Chạy test để xác nhận pass**

Run: `cargo test --test cli_agent`
Expected: PASS toàn bộ, kể cả các test đã có trong file.

- [ ] **Step 5: Gate chất lượng**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: không cảnh báo, toàn bộ test pass.

- [ ] **Step 6: Commit**

```bash
git add src/cli/mod.rs tests/cli_agent.rs docs/08-RUNTIME-AND-CLI.md
git commit -m "feat(cli): generate agent transport config from a catalog adapter"
```

---

### Task 10: `agent update --adapter`

Không có lệnh này thì một config sai là không sửa được: `agent remove` chỉ đặt `status = "inactive"` (src/application/collaboration.rs:503) nên tên vẫn bị chiếm và tạo lại cùng tên báo `AgentNameConflict`.

**Files:**

- Modify: `src/application/collaboration.rs`
- Modify: `src/cli/mod.rs`
- Modify: `docs/08-RUNTIME-AND-CLI.md`
- Test: `tests/cli_agent.rs`, `tests/phase4_application.rs`

**Interfaces:**

- Consumes: `AdapterStore::config_for` (Task 4), `CollaborationService::resolve_agent` đã có.
- Produces: `CollaborationService::set_agent_transport(&mut self, reference: AgentRef, transport_type: String, transport_config: Value, changed_at: String) -> Result<Agent, CollaborationError>`, `AgentOperation::Update { agent: AgentRef, adapter: Option<String>, config: Option<PathBuf> }`.

- [ ] **Step 1: Viết test thất bại**

Thêm vào `tests/cli_agent.rs`:

```rust
#[tokio::test]
async fn agent_update_rejects_an_unknown_agent() {
    let database = scratch_database();
    unsafe { std::env::set_var("JULY_WORKSPACE_DB", &database) };

    assert!(
        july_workspace::cli::run(arguments(&[
            "agent", "update", "khong-ton-tai", "--adapter", "codex",
        ]))
        .await
        .is_err()
    );
}

#[tokio::test]
async fn agent_update_requires_an_adapter_or_a_config() {
    let database = scratch_database();
    unsafe { std::env::set_var("JULY_WORKSPACE_DB", &database) };

    assert!(
        july_workspace::cli::run(arguments(&["agent", "update", "agent_order"]))
            .await
            .is_err()
    );
}
```

Thêm test tầng application vào `tests/phase4_application.rs` (file đó đã có helper mở `StorageWorker` trên database tạm), kiểm chứng đúng việc `set_agent_transport` thay config tại chỗ:

```rust
#[tokio::test]
async fn setting_the_transport_replaces_the_stored_config_in_place() {
    let database = scratch_database();
    let mut service = CollaborationService::new(
        StorageWorker::open(&database).expect("storage worker"),
    );
    let agent_id = AgentId::new();
    service
        .add_agent(AddAgent {
            agent_id,
            name: "agent_order".into(),
            project_root: "/tmp".into(),
            transport_type: "acp".into(),
            transport_config: serde_json::json!({ "executable": "/old/bin" }),
            runtime: None,
            created_at: "2026-08-25T00:00:00Z".into(),
        })
        .await
        .expect("agent added");

    let updated = service
        .set_agent_transport(
            AgentRef::Name("agent_order".into()),
            "acp".into(),
            serde_json::json!({ "executable": "/new/bin" }),
            "2026-08-25T01:00:00Z".into(),
        )
        .await
        .expect("transport replaced");

    assert_eq!(updated.id, agent_id, "cùng agent, không tạo bản ghi mới");
    assert_eq!(updated.transport_config["executable"], "/new/bin");
    assert_eq!(updated.updated_at, "2026-08-25T01:00:00Z");

    let reloaded = service
        .resolve_agent(AgentRef::Name("agent_order".into()))
        .await
        .expect("agent reloaded");
    assert_eq!(
        reloaded.transport_config["executable"], "/new/bin",
        "config mới phải được ghi xuống storage"
    );
}
```

- [ ] **Step 2: Chạy test để xác nhận thất bại**

Run: `cargo test --test cli_agent`
Expected: FAIL, `invalid command` vì `agent update` chưa tồn tại.

- [ ] **Step 3: Viết implementation nhỏ nhất**

Thêm vào `src/application/collaboration.rs`, cạnh `remove_agent`:

```rust
    /// Thay cấu hình transport của một agent đang tồn tại.
    ///
    /// Cần thiết vì `remove_agent` chỉ retire agent, nên một config sai
    /// không thể sửa bằng cách tạo lại cùng tên.
    pub async fn set_agent_transport(
        &mut self,
        reference: AgentRef,
        transport_type: String,
        transport_config: serde_json::Value,
        changed_at: String,
    ) -> Result<Agent, CollaborationError> {
        let mut agent = self.resolve_agent(reference).await?;
        agent.transport_type = transport_type;
        agent.transport_config = transport_config;
        agent.updated_at = changed_at;
        agent
            .validate()
            .map_err(|error| CollaborationError::InvalidCommand(error.to_string()))?;
        self.runtime.update_agent(agent.clone()).await?;
        Ok(agent)
    }
```

Trong `src/cli/mod.rs`, thêm variant vào `AgentOperation`:

```rust
    Update {
        agent: AgentRef,
        adapter: Option<String>,
        config: Option<PathBuf>,
    },
```

Thêm nhánh vào `parse_agent` (src/cli/mod.rs:564), phía trên nhánh `add`:

```rust
        [_, command, agent, rest @ ..] if command == "update" => {
            let (adapter, config) = parse_agent_update(rest)?;
            AgentOperation::Update {
                agent: agent_ref(agent)?,
                adapter,
                config,
            }
        }
```

Thêm parser:

```rust
fn parse_agent_update(args: &[String]) -> Result<(Option<String>, Option<PathBuf>), CliError> {
    let mut adapter = None;
    let mut config = None;
    let mut index = 0;
    while index < args.len() {
        let Some(value) = args.get(index + 1).filter(|value| !value.starts_with("--")) else {
            return Err(CliError::Usage);
        };
        match args[index].as_str() {
            "--adapter" if adapter.is_none() => adapter = Some(value.clone()),
            "--config" if config.is_none() => config = Some(PathBuf::from(value)),
            _ => return Err(CliError::Usage),
        }
        index += 2;
    }
    match (&adapter, &config) {
        (None, None) | (Some(_), Some(_)) => Err(CliError::Usage),
        _ => Ok((adapter, config)),
    }
}
```

Thêm nhánh xử lý trong `run_agent`, cạnh `AgentOperation::Remove`:

```rust
            AgentOperation::Update {
                agent,
                adapter,
                config,
            } => {
                let name = service.resolve_agent(agent.clone()).await?.name;
                let transport_config = match (adapter, config) {
                    (Some(id), None) => AdapterStore::open_default()?.config_for(&id, &name)?,
                    (None, Some(path)) => serde_json::from_str(&std::fs::read_to_string(path)?)
                        .map_err(|error| CliError::Runtime(error.to_string()))?,
                    _ => return Err(CliError::Usage),
                };
                let agent = service
                    .set_agent_transport(agent, "acp".into(), transport_config, timestamp())
                    .await?;
                Some(render_agent(&agent, json_output))
            }
```

Cập nhật `USAGE`:

```
  july agent update <agent> --adapter <id>    đổi adapter của một agent
```

Cập nhật `docs/08-RUNTIME-AND-CLI.md`: thêm `agent update` vào bảng lệnh, nói rõ nó thay `transport_config` tại chỗ, và `agent remove` chỉ retire nên tên vẫn bị chiếm.

- [ ] **Step 4: Chạy test để xác nhận pass**

Run: `cargo test --test cli_agent`
Expected: PASS toàn bộ.

- [ ] **Step 5: Gate chất lượng cuối**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: không cảnh báo, toàn bộ workspace test pass.

- [ ] **Step 6: Commit**

```bash
git add src/application/collaboration.rs src/cli/mod.rs tests/cli_agent.rs docs/08-RUNTIME-AND-CLI.md
git commit -m "feat(cli): add agent update so a wrong transport config is fixable"
```

---

## Nghiệm thu bằng tay sau Task 10

Chạy trên máy dev, không nằm trong test tự động:

1. `JULY_HOME=$(mktemp -d) cargo run --bin july -- init` - chọn `codex`, xác nhận nó cài, probe, và in danh tính thật.
2. `cargo run --bin july -- agent add probe-test --project $(pwd) --adapter codex`
3. `cargo run --bin july -- dm probe-test` - phải mở được, không còn lỗi `field executable must be a non-empty string`.
4. Dọn nợ ở mục 9 của spec: `july agent update agent_order --adapter codex`, rồi xoá `~/.july/workspace.db.bak-*` và `~/.july/agent_order-acp.json`.

## Tự soát kế hoạch

- Spec mục 3 kiến trúc → Task 1, 2, 4, 7 (`catalog`, `store`), Task 8 (`cli/init`), Task 3 (probe trong `transport`). Sai lệch duy nhất so với spec: probe nằm trong `transport` chứ không phải `store`, để giữ bất biến "chỉ transport nói ACP"; `store` gọi qua `transport::probe_agent_identity`.
- Spec mục 4 màn hình → Task 5 (raw mode, phím), Task 6 (state machine), Task 8 (render, non-TTY, `--adapters`).
- Spec mục 5 verify → Task 3 (probe), Task 4 (`identities.json`), Task 8 (ghi sau khi cài).
- Spec mục 6 `agent add` → Task 4 (`config_for`, `state_directory`), Task 9 (flag, loại trừ nhau, chặn config rỗng), Task 10 (`agent update`).
- Spec mục 7 xử lý lỗi → Task 2 (`AdapterError`), Task 7 (`ToolMissing`, `InstallFailed`), Task 8 (giữ adapter đã cài, exit khác 0, `q`/Ctrl-C, hiện bản mới), Task 3 (timeout 30 giây).
- Spec mục 8 kiểm chứng → Task 5 (giải mã phím), Task 6 (state machine), Task 1 (catalog), Task 4 (round-trip), Task 7 (`install_command` tách khỏi việc chạy thật).
