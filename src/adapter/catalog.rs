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
    /// Khoảng phiên bản adapter mà July chấp nhận, theo cú pháp SemVer.
    pub version_req: &'static str,
    /// Phiên bản pin chính xác để cài khi cần; phải thỏa `version_req`.
    pub install_version: &'static str,
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
        version_req: ">=1.10.0, <2.0.0",
        install_version: "1.10.0",
        bin: "codex-acp",
        installer: Npm,
        tier: Core,
        summary: "Codex qua @agentclientprotocol/codex-acp",
    },
    AdapterSpec {
        id: "claude",
        package: "@agentclientprotocol/claude-agent-acp",
        version_req: ">=0.70.0, <0.71.0",
        install_version: "0.70.0",
        bin: "claude-agent-acp",
        installer: Npm,
        tier: Core,
        summary: "Claude Code qua @agentclientprotocol/claude-agent-acp",
    },
    AdapterSpec {
        id: "claude-rust",
        package: "claude-code-acp-rs",
        version_req: ">=0.1.22, <0.2.0",
        install_version: "0.1.22",
        bin: "claude-code-acp-rs",
        installer: Cargo,
        tier: Optional,
        summary: "Claude Code bản Rust, không cần node (biên dịch ~2 phút)",
    },
    AdapterSpec {
        id: "deepseek",
        package: "@openma/deepseek-harness-acp",
        version_req: ">=0.4.26, <0.5.0",
        install_version: "0.4.26",
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
    use semver::{Version, VersionReq};
    use std::collections::HashSet;

    #[test]
    fn every_adapter_has_complete_metadata() {
        for spec in ADAPTERS {
            assert!(!spec.id.is_empty(), "adapter without id");
            assert!(!spec.package.trim().is_empty(), "{} lacks package", spec.id);
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
    fn install_versions_are_stable_semver_within_their_requirements() {
        for spec in ADAPTERS {
            let pin = Version::parse(spec.install_version).expect("exact semantic version");
            let requirement = VersionReq::parse(spec.version_req).expect("valid requirement");
            assert!(
                pin.pre.is_empty(),
                "{} must install a stable version",
                spec.id
            );
            assert!(
                requirement.matches(&pin),
                "{} install version {} must satisfy {}",
                spec.id,
                pin,
                spec.version_req
            );
        }
    }

    #[test]
    fn catalog_requirements_accept_compatible_releases_within_explicit_bounds() {
        for (id, accepted, rejected) in [
            (
                "codex",
                ["1.10.0", "1.10.1", "1.12.0", "1.99.0"],
                ["1.9.9", "2.0.0", "1.10.0-rc.1", "1.12.0-beta.1"],
            ),
            (
                "claude",
                ["0.70.0", "0.70.1", "0.70.99", "0.70.1+build.1"],
                ["0.69.9", "0.71.0", "1.0.0", "0.70.1-rc.1"],
            ),
            (
                "claude-rust",
                ["0.1.22", "0.1.23", "0.1.99", "0.1.23+build.1"],
                ["0.1.21", "0.2.0", "1.0.0", "0.1.23-rc.1"],
            ),
            (
                "deepseek",
                ["0.4.26", "0.4.27", "0.4.99", "0.4.27+build.1"],
                ["0.4.25", "0.5.0", "1.0.0", "0.4.27-rc.1"],
            ),
        ] {
            let requirement = VersionReq::parse(find(id).expect("adapter").version_req)
                .expect("valid requirement");
            for (versions, expected) in [(accepted, true), (rejected, false)] {
                for version in versions {
                    assert_eq!(
                        requirement.matches(&Version::parse(version).expect("version")),
                        expected,
                        "{id}: {version} against {requirement}"
                    );
                }
            }
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
