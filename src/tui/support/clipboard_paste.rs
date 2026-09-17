//! What the composer needs to know about pasted text and the terminal it is pasted into.
//!
//! Codex's module also turns clipboard images into temp files and un-escapes shell-quoted paths.
//! July's composer attaches no images, so only the two dependency-free helpers survive.
//!
//! ponytail: the image and path halves of `clipboard_paste.rs` stayed behind with their `tempfile`,
//! `shlex`, and `url` dependencies. Port them if the composer ever accepts file attachments.

/// Collapses pasted text into a single-line search query, or `None` if it was only whitespace.
pub(crate) fn normalize_pasted_search_query(pasted: &str) -> Option<String> {
    let normalized = pasted.split_whitespace().collect::<Vec<_>>().join(" ");
    (!normalized.is_empty()).then_some(normalized)
}

/// Whether this process is running under WSL, where the terminal cannot report `Shift+Enter`.
#[cfg(target_os = "linux")]
pub(crate) fn is_probably_wsl() -> bool {
    // /proc/version names the kernel, and WSL's is built by Microsoft.
    if let Ok(version) = std::fs::read_to_string("/proc/version") {
        let version = version.to_lowercase();
        if version.contains("microsoft") || version.contains("wsl") {
            return true;
        }
    }

    // A custom kernel inside WSL leaves /proc/version unmarked, but the interop env stays.
    std::env::var_os("WSL_DISTRO_NAME").is_some() || std::env::var_os("WSL_INTEROP").is_some()
}

/// Whether this process is running under WSL. Always false off Linux.
#[cfg(not(target_os = "linux"))]
pub(crate) fn is_probably_wsl() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::normalize_pasted_search_query;

    #[test]
    fn pasted_query_collapses_every_run_of_whitespace() {
        assert_eq!(
            normalize_pasted_search_query("  find\tthe \n  thing  "),
            Some("find the thing".to_string())
        );
        assert_eq!(normalize_pasted_search_query("word"), Some("word".into()));
    }

    #[test]
    fn whitespace_only_paste_is_not_a_query() {
        assert_eq!(normalize_pasted_search_query("   \n\t "), None);
        assert_eq!(normalize_pasted_search_query(""), None);
    }
}
