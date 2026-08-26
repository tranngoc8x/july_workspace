# Phase 10 Interactive TUI Design

Status: approved for Phase 10.0

Beads: `JULY_WORKSPACE-m5e`

Canonical scope: `docs/11-IMPLEMENTATION-ROADMAP.md`, Phase 10

## Goal

Replace only the no-argument, interactive-terminal REPL presentation with a
full-screen TUI. Agent replies remain visible while they stream and render as
terminal Markdown: paragraphs wrap, quotes and lists are structured, and code
fences become styled code blocks instead of literal fence markers.

The TUI is a presentation layer over the Phase 8 runtime and application
services. It must not merge conversations, replay transcripts, or create a
second ACP ownership model.

## Entry contract

Dispatch is determined before terminal initialization:

| Invocation | stdin/stdout | Behavior |
|---|---|---|
| `july` | both TTY | Phase 10 TUI |
| `july` | either non-TTY | existing line REPL |
| `july dm ...` | any | existing standalone stream |
| `july thread open ...` | any | existing standalone stream |
| finite command or `--json` | any | existing CLI output |

This keeps scripts, pipes, snapshots, and machine-readable output byte
compatible. Phase 10 changes no named command grammar.

## Scope

Phase 10 implements:

- one alternate-screen terminal shell with deterministic restoration;
- a scrollable transcript, context/status line, multiline editor, and help
  hint;
- progressive Markdown rendering for DM and Thread replies;
- existing Root, Room, DM, and Thread navigation inside the TUI;
- an exclusive permission modal and explicit active-turn cancellation;
- resize, follow-tail, and manual scroll behavior.

Phase 10 does not add:

- a daemon, GUI, web server, mouse workflow, theme/plugin system, or terminal
  multiplexer dependency;
- syntax highlighting in the first delivery; fenced code still receives a
  distinct block style;
- new slash commands, semantic routing, model selection, or Agent management;
- ACP/schema/storage changes, transcript transfer, or full transcript replay;
- TUI behavior for standalone `july dm` or `july thread open`.

## Architecture

The dependency direction remains:

```text
terminal input + ChatEvent
          |
          v
     TUI App reducer ---> Ratatui view
          |
          v
existing REPL controller/services ---> WorkspaceRuntime ---> ACP/storage
```

The TUI consumes July-owned `ChatEvent` values. Provider types, transport
handles, session bindings, Messages, and recovery capsules do not enter UI
state. The current process-level `WorkspaceRuntime<AcpTransport>` remains the
single per-Agent owner, and the existing context descriptor stack remains the
source of routing and isolation truth.

Expected file boundary:

```text
src/tui/mod.rs       terminal lifecycle and async event loop
src/tui/app.rs       App state, AppEvent, reducer, and commands
src/tui/ui.rs        pure Ratatui layout/rendering
src/tui/markdown.rs  streamed Markdown block state and rendering
src/cli/mod.rs       TTY dispatch and existing controller integration
```

No interface or factory is introduced solely for the TUI. Existing concrete
services are reused; only controller logic that must serve both the legacy
line REPL and TUI may move behind a presentation-neutral function.

## Dependency boundary

Phase 10.0 may add only these direct responsibilities:

- `ratatui`: terminal model, widgets, layout, and test backend;
- `crossterm` with `event-stream`: terminal input and resize events;
- `tui-markdown` without syntax-highlighting defaults: Markdown-to-Ratatui
  spans;
- `ratatui-textarea`: Unicode multiline editing and cursor movement;
- `futures-util`: async polling of Crossterm's event stream;
- `pulldown-cmark`: source offsets used to identify stable top-level Markdown
  blocks without writing a Markdown parser.

Versions are pinned in the implementation slice after one minimal compile and
MSRV check. No ANSI renderer is part of the TUI path. In particular, the
rejected stdout repaint prototype and `markdown-to-ansi` stay removed.

## App state and event loop

One `App` owns presentation state:

```text
context label and descriptor projection
completed transcript blocks
current streamed assistant response
textarea state
scroll offset + follow_tail
permission modal, active turn, or cancelling state
at most one pending application command with its originating context identity
status/error line
exit request
```

`AppEvent` is the closed set of inputs to the reducer:

```text
terminal key / resize / frame tick
ChatEvent
command success / failure
shutdown result
```

The reducer returns small commands such as submit exact text, execute slash
command, answer permission, cancel turn, or exit. I/O runs outside the reducer
and feeds its result back as an event. This keeps state transitions testable
without a real terminal or ACP provider.

Application commands are single-flight. While submit or a context-changing
slash command is pending, the editor cannot submit another command. Every
result carries the originating context identity; a stale result is displayed
as an error and cannot replace the active descriptor. Phase 10 does not add a
general concurrent command scheduler.

The loop uses a dirty flag and a roughly 30 Hz frame cap. Consecutive text
deltas are concatenated in a bounded batch before reduction. Each loop polls
terminal input before consuming at most one chat batch, so a sustained stream
cannot starve input, resize, permission, or cancellation. No token delta forces
one terminal draw. Input and terminal events mark the frame dirty as well.

## Progressive Markdown model

Each assistant response keeps:

```text
immutable rendered blocks + unresolved raw Markdown tail
```

After a delta, the Markdown parser's source offsets identify completed
top-level blocks. Every completed block is rendered once into owned Ratatui
text and removed from the mutable tail. The final top-level block remains
mutable until a later block proves its boundary or `MessageCompleted` closes
the response. This handles chunk boundaries inside emphasis, inline links,
quotes, lists, and fences without interpreting individual deltas as Markdown.

The streaming contract is deliberately block-local. Reference-style links,
footnotes, and any extension whose later definition could reinterpret an
earlier block are rendered literally; they do not resolve across frozen block
boundaries. Boundary parsing and rendering use pinned compatible CommonMark
options, and differential tests compare chunked and one-shot rendering under
this July subset. This prevents later input from changing a frozen prefix.

Only the unresolved tail is re-rendered on a frame. Completed messages and
completed blocks never repaint. On `MessageCompleted`, the remaining tail is
rendered and frozen. On `TurnFailed` or `Disconnected`, received text is
frozen first, then a separate error block is appended.

Fenced code is visually distinct and excludes opening/closing fence markers.
Block quotes use a visible quote marker/style. Long lines wrap to the current
viewport width. Syntax highlighting is deliberately deferred.

## Viewport and input behavior

The transcript is application-owned scrollback because alternate-screen
terminal history is not reliable across terminals.

- New content follows the tail only while `follow_tail` is true.
- PageUp or upward scrolling disables follow-tail.
- End or downward scrolling at the bottom restores follow-tail.
- Resize recomputes wrapping and clamps the offset without changing content.
- Tiny terminals show a bounded fallback instead of panicking.

`ratatui-textarea` owns text editing. July intercepts only application keys:

| Key | Idle | Active turn | Permission modal |
|---|---|---|---|
| Enter | submit editor | no second submit | choose highlighted option |
| Alt+Enter | newline | newline remains local | ignored |
| Shift+Enter | newline only when terminal reports it distinctly | same | ignored |
| Ctrl-C | clear nonempty editor; otherwise exit | send cancel once; another press exits TUI without resending | cancel active turn |
| Ctrl-D | exit when editor empty | no forced detach | ignored |
| PageUp/PageDown/End | scroll | scroll | modal navigation only |

Exact submitted text, including leading slash behavior, stays governed by the
Phase 8 context grammar. A failed slash command is shown as a non-fatal status
and does not alter the current descriptor.

## Permission, cancellation, and shutdown

A permission request opens a centered modal containing the prompt and every
choice. The first choice is selected by default; Up/Down changes selection,
Enter answers, and Esc sends the existing reject response. Prompt and choices
wrap inside the modal, PageUp/PageDown scroll overflow, and tiny terminals show
a clipped fallback. An empty choice list is a protocol error and returns the
editor to a usable state. While the modal is open, keys are routed only to it,
so option keys cannot leak into the editor or transcript.

Ctrl-C during an active turn transitions once to `Cancelling` and sends one
cancel command. A second Ctrl-C in either `Cancelling` or
`CancelAcknowledged` exits the TUI without sending another cancel, so a hung
delivery cannot trap the terminal. Delivery failure before that second press
returns to `Active` with a visible error, so the next Ctrl-C retries.
Successful delivery transitions to `CancelAcknowledged`; completion, failure,
or disconnect then returns the editor to a usable state while preserving
received output. During a permission modal, Esc rejects the permission choice
while Ctrl-C cancels the active turn.

Terminal ownership follows one bracket:

```text
initialize terminal
run application loop
attempt restore on every return path
report operation and restore failures without hiding either
```

An RAII guard restores raw mode, cursor, and alternate screen on every
controlled return or unwind, before workspace shutdown is allowed to wait.
Normal exit, EOF, initialization failure after partial setup, reducer error,
runtime error, and unwind panic use that bracket. `panic=abort`, SIGKILL, and
external suspend/resume are not recoverable guarantees. SIGTERM and SIGHUP are
handled as exit events through the same restore bracket. Workspace shutdown
still happens through the current application/runtime path after terminal
restore.

## Delivery slices

| Slice | Bead | Independently verified output | Dependencies |
|---|---|---|---|
| 10.0 Contract | `JULY_WORKSPACE-m5e.1` | Design, roadmap, clean baseline, dependency boundary | none |
| 10.1 Terminal shell | `JULY_WORKSPACE-m5e.2` | Inactive shell, lifecycle and PTY restoration tests | 10.0 |
| 10.2 App reducer/input | `JULY_WORKSPACE-m5e.3` | Reducer, textarea, viewport and TestBackend tests | 10.1 |
| 10.3 Markdown viewport | `JULY_WORKSPACE-7cg` | Progressive Markdown and adversarial chunk tests | 10.2 |
| 10.4 Chat integration | `JULY_WORKSPACE-m5e.4` | Existing contexts/services routed through inactive TUI | 10.2 |
| 10.5 Permission/cancel | `JULY_WORKSPACE-m5e.5` | Modal, cancel-once, failure recovery; enable TTY dispatch | 10.3, 10.4 |
| 10.6 Closure | `JULY_WORKSPACE-m5e.6` | Compatibility, docs, review, full gates | 10.5 |

Slices 10.3 and 10.4 may proceed independently after 10.2. Each slice begins
with a failing focused regression and receives its own green commit. No slice
claims later-slice behavior.

## Verification strategy

Focused tests must prove:

- TTY dispatch selects the TUI and non-TTY dispatch preserves the line REPL;
- normal, error, cancellation, and panic paths attempt terminal restoration;
- PTY child-process checks prove normal, error, Ctrl-C, SIGTERM, and SIGHUP
  exits leave the terminal outside raw/alternate-screen mode;
- reducer behavior for Unicode editing, submit, resize, scrolling, and
  follow-tail;
- adversarial delta splits inside paragraphs, emphasis, quotes, lists, links,
  and fenced code converge to the same completed rendering;
- completed blocks remain byte-for-byte stable while only the tail changes;
- raw fence markers are absent and quote styling is visible;
- permission keys are exclusive and active-turn Ctrl-C sends one cancel;
- cancel failure is retryable, a missing terminal event remains escapable, and
  a sustained delta stream cannot starve input or a permission modal;
- a hung cancel delivery is escapable without sending a duplicate cancel;
- pending context commands are single-flight and stale results cannot replace
  the active descriptor;
- DM/Thread switching preserves distinct bindings, histories, exact input,
  descriptor rollback, and recovery behavior;
- named commands, JSON output, standalone streams, and non-TTY REPL snapshots
  remain compatible.

Phase closure requires fresh successful output from:

```text
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
cargo test --all-targets --all-features
cargo build --release
test -x target/release/july
git diff --check
```

Live-provider smoke remains opt-in. Source-level and simulated terminal tests
must not be reported as proof of live provider behavior.

## Rollback boundary

Before Phase 10 closure, removing TTY dispatch and the `src/tui` module returns
`july` to the Phase 8 line REPL without a data migration. No durable format or
runtime ownership change is allowed to make that rollback unsafe.
