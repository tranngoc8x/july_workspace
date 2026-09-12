# July Workspace — Runtime and CLI

## Principle

Terminal-first, but not terminal-dependent.

Core must run with no:
- Herdr;
- Zellij;
- tmux.

## Initial mode

Phase 3 starts with one normal interactive command:

```bash
july dm <agent>
```

The target name is exact and resolves an Agent already stored in SQLite. The
database path is `JULY_WORKSPACE_DB` when set, otherwise
`$HOME/.july/workspace.db`. The Agent supplies the project root and a strict ACP
configuration; the CLI never writes SQLite directly and does not invoke an LLM
to route an explicit target.

Phase 4 locks and implements the corresponding application command surface so
the presentation layer does not define domain behavior. Phase 8 adds the
top-level REPL and the Room/Thread shell commands below on top of that surface.
Phase 10 replaces only the no-argument, both-streams-TTY presentation with a
full-screen TUI over the same controller and services.

## Core commands

### Adapter onboarding

```bash
july setup [--adapters <ids>]
```

`july setup` chọn và cài ACP adapter vào `~/.july/adapters`, xác minh ACP
handshake, rồi ghi danh tính đã xác minh vào `identities.json`. Màn hình chọn
tương tác chỉ chạy trên Unix; automation hoặc stdin không phải terminal dùng
`--adapters codex,claude` để chọn rõ adapter cần cài. Khi không chỉ định
`--adapters` và stdin không phải terminal, `july setup` tự cài mặc định
`codex` và `claude`.

### Current-project onboarding

```bash
cd /absolute/path/to/project
july init
```

`july init` lấy thư mục hiện tại làm project, gợi ý tên Agent từ tên thư mục
(chữ Latin Unicode được chuyển về không dấu, khoảng trắng thành `_`), rồi cho
chọn một adapter đã được `july setup` cài và xác minh. Enter ở prompt tên dùng
tên gợi ý; tên được nhập thủ công được giữ nguyên. Command này cần terminal
tương tác. Dùng `july agent add ...` cho script hoặc automation.

### Agents

```bash
july agent add <name> --project <path> --adapter <id> [--runtime <runtime>]
july agent add <name> --project <path> --transport <type> --config <file> [--runtime <runtime>]
july agent update <agent> --adapter <id>
july agent update <agent> --config <file>
july agent list
july agent show <agent>
july agent remove <agent>
```

`july agent add` creates a persistent logical Agent identity bound to a
project. It does **not** start an `AgentSession` and does **not** add the agent
to any Room; those are separate operations. `--runtime` records a user-facing
preference (`codex`, `claude`, …) as configuration, not identity, so changing it
never creates a new logical agent. `--adapter` selects the transport
catalog entry and generates the ACP connection details from its verified
identity. `--adapter` cannot be combined with `--transport` or `--config`.
For a custom transport, use `--transport` with `--config <file>`; the JSON file
supplies that transport's connection details. Provider session IDs, process IDs
and terminal identifiers are never part of the Agent model.

`july agent update` replaces an existing agent's ACP `transport_config` in
place, either from a verified adapter or from the custom JSON config escape
hatch. It keeps the same logical agent and its unrelated fields intact.

`july agent remove` retires an identity by marking it inactive. Rooms, Threads
and transcripts are left untouched; it does not free the agent name. Use
`july agent update` to fix a wrong transport configuration.

Nâng cấp một adapter (ví dụ chạy lại `july setup` để cài bản mới hơn) **không**
tự cập nhật các agent đã tạo từ adapter đó trước đây. `transport_config` đã
lưu vẫn giữ `expected_agent_version` cũ; nếu version đó không còn khớp với
adapter thật, `july dm <agent>` sẽ thất bại ngay ở bước handshake ACP. Sau khi
nâng cấp adapter, chạy `july agent update <agent> --adapter <id>` cho từng
agent dùng adapter đó để đồng bộ lại cấu hình.

Runtime creation stays lazy: a session is created or resumed the first time
`/dm <agent>` or `july thread open` needs one.

### DM

```bash
july dm cashpoint
```

The Phase 3 command preserves the submitted line exactly except for the
terminal newline. `/exit` (or its `/quit` alias) or EOF exits. Permission
choices are displayed as a numbered list; invalid, blank, EOF or interrupted input resolves explicitly to
`Cancelled`. Shutdown disconnects the current binding so the next process can
resume it. A `Lost` binding is reported and is not replaced or replayed before
Phase 7.

### Rooms

```bash
july room create <name> [--description <text>]
july room list
july room members <room>
july room member add <room> <agent>
july room member remove <room> <agent>
```

`<room>` resolves only by exact case-sensitive name or canonical `RoomId`.
`<agent>` resolves only by exact case-sensitive name or canonical `AgentId`.
Room commands do not establish implicit current context.

### Threads

```bash
july thread create <title> --room <room> [--goal <text>] [--member <agent>]...
july thread list --room <room>
july thread members <thread-id>
july thread member add <thread-id> <agent>
july thread member remove <thread-id> <agent>
july thread open <thread-id> --agent <agent>
```

`<thread-id>` is a canonical `ConversationId`; titles are not unique
identifiers. `thread open` addresses exactly one active Thread member. It does
not broadcast, auto-join, infer a recipient or import Room/DM history. Opening
requires an active Agent, active Room, open Thread and active membership in
both scopes.

The locked Phase 4 application commands are `CreateRoom`, `ListRooms`,
`ListRoomMembers`, `AddRoomMember`, `RemoveRoomMember`, `CreateThread`,
`ListThreads`, `ListThreadMembers`, `AddThreadMember`, `RemoveThreadMember` and
`OpenThreadForAgent`. Only the local user may invoke membership mutations in
Phase 4.

Create commands return the durable IDs they create. Membership mutations return
the target state (`active` or `left`) and whether durable state changed. Phase 4
uses typed not-found, inactive-parent, membership-required, active-Thread-
membership and conflicting-ID errors. Phase 8 adds human table formatting and
`--json` on top of those results.

`thread open` streams an interactive session, so it never frames `--json`; the
same rule applies to `july dm`.

### Work

```bash
july work show <id>
july work block <id>
july work ready <id>
```

These are administrative and recovery operations. Normal work state changes flow
from the agent through the collaboration protocol and the application service;
the REPL's `/work` is inspection only.

### Publish

```bash
july publish <result-id> --to <conversation-id>
```

The CLI form always names its target. The REPL form may omit it when exactly
one downstream conversation is linked by a work dependency; see `/publish`
below.

### Failed deliveries

```bash
july delivery list [--json]
july delivery retry <message-id> --agent <agent> [--json]
```

`delivery list` is read-only and returns only `FAILED` delivery rows. `delivery
retry` requires both the canonical message ID and an exact agent name or
canonical `AgentId`; it atomically claims only the matching `FAILED` row and
reuses the stored target, conversation kind and exact body. Missing, already
delivered and concurrently claimed rows return `delivery_not_retryable`.

Retry is explicit and remains at-least-once. A crash after transport acceptance
but before the `DELIVERED` write can cause the exact body to be delivered again.
There is no automatic retry/backoff and no exactly-once promise.

### Session

```bash
july session list
july session restart <conversation> --agent cashpoint
```

## Interactive shell

No-argument dispatch is selected before terminal initialization:

| Invocation | Standard streams | Behavior |
|---|---|---|
| `july` | stdin and stdout are TTYs | Phase 10 TUI |
| `july` | either stream is not a TTY | Phase 8 line REPL |
| `july dm ...` | any | existing standalone stream |
| `july thread open ...` | any | existing standalone stream |
| finite command or `--json` | any | existing CLI output |

The TUI keeps application-owned scrollback, a multiline editor, Root/Room/DM/
Thread navigation, progressive Markdown, resize and follow-tail behavior. A
permission modal owns input while open. Ctrl-C clears nonempty idle input,
cancels an active turn once, and a second press exits from a pending or
acknowledged cancellation. Normal, error, panic, Ctrl-C, SIGTERM and SIGHUP
paths restore the terminal.

Mouse support, themes, plugins, syntax highlighting, a daemon, new command
grammar, schema changes, transcript replay and TUI wrappers for standalone DM
or Thread commands remain out of scope. Live-provider smoke is optional; local
PTY and simulated-runtime tests do not prove provider behavior.

The compatibility line REPL prompt remains `> `; entering a context echoes the
resolved descriptor.

```text
$ july

> /dm cashpoint
dm	<agent-id>	cashpoint
> fix callback retry
```

Slash commands are a presentation-layer interface. They are not the
collaboration protocol: agents hand off, propose, challenge and decide through
the collaboration layer, never through user-facing commands.

### Context model

There are four explicit interactive scopes: `Root`, `Room`, `Dm` and `Thread`.
Every command declares the scopes it is valid in, and a command used outside
them is rejected with a clear message rather than silently reinterpreted:

```text
> /work

/work is unavailable in root context (available in: thread)
```

### Command registry

`src/cli/registry.rs` is the single source of truth for interactive commands.
Each entry carries its canonical name, aliases, kind, valid scopes, summary,
usage and examples, and the parser, scope validation, help and tests all resolve
through it. A command cannot be executable, documented or completed without
being registered, and no command may exist in only one of those places.

### Navigation

```text
/dm <agent>        Root | Room | Dm | Thread
/room <room>       Root | Room | Dm | Thread
/thread <thread>   Room | Thread
/back              Root | Room | Dm | Thread
```

`<thread>` is a canonical `ConversationId`, never a title. `--agent <agent>`
binds the turn explicitly; without it, the Thread's single active agent member
is used, and zero or several candidates are reported instead of guessed.
Entering a Thread requires it to belong to the current Room; from a DM, use
`/room <name>` first.

`/back` pops the navigation history and restores the previous context. It is UI
state only: it never leaves a Room, drops a membership, closes a conversation,
terminates a session, merges model context or mutates Work state. At root it
reports a no-op.

### Inspection

```text
/rooms      Root | Room | Dm | Thread
/agents     Root | Room | Dm | Thread
/deliveries Root | Room | Dm | Thread
/status     Root | Room | Dm | Thread
/help       Root | Room | Dm | Thread
/members    Room | Thread
/work       Thread
/results    Thread
```

All of these are read-only. `/agents` lists logical agents and never mutates
them: onboarding is `july agent add`. `/members` lists the active members of the
current Room or Thread. `/work` lists the current Thread's work items and
`/results` the results its work produced; work state is normally changed by the
collaboration runtime, not by a REPL command.

`/deliveries` is the interactive form of `july delivery list`; it inspects the
same workspace-wide FAILED rows without changing the current context.

`/help` renders the commands valid in the current scope, grouped by kind, from
the registry. `/help <command>` renders that command's name, summary, usage,
valid contexts, aliases and examples from the same metadata.

### Control

```text
/thread new <title> [--goal <goal>]   Room | Thread
/publish <result> [--to <target>]     Thread
/restart                              Dm | Thread
/delivery retry <message-id> --agent <agent>  Root | Room | Dm | Thread
/exit  (alias /quit)                  Root | Room | Dm | Thread
```

`/thread new` creates a Thread in the current Room. It is deliberately not
option-heavy; advanced creation stays in `july thread create`.

`/publish` target resolution is deterministic and never inferred from a
transcript or a model: exactly one downstream conversation linked by a work
dependency resolves, no link is an error, and several require an explicit
`--to`.

`/restart` restarts the current conversation's agent session in place. Session
and binding identifiers stay out of the REPL; low-level session operations
remain in the administrative CLI.

`/delivery retry` uses the same failed-only retry operation as the finite CLI
and leaves the REPL navigation stack unchanged.

`/exit` is the canonical REPL exit and `/quit` is an alias. It leaves the REPL
only: it deletes no workspace state, removes no membership and completes no
work.

Any line that is not a registered command is sent to the live Conversation; at
root or in a Room it is rejected as an invalid command.

### Administrative CLI versus REPL

`july room create …`, `july thread create …`, `july agent add …` and the
membership mutations are the administrative and scripting interface. The REPL
exposes only the common interactive operations. The two surfaces do not need
identical command sets; the application layer remains the authoritative domain
API, and a command is not removed merely because the REPL does not expose it.

### Runtime and Room communication

The command layer operates on July concepts, not protocol or provider details.
ACP remains the runtime execution boundary. Under [Room agent communication via A2A](<24-JULY WORKSPACE — ROOM AGENT COMMUNICATION VIA A2A.md>), A2A carries
interactions between July-managed agents in the same Room through July’s bridge.
It is not an alternative runtime adapter. No protocol-specific slash command,
Thread or Work is required for the Room chat flow. External onboarding
is deferred beyond this plan. See docs/11 for the implementation status.

Switching shell context must not merge underlying LLM session histories. Only
the top descriptor is live; a cold descriptor holds no transcript or model
state, and a failed switch leaves the previous descriptor active.

Phase 8 keeps current context in the REPL descriptor stack instead of adding a
`room use` command; non-interactive commands still establish no implicit
context, so this remains presentation behavior and not part of the Phase 4
application contract.

## Thread mention

Mentions and dynamic membership are Phase 5 behavior.

```text
[vna/payment-42] >
@pay check UB123
```

Runtime may show:

```text
pay joined thread
pay working
```

## Runtime architecture

Initial implementation may be a single process:

```text
CLI/REPL
→ workspace services
→ SQLite
→ AgentRuntime
```

Only add `july daemon` if multiple simultaneous clients or background delivery truly require it.

## JSON output

Operational commands should support:

```bash
--json
```

for testing/automation.

## Packaging and release

July ships as one release binary; no Python or other runtime is required.

```bash
july --version           # july <version>
july --version --json    # {"name":"july","version":"<version>"}
```

Build signed-off artifacts for the macOS development targets:

```bash
scripts/release.sh                       # aarch64 + x86_64 apple-darwin
scripts/release.sh aarch64-apple-darwin  # single target
```

The script writes `dist/july-<version>-<target>.tar.gz` plus `dist/SHA256SUMS`,
and smoke-runs `--version` for the host target.

Install and uninstall:

```bash
scripts/install.sh [--prefix DIR]              # default prefix ~/.local
scripts/uninstall.sh [--prefix DIR] [--purge]
```

Uninstall removes only the binary. Workspace data in `~/.july` is kept unless
`--purge` is passed, which additionally requires an interactive confirmation.
`JULY_PREFIX` and `JULY_DATA_DIR` override the defaults.

The first command run against a fresh machine creates `~/.july`, creates
`workspace.db`, and applies all pending SQLite migrations; later runs only
check the schema version. `JULY_WORKSPACE_DB` overrides the database path.
A database newer than the binary is rejected instead of downgraded.

Homebrew packaging stays deferred until the release cadence is stable.

## Optional integrations

Later:
- Herdr session visibility;
- Zellij focus helpers;
- desktop notifications;
- GUI.

These consume the same Workspace API and must not own state.
