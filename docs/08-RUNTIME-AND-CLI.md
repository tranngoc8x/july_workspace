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

The full top-level REPL and other command families below remain Phase 8. No full
TUI is required.

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
july room list
july room use vna
july room members vna
```

### Threads

```bash
july thread list --room vna
july thread create "payment callback" --room vna
july thread open payment-42
```

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

## Thread mention

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
