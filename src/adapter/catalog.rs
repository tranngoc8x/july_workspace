//! Static catalog of supported ACP adapters.

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
