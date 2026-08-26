//! Màn hình onboarding cài ACP adapter.

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
pub(crate) async fn run_init(adapters: Option<Vec<String>>) -> Result<(), CliError> {
    let store = AdapterStore::open_default()?;
    let chosen = match adapters {
        Some(ids) => resolve_ids(&ids)?,
        None => match RawMode::enable()? {
            Some(guard) => {
                let chosen = interactive_select(&guard, &store)?;
                drop(guard);
                let Some(chosen) = chosen else {
                    return Ok(());
                };
                chosen
            }
            None => {
                println!(
                    "stdin không phải terminal, dùng mặc định: codex, claude.\n\\
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
                println!("  đã xác minh: {} {}", identity.name, identity.version);
                store.record_identity(
                    spec.id,
                    AdapterIdentity {
                        name: identity.name,
                        version: identity.version,
                        bin,
                    },
                )?;
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
            crate::adapter::find(id.trim()).ok_or_else(|| {
                CliError::Adapter(crate::adapter::AdapterError::UnknownAdapter(id.clone()))
            })
        })
        .collect()
}

/// `None` means the user cancelled; an empty selection means Enter on no ticks.
fn interactive_select(
    _guard: &RawMode,
    store: &AdapterStore,
) -> Result<Option<Vec<&'static AdapterSpec>>, CliError> {
    let mut selection = Selection::new(ADAPTERS.iter().collect());
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 16];
    let mut stdin = std::io::stdin();
    let mut first = true;

    loop {
        render(&selection, store, first)?;
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

fn render(selection: &Selection, store: &AdapterStore, first: bool) -> Result<(), CliError> {
    let mut out = std::io::stdout();
    if !first {
        write!(out, "\x1b[{}A", selection.items().len() + 4)?;
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
}
