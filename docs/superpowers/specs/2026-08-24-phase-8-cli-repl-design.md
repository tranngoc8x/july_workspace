# Phase 8 CLI / REPL Design

Status: approved for Phase 8.0

Beads: `JULY_WORKSPACE-4ec`

Canonical scope: `docs/11-IMPLEMENTATION-ROADMAP.md`, Phase 8

## Goal

Make common July workflows available from one terminal without merging the
underlying DM and Thread model contexts. Phase 8 adds a broader operational
CLI and a presentation-only REPL over the application/runtime boundaries that
already own durable state and ACP sessions.

## Scope

Phase 8 implements:

- the seven roadmap slash commands: `/dm`, `/room`, `/thread`, `/back`,
  `/members`, `/status`, and `/publish`;
- the Room and Thread operational commands deferred by Phase 4;
- `july publish <result-id> --to <conversation-id>`;
- machine-readable output for finite operational commands;
- one process-level REPL that can move between isolated Room, DM, and Thread
  presentation contexts.

`july dm <agent>` remains backward compatible, including exact message-body
submission, permission handling, cancellation, persisted history, and graceful
binding disconnect on exit. Its input grammar does not become the REPL grammar:
standalone `july dm` continues to treat every non-blank line except `/quit` as
an exact message body, including lines such as `/status` or `/dm pay`.

## Non-goals

Phase 8 does not add:

- Phase 6.5 deliberation or Decision workflows;
- Agent, Work, or Session shell catalogs not named by the Phase 8 roadmap;
- Phase 9 packaging, checksums, installers, or Homebrew support;
- a TUI, GUI, daemon, Herdr/Zellij/tmux dependency, or background retry loop;
- semantic target routing, an LLM facilitator, or inferred Agent/Thread names;
- transcript transfer, shared model context, vector memory, or exactly-once
  delivery;
- a new CLI framework or general-purpose output abstraction.

## Architecture

The dependency direction remains:

```text
CLI / REPL -> application services -> runtime/storage and AgentTransport
```

The CLI never writes SQLite directly. Operational Room and Thread commands use
`CollaborationService`; Publish uses `PublishService`; interactive DM and
Thread contexts use the shared `WorkspaceRuntime`. Existing exact reference
resolution, membership validation, publish invariants, permission handling,
and recovery behavior remain authoritative.

The current one-Agent `open_acp_direct_message` bootstrap evolves into one
ACP-specific runtime-boundary bootstrap that owns a single
`WorkspaceRuntime<AcpTransport>` for the process. It lazily resolves and
validates an Agent's persisted ACP configuration, constructs its transport,
and registers exactly one owner before opening that Agent's first DM or Thread.
Later opens reuse the registered owner. A failed resolve, validation, transport
construction, or registration leaves no cached owner and does not disturb any
other Agent owner. Agent configuration is not hot-reloaded inside one REPL
process.

The existing manual argument parser is sufficient for the approved surface.
Phase 8 adds no `clap` dependency. JSON objects are assembled at the
presentation boundary; domain records do not gain broad `Serialize` derives
solely for CLI output.

## Operational shell grammar

```text
july dm <agent>

july room create <name> [--description <text>]
july room list
july room members <room>
july room member add <room> <agent>
july room member remove <room> <agent>

july thread create <title> --room <room> [--goal <text>] [--member <agent>]...
july thread list --room <room>
july thread members <thread-id>
july thread member add <thread-id> <agent>
july thread member remove <thread-id> <agent>
july thread open <thread-id> --agent <agent>

july publish <result-id> --to <conversation-id>
```

`<room>` and `<agent>` resolve only by exact case-sensitive name or canonical
typed ID. `<thread-id>`, `<result-id>`, and `<conversation-id>` are canonical
typed IDs; titles are never identifiers. `thread open` addresses exactly one
explicit active Agent and never broadcasts or auto-joins.

Create commands generate their IDs in the presentation layer and return all
durable IDs created. Membership commands return `active` or `left` plus
whether durable state changed. Publish remains reference-only and idempotent
for the existing Result/target natural key.

`--json` may appear once anywhere after the program name on a finite
operational command. It is rejected for `july`, `july dm`, and `thread open`
because those commands stream an interactive session.

Unknown flags, duplicate singleton flags, missing values, extra positional
arguments, invalid UTF-8, and invalid typed IDs are usage errors and cause no
durable mutation.

## REPL grammar

Running `july` with no arguments enters the workspace REPL:

```text
/dm <agent>
/room <room>
/thread <thread-id> --agent <agent>
/back
/members
/status
/publish <result-id>
/quit
```

References follow the same exact-resolution rules as the operational shell.
`/thread` may be entered from any context. When the current context is a Room,
the selected Thread must belong to that Room; otherwise the command fails
without changing context.

`/publish` requires a current DM or Thread because Publish targets a
`ConversationId`; a Room is not a Conversation. It publishes to the current
conversation. The explicit shell form remains available for any target
conversation.

`/members` lists active members only in a Room or Thread. It is an error at the
root or in a DM. `/status` reports the current presentation context and, for a
DM or Thread, the current binding/session lifecycle state. It does not infer
Work status or aggregate unrelated Agent status.

`/quit` exits the process. A recognized slash command is handled by July. Any
other non-blank line, including an unknown leading-slash line, is submitted
unchanged when a DM or Thread is active; at the root or in a Room it is a
non-fatal REPL error. This preserves the Phase 3 exact-message contract without
inventing an escape syntax.

Interactive Thread chat adds one narrow application boundary parallel to DM:
`ThreadChatService<R: ThreadChatRuntime>`. `CollaborationService` remains the
owner of operational Room/Thread commands, while the existing
`AgentThreadRuntime` implements the new chat port. The port contains only the
interactive operations Phase 8 needs: open, send exact user Message, receive
the next July-owned event, respond to permission, cancel the active turn, and
detach/shutdown.

`ThreadChatService` uses `local-user` as the local sender, persists the exact
outbound Message before transport submission, and persists the completed
inbound Agent Message. Thread metadata uses the same July-owned
channel/direction convention as DM with `channel = thread`. Persistence failure
prevents transport submission; a later transport failure does not erase the
already durable outbound Message. Text, completion, permission, cancellation,
disconnect, and terminal failure events map through the same July-owned
behavior as DM. Parse or context-switch failure creates no Message and starts
no turn.

## Context lifecycle and isolation

The REPL owns a stack of lightweight presentation descriptors:

```text
Root | Room(room_id) | Dm(conversation_id, agent_id)
     | Thread(conversation_id, agent_id)
```

Entering `/room`, `/dm`, or `/thread` pushes a descriptor. `/back` leaves the
current descriptor and restores the previous one. At Root, `/back` reports
`already at root` and keeps the REPL running.

Only the top descriptor is interactive. Leaving a DM or Thread gracefully
detaches and drops its live service before another interactive service opens,
while preserving durable conversation and remote-session state. Lower stack
entries are cold descriptors, never live services. Restoring one creates a new
service handle through the shared workspace and resumes through the same
runtime/recovery rules as an explicit reopen. If detach or open fails, July
retains the prior descriptor as the active presentation context and reports
the failure. The process-level
`WorkspaceRuntime` remains alive until `/quit` or EOF and continues to own at
most one ACP connection task per Agent.

The stack never contains Messages, prompts, transcript fragments, or model
state. Switching contexts cannot copy one Conversation's transcript or
recovery capsule into another Conversation.

## Output contract

Human output is concise terminal text. Successful finite commands write only
their result to stdout. Diagnostics and errors go to stderr. Failure exits
non-zero; success exits zero.

With `--json`, stdout contains exactly one JSON value followed by a newline.
IDs and timestamps are strings; enum/state values use their canonical
lowercase spelling; booleans remain booleans; absent optional values are
`null`. Lists are JSON arrays in the same deterministic ordering supplied by
the application/storage contract.

Representative success shapes are:

```json
{"room_id":"..."}
```

```json
{"state":"active","changed":true}
```

```json
{"thread_id":"...","primary_work_id":"..."}
```

```json
{"publish_id":"...","result_id":"...","source_conversation_id":"...","target_conversation_id":"...","published_at":"..."}
```

On `--json` failure, stdout remains empty and stderr contains one JSON object:

```json
{"error":{"code":"room_not_found","message":"room vna does not exist"}}
```

Error codes are stable lowercase snake-case presentation identifiers mapped
from typed application/runtime errors. The human-readable message may gain
context without changing the code.

REPL and interactive chat do not support JSON framing in Phase 8. Their
streamed prompts, text deltas, permission choices, and non-fatal command errors
remain human-readable.

## Error and shutdown behavior

- Parse and reference errors happen before mutation or transport startup.
- A failed context switch leaves the previous descriptor active.
- A non-fatal slash-command failure prints an error and keeps the REPL alive.
- EOF and Ctrl-C follow the current fail-closed permission/cancellation rules.
- Process exit attempts current-context shutdown before workspace shutdown.
- Storage/runtime shutdown errors remain visible and never convert a failed
  operation into a reported success.

## Delivery slices

| Slice | Bead | Independently verified output | Dependencies |
|---|---|---|---|
| 8.0 Contract | `JULY_WORKSPACE-4ec.1` | This reconciled design and Beads DAG | none |
| 8.1 Room shell | `JULY_WORKSPACE-4ec.2` | Room commands, human/JSON integration tests | 8.0 |
| 8.2 Thread management | `JULY_WORKSPACE-4ec.3` | Non-interactive Thread commands and tests | 8.0 |
| 8.3 Publish shell | `JULY_WORKSPACE-4ec.4` | Explicit Publish command and tests | 8.0 |
| 8.4 REPL navigation | `JULY_WORKSPACE-4ec.5` | Root/Room stack, members/status tests | 8.0 |
| 8.5 DM switching | `JULY_WORKSPACE-4ec.6` | Multi-Agent DM switching/isolation tests | 8.4 |
| 8.6 Thread context | `JULY_WORKSPACE-4ec.7` | Thread chat, contextual publish/isolation tests | 8.2, 8.3, 8.5 |
| 8.7 Closure | `JULY_WORKSPACE-4ec.8` | Docs, review, full gates and roadmap evidence | 8.1-8.6 |

Each green slice is committed separately. Phase 8.1, 8.2, 8.3, and 8.4 are
independent after 8.0 and may be implemented in any order without claiming
another slice's behavior.

## Verification strategy

Each implementation slice starts with the smallest failing binary-level or
service-level regression that proves its contract, then makes that test green.
Existing service/storage tests remain the invariant oracle; CLI tests verify
only parsing, rendering, bootstrap, lifecycle, and boundary wiring.

Focused acceptance includes:

- malformed grammar causes no mutation;
- human and JSON forms represent the same result;
- exact name/ID resolution and typed failures remain intact;
- repeated membership and Publish operations preserve existing idempotency;
- `/back` restores descriptors deterministically;
- switching Agent or Conversation preserves distinct bindings and histories;
- DM-to-Thread and Thread-to-Thread transitions inject no foreign transcript;
- permission, cancellation, recovery, and graceful shutdown behavior regress
  neither in `july dm` nor in REPL contexts.

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

Live-provider smoke remains opt-in and is not required for Phase 8 closure.

The Phase 8.7 documentation pass must replace the illustrative
`/thread payment-42` text in `docs/08-RUNTIME-AND-CLI.md` with an explicitly
canonical Thread ID placeholder so it cannot be mistaken for title lookup.
