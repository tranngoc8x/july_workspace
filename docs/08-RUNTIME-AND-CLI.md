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

The full top-level REPL and the Room/Thread shell commands below remain Phase 8.
Phase 4 locks and implements the corresponding application command surface so
the presentation layer does not define domain behavior. No full TUI is
required.

## Core commands

### Agents

```bash
july agents
july agent show cashpoint
```

### DM

```bash
july dm cashpoint
```

The Phase 3 command preserves the submitted line exactly except for the
terminal newline. `/quit` or EOF exits. Permission choices are displayed as a
numbered list; invalid, blank, EOF or interrupted input resolves explicitly to
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
membership and conflicting-ID errors. Human table formatting and `--json`
remain Phase 8 concerns.

### Work

```bash
july work show <id>
july work block <id>
july work ready <id>
```

### Publish

```bash
july publish <result-id> --to <conversation-id>
```

### Session

```bash
july session list
july session restart <conversation> --agent cashpoint
```

## REPL UX

```text
$ july

> /dm cashpoint
[cashpoint] > fix callback retry
```

Switch:

```text
/back
/room vna
/thread payment-42
/dm pay
```

Switching shell context must not merge underlying LLM session histories.

`room use`, implicit current Room/Thread state and these slash commands are
Phase 8 presentation behavior, not part of the Phase 4 application contract.

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

## Optional integrations

Later:
- Herdr session visibility;
- Zellij focus helpers;
- desktop notifications;
- GUI.

These consume the same Workspace API and must not own state.
