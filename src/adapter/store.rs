//! Quản lý các adapter đã cài dưới `~/.july/adapters`.

use super::{AdapterSpec, Installer};
use serde_json::Value;
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use thiserror::Error;

const IDENTITIES: &str = "identities.json";

/// Danh tính thật của một adapter, ghi lại sau khi `july setup` xác minh handshake.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdapterIdentity {
    pub name: String,
    pub version: String,
    pub bin: PathBuf,
}

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("biến môi trường HOME chưa được thiết lập")]
    MissingHome,
    #[error("adapter `{0}` không có trong danh mục; xem `july setup` để biết danh sách")]
    UnknownAdapter(String),
    #[error("adapter `{id}` chưa được cài; chạy `july setup` để cài")]
    NotInstalled { id: String },
    #[error("tên agent `{0}` không hợp lệ: không được chứa `/` hay `..`")]
    InvalidAgentName(String),
    #[error("adapter `{id}` đã cài nhưng chưa xác minh được danh tính; chạy lại `july setup`")]
    NotVerified { id: String },
    #[error("không tìm thấy `{tool}` trên PATH; cài `{tool}` rồi chạy lại `july setup`")]
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
                format!("{}@{}", spec.package, spec.install_version),
            ],
        ),
        Installer::Cargo => (
            "cargo",
            vec![
                "install".into(),
                spec.package.into(),
                "--version".into(),
                spec.install_version.into(),
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
                if parts.next()? != spec.package {
                    return None;
                }
                Some(parts.next()?.to_owned())
            })
            .next()
    }

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
    pub fn record_identity(&self, id: &str, identity: AdapterIdentity) -> Result<(), AdapterError> {
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
        let identity =
            self.identities()?
                .remove(spec.id)
                .ok_or_else(|| AdapterError::NotVerified {
                    id: spec.id.to_owned(),
                })?;
        if !identity.bin.is_file() {
            return Err(AdapterError::NotInstalled {
                id: spec.id.to_owned(),
            });
        }
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

/// Thư mục state của một agent, july tự tạo trước khi adapter chạy.
pub fn ensure_state_directory(root: &Path, agent_name: &str) -> Result<PathBuf, AdapterError> {
    if agent_name.contains('/') || agent_name.contains('\\') || agent_name.contains("..") {
        return Err(AdapterError::InvalidAgentName(agent_name.to_owned()));
    }
    let directory = root.join(agent_name);
    std::fs::create_dir_all(&directory)?;
    Ok(directory)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::find;

    fn scratch() -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("july-adapter-store-{}", ulid::Ulid::generate()));
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
                "@agentclientprotocol/codex-acp@1.10.0".to_string(),
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
    fn installers_use_the_install_pin_instead_of_the_compatibility_range() {
        for (id, pin, expected) in [
            (
                "codex",
                "1.12.0",
                vec![
                    "install",
                    "--prefix",
                    "/opt/july/adapters",
                    "@agentclientprotocol/codex-acp@1.12.0",
                ],
            ),
            (
                "claude-rust",
                "0.1.23",
                vec![
                    "install",
                    "claude-code-acp-rs",
                    "--version",
                    "0.1.23",
                    "--root",
                    "/opt/july/adapters",
                ],
            ),
        ] {
            let spec = AdapterSpec {
                install_version: pin,
                ..*find(id).expect("adapter")
            };
            let (_, arguments) = install_command(&spec, Path::new("/opt/july/adapters"));
            assert_eq!(arguments, expected, "{id}");
        }
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
            store.config_for("nope", "cashpoint"),
            Err(AdapterError::UnknownAdapter(id)) if id == "nope"
        ));
    }

    #[test]
    fn config_for_rejects_an_adapter_that_was_never_verified() {
        let store = AdapterStore::new(scratch());

        assert!(matches!(
            store.config_for("codex", "cashpoint"),
            Err(AdapterError::NotVerified { id }) if id == "codex"
        ));
    }

    #[test]
    fn config_for_rejects_an_adapter_whose_binary_is_missing() {
        let store = AdapterStore::new(scratch());
        store.record_identity("codex", identity()).expect("record");

        assert!(matches!(
            store.config_for("codex", "cashpoint"),
            Err(AdapterError::NotInstalled { id }) if id == "codex"
        ));
    }

    #[test]
    fn config_for_is_accepted_by_the_runtime_parser() {
        let home = scratch();
        let store = AdapterStore::new(home.clone());
        let bin = home.join("node_modules/.bin/codex-acp");
        std::fs::create_dir_all(bin.parent().unwrap()).expect("bin dir");
        std::fs::write(&bin, "#!/bin/sh\n").expect("fake executable");
        let identity = AdapterIdentity {
            name: "codex-acp".into(),
            version: "1.6.2".into(),
            bin: bin.clone(),
        };
        store.record_identity("codex", identity).expect("record");

        let config = store
            .config_for("codex", "cashpoint")
            .expect("config generated");

        // Đây là hợp đồng bị vỡ trước đây: nơi ghi và nơi đọc phải khớp nhau.
        let parsed = crate::runtime::parse_acp_config(&config).expect("runtime accepts the config");
        assert_eq!(parsed.executable, bin);
        assert_eq!(parsed.expected_agent_name, "codex-acp");
        assert_eq!(parsed.expected_agent_version, "1.6.2");
        assert_eq!(parsed.state_directory, home.join("state/cashpoint"));
        assert!(parsed.arguments.is_empty());
        assert!(parsed.environment.is_empty());
        assert!(
            parsed.state_directory.is_dir(),
            "state directory phải được tạo trước khi adapter chạy"
        );
    }

    #[test]
    fn ensure_state_directory_rejects_a_path_traversing_agent_name() {
        let root = scratch();

        assert!(matches!(
            ensure_state_directory(&root, "../../tmp/x"),
            Err(AdapterError::InvalidAgentName(name)) if name == "../../tmp/x"
        ));
        assert!(matches!(
            ensure_state_directory(&root, "sub/dir"),
            Err(AdapterError::InvalidAgentName(_))
        ));
        assert!(!root.join("../../tmp/x").exists());
    }
}
