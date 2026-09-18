# July Workspace — Architecture

> Room collaboration architecture: [Room agent communication via A2A](<24-JULY WORKSPACE — ROOM AGENT COMMUNICATION VIA A2A.md>) supersedes older external-first A2A assumptions.
> July is a shared workspace where project agents collaborate like teammates
> inside Rooms. Rooms own shared human-visible conversation; mentions determine
> attention and routing. A2A carries agent-to-agent interactions; ACP runs each
> agent runtime. July bridges identity, routing, persistence, recovery and Room
> membership without acting as a supervisor brain. Agents may communicate only
> within a shared Room. Shared messages and explicit Results/Artifacts may cross
> agent boundaries; private runtime transcripts do not.
>
> See docs/11 for implementation and verification status.

## 1. System overview

```text
┌────────────────────────────────────────────┐
│ Presentation                              │
│ CLI / REPL / Phase 10 TUI / future GUI    │
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

- explicit `@agent_order`;
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

| Concern                      | Owner                |
| ---------------------------- | -------------------- |
| agent identity               | SQLite               |
| room membership              | SQLite               |
| conversations/messages       | SQLite               |
| work/dependencies/results    | SQLite               |
| session bindings             | SQLite               |
| active LLM context           | Claude/Codex harness |
| code state                   | filesystem/Git       |
| project/room human knowledge | Markdown             |
| optional terminal view       | external integration |

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
