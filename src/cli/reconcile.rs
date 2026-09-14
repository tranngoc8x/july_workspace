//! Đưa một adapter về đúng `AdapterSpec` của bản July đang chạy.
//!
//! Hàm này trả về kết quả thay vì tự in ra, vì `july setup` và `july update`
//! báo cáo theo hai bố cục khác nhau nhưng phải chạy đúng một logic.

use crate::adapter::{
    AdapterIdentity, AdapterSpec, AdapterStore, PackageInstaller, VersionDetection, VersionStatus,
};
use crate::transport::probe_agent_identity;

/// Adapter đã ở trạng thái tương thích, cùng với việc July có phải chạm vào nó
/// hay không.
pub(crate) struct RuntimeReport {
    pub(crate) id: &'static str,
    /// Phiên bản trước khi reconcile; `None` khi chưa cài hoặc không đọc được.
    pub(crate) from: Option<String>,
    /// Phiên bản ACP mà executable tự khai sau khi reconcile.
    pub(crate) to: String,
    pub(crate) bin: std::path::PathBuf,
    pub(crate) changed: bool,
}

/// `Err` mô tả lý do adapter chưa dùng được; bản cài đặt cũ không bị kích hoạt.
pub(crate) async fn reconcile_adapter(
    spec: &'static AdapterSpec,
    store: &AdapterStore,
    installer: &impl PackageInstaller,
    search: Option<&std::ffi::OsStr>,
    progress: bool,
) -> Result<RuntimeReport, String> {
    use crate::adapter::{
        AdapterAction, CompatibilityState, InstallationOwnership, plan_adapter, probe_version,
        resolve_executable,
    };
    use std::collections::BTreeMap;
    let (managed, receipt) = store.managed_candidate(spec).map_err(|e| e.to_string())?;
    let selected = resolve_executable(
        spec,
        receipt.as_ref().map(|_| managed.as_path()),
        &managed,
        search,
    )
    .map_err(|e| e.to_string())?;
    let mut detected = match selected {
        Some(bin) => Some(probe_version(spec, bin, &[], &BTreeMap::new()).await),
        None => None,
    };
    let before = detected
        .as_ref()
        .and_then(|detection| match &detection.status {
            VersionStatus::Detected(version) => Some(version.to_string()),
            _ => None,
        });
    let ownership = if receipt.is_some()
        && detected
            .as_ref()
            .is_some_and(|d| d.executable.path == managed)
    {
        InstallationOwnership::JulyManaged
    } else {
        InstallationOwnership::ExternalOrUnknown
    };
    let plan = plan_adapter(spec, detected.as_ref(), ownership)?;
    let mut installed_root = receipt;
    let changed = !matches!(plan.action, AdapterAction::Keep);
    match plan.action {
        AdapterAction::Keep => {}
        AdapterAction::Report => {
            return Err(format!(
                "{:?}, executable {}; cần xử lý thủ công, chưa thay đổi cài đặt",
                plan.state,
                managed_display(detected.as_ref())
            ));
        }
        AdapterAction::Install { version }
        | AdapterAction::Upgrade { version }
        | AdapterAction::Reinstall { version } => {
            // Keep the complete previous installation for existing exact-identity configs.
            let root = std::path::absolute(
                store
                    .adapters_root()
                    .join("installations")
                    .join(spec.id)
                    .join(ulid::Ulid::generate().to_string()),
            )
            .map_err(|error| error.to_string())?;
            if progress {
                println!("{}: cài {} vào {}", spec.id, version, root.display());
            }
            installer.install(spec, &root).map_err(|e| e.to_string())?;
            let bin = AdapterStore::installed_bin(spec, &root);
            let resolved = resolve_executable(spec, Some(&bin), &bin, None)
                .map_err(|e| e.to_string())?
                .expect("explicit candidate");
            detected = Some(probe_version(spec, resolved, &[], &BTreeMap::new()).await);
            // Không tin trình cài: phiên bản được đo lại trước khi kích hoạt.
            let verified =
                plan_adapter(spec, detected.as_ref(), InstallationOwnership::JulyManaged)?;
            if verified.state != CompatibilityState::Compatible {
                return Err(format!(
                    "bản vừa cài không tương thích: {}; chưa kích hoạt",
                    version_description(&detected.as_ref().expect("post-install detection").status)
                ));
            }
            installed_root = Some(root);
        }
    }
    let detection = detected.expect("compatible detection");
    let after = version_description(&detection.status);
    let bin = detection.executable.path;
    let identity = probe_agent_identity(&bin, &[])
        .await
        .map_err(|error| format!("không xác minh được ACP: {error}; chưa kích hoạt"))?;
    store
        .record_verified(
            spec,
            AdapterIdentity {
                name: identity.name,
                version: identity.version,
                bin: bin.clone(),
            },
            installed_root
                .as_deref()
                .filter(|root| AdapterStore::installed_bin(spec, root) == bin),
        )
        .map_err(|e| e.to_string())?;
    Ok(RuntimeReport {
        id: spec.id,
        from: before,
        to: after,
        bin,
        changed,
    })
}

fn managed_display(detected: Option<&VersionDetection>) -> String {
    detected
        .map(|d| d.executable.path.display().to_string())
        .unwrap_or_else(|| "missing".into())
}

pub(crate) fn version_description(status: &VersionStatus) -> String {
    match status {
        VersionStatus::Detected(version) => version.to_string(),
        VersionStatus::UnknownVersion => "UnknownVersion".into(),
        VersionStatus::Broken(reason) => format!("Broken: {reason}"),
    }
}

#[cfg(all(test, unix))]
mod reconciliation_tests {
    use super::reconcile_adapter;
    use crate::adapter::{
        AdapterError, AdapterIdentity, AdapterSpec, AdapterStore, PackageInstaller,
    };
    use crate::transport::probe_agent_identity;
    use std::{cell::Cell, os::unix::fs::PermissionsExt, path::Path};
    struct Fixture {
        store: AdapterStore,
        installs: Cell<usize>,
        version: &'static str,
    }
    impl Fixture {
        fn new(version: &'static str) -> Self {
            Self {
                store: AdapterStore::new(
                    std::env::temp_dir().join(format!("july-setup-{}", ulid::Ulid::generate())),
                ),
                installs: Cell::new(0),
                version,
            }
        }
        fn binary(&self, path: &Path, version: &str) {
            let (version, args) = if version == "acp-broken" {
                ("1.10.0", "--protocol-zero")
            } else {
                (version, "")
            };
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, format!("#!/bin/sh\nif [ \"$1\" = --version ]; then echo '{version}'; exit; fi\nexec python3 '{}/tests/fixtures/acp_agent.py' {args}\n", env!("CARGO_MANIFEST_DIR"))).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(self.store.adapters_root().parent().unwrap());
        }
    }
    impl PackageInstaller for Fixture {
        fn install(&self, spec: &AdapterSpec, root: &Path) -> Result<(), AdapterError> {
            self.installs.set(self.installs.get() + 1);
            self.binary(&AdapterStore::installed_bin(spec, root), self.version);
            Ok(())
        }
    }
    #[tokio::test]
    async fn setup_keeps_compatible_path_and_managed_executables() {
        let spec = crate::adapter::find("codex").unwrap();
        for managed in [false, true] {
            let f = Fixture::new("1.10.0");
            let path = if managed {
                f.store.bin_path(spec)
            } else {
                f.store.state_root().join(spec.bin)
            };
            f.binary(&path, "1.12.0");
            reconcile_adapter(
                spec,
                &f.store,
                &f,
                Some(path.parent().unwrap().as_os_str()),
                false,
            )
            .await
            .unwrap();
            assert_eq!(f.installs.get(), 0);
            let config = f.store.config_for(spec.id, "test").unwrap();
            assert_eq!(config["executable"], path.to_str().unwrap());
            assert_eq!(config["expected_agent_version"], "1.0.0");
        }
    }
    #[tokio::test]
    async fn setup_installs_missing_and_rejects_bad_postinstall_version() {
        let spec = crate::adapter::find("codex").unwrap();
        for version in ["1.10.0", "1.6.2", "unknown"] {
            let f = Fixture::new(version);
            let result = reconcile_adapter(spec, &f.store, &f, None, false).await;
            assert_eq!(result.is_ok(), version == "1.10.0");
            assert_eq!(f.installs.get(), 1);
            if result.is_ok() {
                reconcile_adapter(spec, &f.store, &f, None, false)
                    .await
                    .unwrap();
                assert_eq!(
                    f.installs.get(),
                    1,
                    "repeat setup keeps activated installation"
                );
            }
            assert_eq!(
                f.store.identities().unwrap().contains_key(spec.id),
                version == "1.10.0"
            );
        }
    }
    #[tokio::test]
    async fn setup_reports_unowned_bad_candidates_without_fallback() {
        let spec = crate::adapter::find("codex").unwrap();
        for version in ["1.6.2", "2.0.0", "unknown"] {
            let f = Fixture::new("1.10.0");
            f.binary(&f.store.bin_path(spec), version);
            let fallback = f.store.state_root().join(spec.bin);
            f.binary(&fallback, "1.12.0");
            assert!(
                reconcile_adapter(
                    spec,
                    &f.store,
                    &f,
                    Some(fallback.parent().unwrap().as_os_str()),
                    false
                )
                .await
                .is_err()
            );
            assert_eq!(f.installs.get(), 0);
            assert!(f.store.identities().unwrap().is_empty());
        }
    }
    #[tokio::test]
    async fn owned_replacement_preserves_old_launch_and_activation_is_verified() {
        let spec = crate::adapter::find("codex").unwrap();
        for old_version in ["1.6.2", "broken", "missing"] {
            for new_version in ["1.10.0", "2.0.0", "acp-broken"] {
                let f = Fixture::new(new_version);
                let old_root = f.store.adapters_root().join("installations/codex/old");
                let old_bin = AdapterStore::installed_bin(spec, &old_root);
                f.binary(&old_bin, old_version);
                if old_version == "broken" {
                    std::fs::write(&old_bin, "#!/bin/sh\nexit 1\n").unwrap();
                }
                f.store
                    .record_verified(
                        spec,
                        AdapterIdentity {
                            name: "test-acp-agent".into(),
                            version: "1.0.0".into(),
                            bin: old_bin.clone(),
                        },
                        Some(&old_root),
                    )
                    .unwrap();
                let old_config = f.store.config_for(spec.id, "old").unwrap();
                let before = std::fs::read(&old_bin).unwrap();
                if old_version == "missing" {
                    std::fs::remove_file(&old_bin).unwrap();
                }
                let result = reconcile_adapter(spec, &f.store, &f, None, false).await;
                assert_eq!(result.is_ok(), new_version == "1.10.0");
                assert_eq!(f.installs.get(), 1);
                if old_version == "missing" {
                    assert!(!old_bin.exists());
                } else {
                    assert_eq!(std::fs::read(&old_bin).unwrap(), before);
                }
                let active = f.store.managed_candidate(spec).unwrap();
                assert_eq!(active.0 == old_bin, new_version != "1.10.0");
                if old_version == "1.6.2" {
                    let parsed = crate::runtime::parse_acp_config(&old_config).unwrap();
                    let identity = probe_agent_identity(&parsed.executable, &parsed.arguments)
                        .await
                        .unwrap();
                    assert_eq!(identity.name, parsed.expected_agent_name);
                    assert_eq!(identity.version, parsed.expected_agent_version);
                }
            }
        }
    }
}
