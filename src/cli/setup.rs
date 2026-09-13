//! Màn hình `july setup` cài ACP adapter.

use super::CliError;
use super::keys::{Key, RawMode, decode};
use crate::adapter::{
    ADAPTERS, AdapterIdentity, AdapterSpec, AdapterStore, PackageInstaller, SystemInstaller, Tier,
};
use crate::transport::probe_agent_identity;
use std::io::{Read, Write};

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

/// Chạy màn hình onboarding, hoặc đi đường không tương tác khi được chỉ định.
pub(crate) async fn run_setup(adapters: Option<Vec<String>>) -> Result<(), CliError> {
    let store = AdapterStore::open_default()?;
    let chosen = match adapters {
        Some(ids) => resolve_ids(&ids)?,
        None => match RawMode::enable()? {
            Some(guard) => {
                let chosen = interactive_select(&guard)?;
                drop(guard);
                let Some(chosen) = chosen else {
                    return Ok(());
                };
                chosen
            }
            None => {
                println!(
                    "stdin không phải terminal, dùng mặc định: codex, claude.\n\
                     Chỉ định khác bằng july setup --adapters <ids>"
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
    let search = std::env::var_os("PATH");
    for spec in chosen {
        if let Err(error) = reconcile_adapter(spec, &store, &installer, search.as_deref()).await {
            println!("  {}: {error}", spec.id);
            failures.push(spec.id);
        }
    }

    if failures.is_empty() {
        println!("Xong. Tạo agent cho thư mục hiện tại bằng: july init");
        return Ok(());
    }
    Err(CliError::Runtime(format!(
        "các adapter sau chưa dùng được: {}. Chạy lại july setup để thử tiếp",
        failures.join(", ")
    )))
}

fn resolve_ids(ids: &[String]) -> Result<Vec<&'static AdapterSpec>, CliError> {
    ids.iter()
        .map(|id| {
            crate::adapter::find(id.trim()).ok_or_else(|| {
                CliError::Adapter(crate::adapter::AdapterError::UnknownAdapter(id.clone()))
            })
        })
        .collect()
}

/// `None` means the user cancelled; an empty selection means Enter on no ticks.
fn interactive_select(_guard: &RawMode) -> Result<Option<Vec<&'static AdapterSpec>>, CliError> {
    let mut selection = Selection::new(ADAPTERS.iter().collect());
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 16];
    let mut stdin = std::io::stdin();
    let mut first = true;

    loop {
        render(&selection, first)?;
        first = false;
        let read = stdin.read(&mut chunk)?;
        if read == 0 {
            return Ok(None);
        }
        buffer.extend_from_slice(&chunk[..read]);
        while let Some((key, used)) = decode(&buffer) {
            buffer.drain(..used);
            match key {
                Key::Up => selection.up(),
                Key::Down => selection.down(),
                Key::Space => selection.toggle(),
                Key::Enter => return Ok(Some(selection.chosen())),
                Key::Quit | Key::Interrupt => return Ok(None),
                Key::Other => {}
            }
        }
    }
}

fn render(selection: &Selection, first: bool) -> Result<(), CliError> {
    let mut out = std::io::stdout();
    if !first {
        write!(out, "\x1b[{}A", redraw_rows(selection.items().len()))?;
    }
    write!(
        out,
        "\rJuly cần ít nhất một ACP adapter. Chọn adapter để cài:\r\n\r\n"
    )?;
    for (index, spec) in selection.items().iter().enumerate() {
        let pointer = if index == selection.cursor() {
            "❯"
        } else {
            " "
        };
        let tick = if selection.is_checked(index) {
            "x"
        } else {
            " "
        };
        write!(
            out,
            "  {pointer} [{tick}] {:<13}{}\r\n",
            spec.id, spec.summary
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

fn redraw_rows(items: usize) -> usize {
    items + 5
}

async fn reconcile_adapter(
    spec: &AdapterSpec,
    store: &AdapterStore,
    installer: &impl PackageInstaller,
    search: Option<&std::ffi::OsStr>,
) -> Result<(), CliError> {
    use crate::adapter::{
        AdapterAction, CompatibilityState, InstallationOwnership, plan_adapter, probe_version,
        resolve_executable,
    };
    use std::collections::BTreeMap;
    let (managed, receipt) = store.managed_candidate(spec)?;
    let selected = resolve_executable(
        spec,
        receipt.as_ref().map(|_| managed.as_path()),
        &managed,
        search,
    )?;
    let mut detected = match selected {
        Some(bin) => Some(probe_version(spec, bin, &[], &BTreeMap::new()).await),
        None => None,
    };
    if let Some(detection) = &detected {
        println!(
            "{}: {} {}",
            spec.id,
            detection.executable.path.display(),
            version_description(&detection.status)
        );
    }
    let ownership = if receipt.is_some()
        && detected
            .as_ref()
            .is_some_and(|d| d.executable.path == managed)
    {
        InstallationOwnership::JulyManaged
    } else {
        InstallationOwnership::ExternalOrUnknown
    };
    let plan = plan_adapter(spec, detected.as_ref(), ownership).map_err(CliError::Runtime)?;
    let mut installed_root = receipt;
    match plan.action {
        AdapterAction::Keep => println!("{}: giữ executable tương thích", spec.id),
        AdapterAction::Report => {
            return Err(CliError::Runtime(format!(
                "{}: {:?}, executable {}; cần xử lý thủ công, chưa thay đổi cài đặt",
                spec.id,
                plan.state,
                managed_display(detected.as_ref())
            )));
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
            )?;
            println!("{}: cài {} vào {}", spec.id, version, root.display());
            installer.install(spec, &root)?;
            let bin = AdapterStore::installed_bin(spec, &root);
            let resolved =
                resolve_executable(spec, Some(&bin), &bin, None)?.expect("explicit candidate");
            detected = Some(probe_version(spec, resolved, &[], &BTreeMap::new()).await);
            let verified =
                plan_adapter(spec, detected.as_ref(), InstallationOwnership::JulyManaged)
                    .map_err(CliError::Runtime)?;
            if verified.state != CompatibilityState::Compatible {
                return Err(CliError::Runtime(format!(
                    "{}: bản vừa cài không tương thích: {}; chưa kích hoạt",
                    spec.id,
                    version_description(&detected.as_ref().expect("post-install detection").status)
                )));
            }
            installed_root = Some(root);
        }
    }
    let bin = detected.expect("compatible detection").executable.path;
    let identity = probe_agent_identity(&bin, &[]).await.map_err(|error| {
        CliError::Runtime(format!(
            "{}: không xác minh được ACP: {error}; chưa kích hoạt",
            spec.id
        ))
    })?;
    store.record_verified(
        spec,
        AdapterIdentity {
            name: identity.name,
            version: identity.version,
            bin: bin.clone(),
        },
        installed_root
            .as_deref()
            .filter(|root| AdapterStore::installed_bin(spec, root) == bin),
    )?;
    println!(
        "{}: đã xác minh {}; cấu hình agent hiện có được giữ nguyên",
        spec.id,
        bin.display()
    );
    Ok(())
}

fn managed_display(detected: Option<&crate::adapter::VersionDetection>) -> String {
    detected
        .map(|d| d.executable.path.display().to_string())
        .unwrap_or_else(|| "missing".into())
}

fn version_description(status: &crate::adapter::VersionStatus) -> String {
    use crate::adapter::VersionStatus;
    match status {
        VersionStatus::Detected(version) => version.to_string(),
        VersionStatus::UnknownVersion => "UnknownVersion".into(),
        VersionStatus::Broken(reason) => format!("Broken: {reason}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::{ADAPTERS, find};

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

    #[test]
    fn redraw_moves_back_over_every_rendered_row() {
        assert_eq!(redraw_rows(catalog().len()), catalog().len() + 5);
    }
}

#[cfg(all(test, unix))]
mod reconciliation_tests {
    use super::*;
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
        fn install(
            &self,
            spec: &AdapterSpec,
            root: &Path,
        ) -> Result<(), crate::adapter::AdapterError> {
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
            reconcile_adapter(spec, &f.store, &f, Some(path.parent().unwrap().as_os_str()))
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
            let result = reconcile_adapter(spec, &f.store, &f, None).await;
            assert_eq!(result.is_ok(), version == "1.10.0");
            assert_eq!(f.installs.get(), 1);
            if result.is_ok() {
                reconcile_adapter(spec, &f.store, &f, None).await.unwrap();
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
                    Some(fallback.parent().unwrap().as_os_str())
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
                let result = reconcile_adapter(spec, &f.store, &f, None).await;
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
