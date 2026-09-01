# TUI Context History Replay Design

**Date:** 2026-09-01
**Status:** Approved in chat
**Bead:** `JULY_WORKSPACE-sca.4`

## Goal

When the full-screen TUI enters or switches to a DM or Work-backed Thread,
replace the visible transcript with that conversation's 50 most recent stored
messages. Keep context isolation, stale-result protection, live streaming, and
all non-TTY contracts unchanged.

This is presentation hydration only. Stored history is never resent to an
agent and does not participate in ACP session recovery.

## Current State

- `SqliteStore::list_recent_messages_after` already reads the newest bounded
  message set and returns it in chronological `(created_at, id)` order with a
  truncation flag.
- `StorageHandle` exposes only the unbounded `list_messages` path. Recovery
  calls the bounded store primitive internally with its own limit of 20.
- Opening a DM already obtains stored messages, but `open_repl_dm` discards
  them. Opening a Thread returns identifiers and a live chat service only.
- The TUI owns one `MarkdownStream`. Successful navigation changes the typed
  `Context`, but it does not reset or hydrate the transcript.
- `CommandFinished` carries the originating `ContextId`; the reducer rejects a
  result unless it matches the current pending command. The CLI bridge is
  single-flight and preserves event order.

Appending stored history to the existing transcript would mix contexts and
duplicate messages. A separate asynchronous history event would require a
second stale guard and could race live deltas.

## Scope

### In scope

- Hydrate the TUI when navigation enters a DM or Work-backed Thread.
- Display at most 50 recent messages for the exact conversation ID.
- Replace the prior transcript atomically with the new context and history.
- Render stored user and agent messages with the existing transcript styles.
- When older messages were truncated, show the exact first line
  `… showing 50 most recent messages …` using `SYSTEM_COLOR`.
- Clear conversational history when navigation reaches Root or Room.
- Hydrate mention-driven context entry before the new live turn is rendered.
- Preserve the current context transition even when history loading fails,
  while showing the failure in the footer.
- Add focused storage, reducer, bridge, and compatibility regressions.

### Out of scope

- Pagination, search, transcript export, or per-context scroll restoration.
- Transcript caching or background loading.
- Schema, migration, dependency, ACP, session-binding, or recovery changes.
- Replaying stored messages to an agent.
- Changing standalone `july dm`, line REPL, finite CLI, or JSON output.
- Configuring the 50-message presentation limit.

## Approved Approach

Fetch history synchronously as part of successful navigation and return one
presentation-only context snapshot through the existing `CommandFinished`
event. The reducer checks the existing originating-context guard before it
changes either the breadcrumb or transcript, then applies both atomically.

Do not add a `HistoryLoaded` event, generation counter, loading state, or
per-context cache. The new hydration query is bounded, and the existing bridge
is already single-flight.

## Data Ownership and Interfaces

### Bounded storage access

Add a crate-private `StorageHandle` operation that forwards to the existing
bounded query:

```text
list_recent_messages(conversation_id, limit)
    -> (messages in chronological order, truncated)
```

The storage layer owns ordering and limiting. The CLI must not call unbounded
`list_messages` and truncate in memory. The limit policy belongs to the TUI
bridge as a fixed `50`; recovery keeps its independent limit and semantics.

DM opening currently performs its own unbounded history read because
`OpenedDirectMessage.messages` is part of the standalone DM contract. The TUI
hydration path intentionally performs one additional bounded read instead of
changing that established contract or plumbing the unbounded vector into the
presentation path. Removing the existing DM read belongs to a separate API
change only if profiling shows it matters.

### Presentation projection

Domain `Message` values must not enter TUI reducer state. Before emitting a
result, the CLI projects them into a small presentation DTO containing only:

```text
HistoryEntry { author: User | Agent, body }
History { entries, truncated }
ContextSnapshot {
    context,
    history: Result<History, String>,
    history_fallback: Option<HistoryEntry>
}
```

No message IDs, sender IDs, metadata, timestamps, storage handles, sessions,
or provider objects cross the presentation boundary.

### Command results

Navigation results carry a `ContextSnapshot` instead of a bare `Context`.
Mention submission may carry an optional snapshot because it can both enter a
new context and start a live turn. Its snapshot carries the stripped submitted
prompt as `history_fallback`, used only if the history read fails after a
successful send. A failed command may also carry an optional snapshot when
navigation succeeded before the later send failed; that snapshot has no
fallback because the error does not prove persistence succeeded. Ordinary chat
submission and failures before navigation carry none.

Inspection command output remains unchanged and does not reload history.

## Navigation and Hydration Flow

```text
TUI submits command from origin ContextId
  -> REPL performs deterministic navigation
  -> project the new typed Context
  -> for DM/Thread: read newest 50 messages by conversation_id
  -> for Root/Room: produce empty history
  -> emit CommandFinished(origin, snapshot)
  -> reducer rejects stale origin, or applies snapshot atomically
```

Explicit `/dm`, `/work`, `/thread`, and `/back` navigation use this flow.
Mention routing that enters a context returns the same snapshot with the
submitted result; the reducer installs it without changing the active-turn
state before subsequent live deltas arrive. For a non-empty mention prompt,
the CLI attempts the send first, then reads history, then emits the result.
Because both DM and Thread send paths persist before calling the transport,
the snapshot contains the stripped prompt exactly once whenever persistence
succeeded. It does not retain the original `@agent` routing syntax rendered
under the old context.

If sending fails after navigation, the CLI still reads history and returns the
new snapshot with the send error. The snapshot therefore displays the prompt
only when storage contains it, changes the breadcrumb to match the live REPL
context, and leaves the turn idle with the error in the footer. A persistence
failure produces no stored prompt and therefore no replayed prompt.

Footer error precedence is deterministic: a command or send error wins over a
simultaneous history-read error; a history-read error is shown only when no
command/send error exists. Errors use the existing sanitized CLI strings and
are not concatenated into a second compound error format.

History loading is TUI-only: the CLI performs it only when an originating TUI
context exists. Existing non-TTY paths do no extra read and retain their
current output.

## Transcript Replacement

Applying a valid snapshot resets only conversation presentation state:

- replace the current typed context;
- replace the `MarkdownStream` with a fresh stream;
- reset transcript scroll to the tail;
- render stored entries once in chronological order;
- leave the editor contents and prompt history unchanged.

Stored user messages use the current `› ` prefix and user color. Each stored
agent body goes through the existing Markdown stream and is finalized as one
completed response, preserving paragraphs, lists, quotes, and fenced code.
If `truncated` is true, the first line is exactly
`… showing 50 most recent messages …` in `SYSTEM_COLOR`.

Root and Room snapshots contain empty history, so leaving a conversation does
not expose its transcript under a non-conversation breadcrumb.

## Error and Race Semantics

### Stale results

The reducer validates the originating `ContextId` before touching the context
or transcript. A stale snapshot changes neither and reports the same stale
command error used today. No second identity or generation counter is added.

### History read failure

Navigation has already succeeded before hydration runs. Therefore a history
read failure must not leave the TUI showing the old breadcrumb or old
transcript while the REPL is operating in the new context.

The snapshot carries the new context plus the read error. The reducer changes
context, clears the transcript, returns to the result's correct turn state,
and exposes the sanitized error in the footer. If this follows a successful
mention send, the turn stays active and live chat remains usable; the reducer
renders the snapshot's stripped submitted prompt as a user entry fallback
because successful send proves persistence completed. If the send failed, the
turn stays idle and no fallback is rendered when history is unavailable.

### Live events

The snapshot is emitted before live turn events for mention-driven entry.
Existing FIFO delivery preserves this order. Stored history never creates
`ChatEvent`s that could activate a turn, request permission, or trigger
persistence.

## Testing Strategy

### Storage tests

- The worker forwards the bounded request to SQLite.
- More than 50 stored messages returns only the newest 50.
- Returned messages remain chronological and ties remain stable by ID.
- The truncation flag is correct.
- Only the requested conversation is returned.

Existing recovery tests continue to prove the independent 20-message capsule
behavior.

### Reducer tests

- A matching snapshot replaces context A's transcript with context B's
  history exactly once.
- User and agent entries render in chronological order with their existing
  presentation styles.
- A truncated snapshot shows `… showing 50 most recent messages …` first in
  `SYSTEM_COLOR`.
- Root and Room snapshots clear conversational history.
- A stale snapshot containing sentinel content changes neither context nor
  transcript.
- A failed history load changes to the new context, clears the old transcript,
  and reports the error.
- A submitted mention snapshot replaces the old-context input echo with the
  single persisted stripped prompt and keeps the turn active.
- A mention send failure after navigation changes context, replays the prompt
  only if it was persisted, stays idle, and reports the send error.
- When send and history reads both fail, the footer reports only the sanitized
  send error; history error never masks the primary operation failure.

### Bridge and compatibility tests

- DM and Thread entry hydrate by their exact conversation ID.
- DM A -> Thread B -> `/back` never mixes or duplicates transcripts.
- Mention entry installs stored history before live deltas.
- Ordinary chat in an unchanged context does not reload history.
- Standalone DM, non-TTY REPL, finite CLI, and JSON behavior remain unchanged.
- Hydration adds no ACP send, recovery-capsule creation, or session replacement
  call. Mention entry still performs exactly its existing single prompt send.

## Verification Gates

Run focused tests through the red-green cycle, then finish with:

```bash
cargo fmt --all -- --check
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets --all-features -- -D warnings
git diff --check
```

Final review must compare the changed TUI path with non-TTY behavior and check
that only the selected conversation can contribute hydrated entries.

## Dependency-Ordered Implementation Slices

1. Expose the existing bounded SQLite query through `StorageHandle` and prove
   ordering, limit, truncation, and conversation isolation.
2. Add the presentation snapshot types and reducer replacement behavior using
   test-first red-green cycles.
3. Project DM/Thread history into navigation and mention results without
   changing non-TTY execution.
4. Add end-to-end context-switch and compatibility regressions.
5. Run focused and full gates, review the complete diff for scope drift, and
   close `JULY_WORKSPACE-sca.4` only when every acceptance criterion passes.

Only after `JULY_WORKSPACE-sca.4` closes may the dependency chain advance to
failed-delivery inspection and manual retry.
