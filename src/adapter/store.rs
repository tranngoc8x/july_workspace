//! Quản lý các adapter đã cài dưới `~/.july/adapters`.

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
                if parts.next()? != spec.package {
                    return None;
                }
                Some(parts.next()?.to_owned())
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
}
