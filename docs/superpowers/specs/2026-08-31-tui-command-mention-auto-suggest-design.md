# TUI Command and Mention Auto-Suggest Design

**Date:** 2026-08-31
**Status:** Approved in chat
**Bead:** `JULY_WORKSPACE-sjc`

## Goal

Add one consistent, keyboard-first auto-suggest experience for slash commands
and `@mention` agent names in the full-screen TUI without duplicating command
grammar or changing multiline editing and prompt-history behavior.

## Current State

- `src/cli/registry.rs` is the canonical command catalog. Each command already
  carries its canonical name, aliases, scope, summary, usage, and examples.
- `registry::visible_for_scope` already removes commands unavailable in the
  current context and commands intentionally hidden from user-facing help.
- `src/tui/app.rs` already derives `@mention` candidates from the current agent
  snapshot and completes their longest common prefix with `Tab`.
- `src/tui/ui.rs` already renders mention candidates in the one-line footer.
- Plain `Up` and `Down` first move through real or soft-wrapped input rows and
  only fall back to prompt history at editor boundaries.
- Plain `Enter` currently submits; modified Enter variants insert a newline.

## Scope

### In scope

- Auto-suggest canonical slash commands valid in the active Root, Room, DM, or
  Work context.
- Keep the existing `@mention` candidate source and make command and mention
  completion share the same completion semantics.
- Render both candidate types in the existing footer.
- Let both `Tab` and plain `Enter` complete an active suggestion without
  dispatching a command.
- Preserve command parsing and execution in the CLI bridge.
- Add focused reducer, registry, bridge, and rendered-buffer regressions.

### Out of scope

- Fuzzy matching, ranking, recency, or personalization.
- Command argument completion.
- Alias or hidden-command advertisement.
- Popup, selection index, `Up`/`Down` candidate navigation, or animation.
- New dependencies, configuration, persistence, or schema changes.
- Changes to non-TTY CLI behavior or command execution semantics.

## Approved Approach

Reuse the existing one-line completion footer and `Tab` completion path. The
CLI supplies the active context's visible canonical command names to the TUI;
the TUI derives candidates from the current input and only edits the input.
It never resolves or executes commands.

This keeps the completion path presentation-only and avoids introducing a
second command catalog, a popup state machine, or new key ownership.

## Data Ownership and Interfaces

### Command registry

`src/cli/registry.rs` remains the only source of command names and scope rules.
The CLI maps `ReplContext::scope()` through `registry::visible_for_scope` and
copies only the canonical names needed by the presentation layer.

Aliases and entries hidden from help are still executable through the existing
parser but are not completion candidates.

### TUI context

The typed TUI `Context` carries its existing identity and label plus the
canonical command candidates for that exact context. A successful navigation
result replaces the context label and candidates atomically. The existing
context-ID stale-result guard therefore also protects completion metadata.

The TUI must not infer a command scope from a label or `ContextId`, and it must
not import or duplicate command parsing rules.

### Agent candidates

The existing `AppEvent::Agents` snapshot remains the source for `@mention`
candidates. No persistence or refresh behavior changes are required.

## Candidate Detection

### Slash commands

Slash suggestions are active only when all of these conditions hold:

1. The input contains one logical line.
2. After leading whitespace, the input starts with `/`.
3. At least one visible canonical command starts with the complete remaining
   input prefix, including any internal spaces.

Leading whitespace remains compatible with the existing command parser.
Trailing whitespace is not removed when matching: after a unique completion
appends one space, no canonical command starts with that longer value, so the
suggestion closes and the next plain `Enter` submits.

This prefix rule naturally supports multiword canonical commands while hiding
suggestions after the input has entered an argument that is not part of a
canonical command name.

### Mentions

Mention detection and agent filtering retain their existing behavior. Slash
and mention candidates are mutually exclusive because their prefixes differ.

## Completion Semantics

`Tab` and plain `Enter` call the same completion operation whenever candidates
are active:

- With one candidate, insert the unmatched suffix and exactly one trailing
  space, then close suggestions.
- With several candidates, insert their additional longest common prefix.
- If several candidates have no additional common prefix, leave the input
  unchanged, consume the key, and never choose the first candidate implicitly.
- Completion emits no `AppCommand` and does not activate a turn.
- Once suggestions are inactive, plain `Enter` follows the existing submit
  path. Modified Enter variants retain their existing newline behavior.

The first plain `Enter` can therefore accept a unique suggestion, while the
next plain `Enter` submits the completed input.

## Key Precedence

The reducer keeps existing higher-priority behavior and adds completion only
at the narrow point where `Tab` and plain `Enter` are handled:

1. An active permission modal remains keyboard-exclusive.
2. Existing cancellation and control-key handling remains unchanged.
3. `Up` and `Down` retain native multiline movement and history fallback.
4. `Tab` completes an active slash or mention suggestion; otherwise it keeps
   its current no-op behavior.
5. Plain `Enter` completes active suggestions; otherwise it submits.
6. Modified Enter variants continue to insert newlines.
7. All remaining keys continue through `ratatui-textarea`.

No candidate selection state is introduced, so candidate completion cannot
intercept `Up`, `Down`, or `Esc`.

## Rendering and Errors

The existing one-row footer remains the only suggestion surface:

- An active error has highest priority.
- Otherwise active slash or mention candidates render with the existing cyan
  completion treatment and an `Enter/Tab` hint.
- Otherwise the current default footer renders unchanged.

Candidate text may be clipped by the terminal width exactly like the current
mention footer; this design does not add scrolling or wrapping that would alter
transcript geometry.

An empty candidate set is normal, not an error. A stale navigation result must
not replace either the visible context or its command candidates. Permission
modal behavior and error handling remain unchanged.

## Testing Strategy

### Registry and context tests

- Root, Room, DM, and Work candidates come from
  `registry::visible_for_scope`.
- Hidden commands, aliases, and unavailable commands are absent.
- A successful context switch replaces candidates with the new context.
- A stale context result changes neither context nor candidates.

### Reducer tests

- `/` and partial canonical names produce the expected candidates.
- A multiword command can be completed from a partial canonical prefix.
- `Tab` and plain `Enter` share longest-common-prefix behavior.
- A unique candidate completes with exactly one trailing space and emits no
  command.
- The next plain `Enter` emits the normal submit command.
- Ambiguous candidates with no longer common prefix consume `Tab` or `Enter`
  without modifying input or dispatching.
- Multiline slash input and command arguments do not expose command
  suggestions.
- Existing mention completion tests remain green and gain an Enter regression.
- Existing multiline, soft-wrap cursor, history, single-flight, permission,
  and modified-Enter regressions remain green.

### Rendering and bridge tests

- A Ratatui `TestBackend` fixture verifies slash candidates, mention
  candidates, error priority, and the default footer.
- Existing tiny-terminal fixtures prove the one-row footer remains bounded.
- CLI bridge fixtures verify Root/Room/DM/Work command metadata and ensure the
  execution path still uses the existing registry resolver.

## Verification Gates

Run the smallest focused tests during TDD, then finish with:

```bash
cargo fmt --all -- --check
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets --all-features -- -D warnings
git diff --check
```

Because the behavior changes plain `Enter`, final verification must explicitly
demonstrate the two-press unique-completion flow and the ambiguous no-op case.

## Dependency-Ordered Implementation Slices

1. Thread visible canonical command names through the typed CLI-to-TUI context
   boundary and cover scope/context replacement.
2. Generalize the existing mention completion operation and add slash
   candidate detection plus `Tab`/`Enter` reducer behavior.
3. Update the shared footer hint and add rendered-buffer and bridge
   regressions.
4. Run focused and full gates, review the complete diff for scope drift, and
   close the Bead only after all acceptance criteria pass.

Each slice must preserve existing CLI dispatch, multiline navigation, history,
permission handling, and non-TTY behavior.
