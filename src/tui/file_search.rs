//! Finding workspace files for the composer's `@` popup.
//!
//! Codex runs this through a separate crate built on `ignore` and `nucleo`. July walks the tree with
//! `std::fs` and scores with the same fuzzy matcher the command popup uses, because the search only
//! has to be good enough to pick a file out of one repository while someone is typing.
//!
//! The walk is bounded three ways - directory depth, directories visited, and matches kept - so a
//! query typed into a huge tree costs a predictable amount of work instead of blocking the UI.
//!
//! ponytail: no gitignore parsing. `SKIPPED_DIRECTORIES` covers what actually clutters a result list.
//! Add the `ignore` crate if per-repository ignore rules start mattering.

use std::cmp::Reverse;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use crate::tui::support::fuzzy_match::fuzzy_match;

/// Directories never worth offering, and expensive to walk.
const SKIPPED_DIRECTORIES: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    ".venv",
    "venv",
    "__pycache__",
    "dist",
    "build",
    ".next",
    ".idea",
];

/// How deep below the workspace root the walk goes.
const MAX_DEPTH: usize = 12;

/// How many directories one search may open, so a pathological tree still returns.
const MAX_DIRECTORIES: usize = 4_000;

/// Whether a match is a file or a directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchType {
    File,
    Directory,
}

/// One workspace path matching a query, with the character positions that matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMatch {
    /// Higher scores sort first.
    pub score: u32,
    /// Path relative to `root`.
    pub path: PathBuf,
    pub match_type: MatchType,
    /// Workspace root the walk started from.
    pub root: PathBuf,
    /// Indices into `path` that the query matched, for highlighting. Sorted and deduplicated.
    pub indices: Option<Vec<u32>>,
}

/// Finds up to `limit` paths under `root` matching `query`, best match first.
///
/// An empty query returns nothing: the popup has nothing useful to show until something is typed.
pub fn search(root: &Path, query: &str, limit: usize) -> Vec<FileMatch> {
    if query.is_empty() || limit == 0 {
        return Vec::new();
    }

    let mut matches = Vec::new();
    for (path, match_type) in walk(root) {
        let Some(relative) = path.strip_prefix(root).ok().map(Path::to_path_buf) else {
            continue;
        };
        let haystack = relative.to_string_lossy();
        let Some((indices, score)) = fuzzy_match(&haystack, query) else {
            continue;
        };
        matches.push(FileMatch {
            score: score.max(0) as u32,
            path: relative,
            match_type,
            root: root.to_path_buf(),
            indices: Some(indices.into_iter().map(|index| index as u32).collect()),
        });
    }

    // Best score first; equal scores fall back to the shorter, then alphabetically earlier, path so
    // repeated searches are stable.
    matches.sort_by_key(|candidate| {
        (
            Reverse(candidate.score),
            candidate.path.as_os_str().len(),
            candidate.path.clone(),
        )
    });
    matches.truncate(limit);
    matches
}

/// Walks `root` breadth-first within the depth and directory budgets.
fn walk(root: &Path) -> Vec<(PathBuf, MatchType)> {
    let mut found = Vec::new();
    let mut queue = VecDeque::from([(root.to_path_buf(), 0usize)]);
    let mut directories_visited = 0usize;

    while let Some((directory, depth)) = queue.pop_front() {
        if directories_visited >= MAX_DIRECTORIES {
            break;
        }
        directories_visited += 1;

        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            // Symlinks are followed by neither branch, so a link loop cannot stall the walk.
            if file_type.is_dir() {
                if is_skipped(&path) {
                    continue;
                }
                found.push((path.clone(), MatchType::Directory));
                if depth + 1 < MAX_DEPTH {
                    queue.push_back((path, depth + 1));
                }
            } else if file_type.is_file() {
                found.push((path, MatchType::File));
            }
        }
    }

    found
}

fn is_skipped(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| SKIPPED_DIRECTORIES.contains(&name))
}

#[cfg(test)]
mod tests {
    use super::{MatchType, search};
    use std::fs;
    use std::path::{Path, PathBuf};

    /// Builds a throwaway tree under the test's own temp directory.
    fn workspace(name: &str, files: &[&str]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("july-file-search-{name}"));
        let _ = fs::remove_dir_all(&root);
        for file in files {
            let path = root.join(file);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("create parent");
            }
            fs::write(&path, b"").expect("write file");
        }
        root
    }

    fn paths(root: &Path, query: &str) -> Vec<String> {
        search(root, query, 20)
            .into_iter()
            .map(|found| found.path.to_string_lossy().replace('\\', "/"))
            .collect()
    }

    #[test]
    fn a_subsequence_query_finds_nested_files() {
        let root = workspace("nested", &["src/tui/app.rs", "src/storage/sqlite.rs"]);

        let found = paths(&root, "app.rs");

        assert_eq!(found, vec!["src/tui/app.rs"]);
    }

    #[test]
    fn noisy_directories_are_never_offered() {
        let root = workspace(
            "skipped",
            &["target/debug/build.rs", "node_modules/pkg/build.rs", "build.rs"],
        );

        let found = paths(&root, "build.rs");

        assert_eq!(found, vec!["build.rs"]);
    }

    #[test]
    fn directories_match_too_and_are_labelled() {
        let root = workspace("dirs", &["storage/migrations/001.sql"]);

        let migrations = search(&root, "migrations", 20);

        assert_eq!(migrations.first().map(|found| found.match_type), Some(MatchType::Directory));
    }

    #[test]
    fn an_empty_query_returns_nothing() {
        let root = workspace("empty", &["a.rs"]);

        assert!(search(&root, "", 20).is_empty());
    }

    #[test]
    fn results_are_capped_and_ordered_shortest_first_on_ties() {
        let root = workspace("cap", &["a/x.rs", "b/x.rs", "x.rs"]);

        let found = search(&root, "x.rs", 2);

        assert_eq!(found.len(), 2);
        assert_eq!(found[0].path.to_string_lossy(), "x.rs");
    }

    #[test]
    fn a_missing_root_yields_no_matches_instead_of_an_error() {
        let root = std::env::temp_dir().join("july-file-search-does-not-exist");
        let _ = std::fs::remove_dir_all(&root);

        assert!(search(&root, "anything", 20).is_empty());
    }
}
