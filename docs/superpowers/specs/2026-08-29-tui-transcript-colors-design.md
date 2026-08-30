# TUI Transcript Colors Design

Status: approved

Beads: `JULY_WORKSPACE-006`

## Goal

Make transcript sources distinguishable on a dark terminal without changing
chat, command, Markdown streaming, viewport, or persistence behavior.

## Palette

| Content | Foreground |
|---|---|
| Agent response body | `#d0d7de` |
| User input | `#79c0ff` |
| Command output | `#7ee787` |
| System/status | `#e3b341` |
| Error/disconnect | `#ff7b72` |
| Inline and fenced code | `#0dcdcd` |

Markdown-specific styles such as headings, links, quotes, and code override the
agent body color. Existing header, spinner, and turn labels use the system
palette where they already communicate status.

## Design

Classification stays at the existing reducer branches: submitted input,
streamed agent Markdown, command output, status, and errors. No content-based
heuristics or transport/schema changes are introduced.

The existing `MarkdownStream` remains the transcript owner. It stores rendered
blocks with their Ratatui styles and keeps only the current agent Markdown tail
mutable. A small style mapping supplies the palette; it is not a configurable
theme system.

## Verification

Focused tests assert the foreground color for every existing content source,
that code uses `#0dcdcd`, and that Markdown semantic styles still win. Run the
relevant TUI tests, formatting, and the repository's Rust quality gates.

