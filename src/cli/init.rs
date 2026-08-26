//! Màn hình onboarding cài ACP adapter.

use crate::adapter::{AdapterSpec, Tier};

/// Trạng thái con trỏ và các ô tick của màn hình onboarding.
// ponytail: no caller yet - Task 8 (onboarding screen) drives this next.
#[allow(dead_code)]
pub(crate) struct Selection {
    items: Vec<&'static AdapterSpec>,
    checked: Vec<bool>,
    cursor: usize,
}

#[allow(dead_code)]
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
