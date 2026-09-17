//! Rendering, wrapping, and key-binding primitives the composer is built on.
//!
//! Ported from `codex-rs/tui/src` with the Codex product surface removed: config-driven keymap
//! overlays, terminal-title and status plumbing, and syntax highlighting all stay behind. What is
//! left is the generic terminal toolkit `bottom_pane` needs - measure a string, wrap a line, name
//! a key, render something that knows its own height.

// ponytail: this is a toolkit, and the composer uses the parts it needs. Porting only the reachable
// half would mean re-deriving the cut lines on every later feature, so the unused parts stay.
#![allow(dead_code)]

pub(crate) mod clipboard_paste;
pub(crate) mod color;
pub(crate) mod footer_hint;
pub(crate) mod fuzzy_match;
pub(crate) mod key_hint;
pub(crate) mod keymap;
pub(crate) mod line_truncation;
pub(crate) mod render;
pub(crate) mod style;
pub(crate) mod terminal_detection;
pub(crate) mod terminal_hyperlinks;
pub(crate) mod terminal_palette;
pub(crate) mod text_formatting;
pub(crate) mod tui_keymap;
pub(crate) mod ui_consts;
pub(crate) mod vim_search;
pub(crate) mod width;
pub(crate) mod wrapping;
