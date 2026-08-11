# July Workspace — Architecture

## 1. System overview

```text
┌────────────────────────────────────────────┐
│ Presentation                              │
│ CLI / REPL / future TUI / future GUI      │
└──────────────────────┬─────────────────────┘
                       │
                       ▼
┌────────────────────────────────────────────┐
│ July Workspace API                         │
│ DM Room Thread Message Work Result Publish │
└──────────────┬───────────────┬─────────────┘
               │               │
               ▼               ▼
        SQLite Store       Agent Runtime
                               │
                               ▼
                         AgentTransport
                               │
                               ▼
                              ACP
                         ┌─────┴─────┐
                         ▼           ▼
                      Claude       Codex
```

## 2. Hard boundaries

### Workspace domain knows
- agents;
- rooms;
- conversations;
- messages;
- work;
- results;
- dependencies;
- memory;
- logical status.

### Workspace domain does not know
- Claude/Codex-specific APIs;
- ACP wire details;
- terminal panes/tabs;
- Herdr/Zellij state;
- stdout parsing.

### Agent Runtime knows
- agent identity;
- project root;
- session bindings;
- lifecycle state;
- transport instance.

### AgentTransport knows
- how to create/resume/send/cancel/close a remote agent session.

### Presentation knows
- how to show workspace state and submit commands.
- It does not own canonical state.

## 3. LLM vs deterministic runtime

### No LLM needed
- explicit `@cashpoint`;
- DM delivery;
- thread membership;
- session lookup;
- result publish;
- dependency propagation;
- persistence;
- retries;
- status transition;
- permission enforcement.

### Semantic reasoning may be useful
- ambiguous target;
- cross-project synthesis;
- architecture disagreement;
- requirement ambiguity;
- portfolio-level question;
- optional compact summarization.

## 4. Source of truth

| Concern | Owner |
|---|---|
| agent identity | SQLite |
| room membership | SQLite |
| conversations/messages | SQLite |
| work/dependencies/results | SQLite |
| session bindings | SQLite |
| active LLM context | Claude/Codex harness |
| code state | filesystem/Git |
| project/room human knowledge | Markdown |
| optional terminal view | external integration |

## 5. Failure rule

```text
logical workspace state > runtime process state
```

A missing process/session must never delete or invalidate the conversation itself.

## 6. Suggested services

- WorkspaceService
- ConversationService
- MessageService
- WorkService
- ResultService
- DependencyService
- MemoryService
- AgentRegistry
- AgentRuntime
- SessionManager
- RecoveryService
- ACPTransport

These can live in one process initially; they are module boundaries, not microservices.
