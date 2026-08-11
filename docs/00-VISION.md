# July Workspace — Vision

## 1. Problem

Working with multiple coding agents across multiple codebases creates fragmentation:

```text
project A → separate Claude/Codex session
project B → separate Claude/Codex session
project C → another session

cross-project question
→ manually copy context
→ manually coordinate
→ manually remember which session owns what
```

The problem is not that coding agents need a smarter supervisor.

The problem is that users need a durable workspace around strong coding agents.

## 2. Product definition

> July Workspace is a personal multi-project agent workspace.

A persistent workspace where:

- each coding agent owns one codebase;
- users can work directly with one agent via DM;
- multiple agents collaborate inside Room Threads;
- agents can communicate directly;
- July persists messages, work state, results, dependencies and session bindings;
- July performs deterministic routing/state management without unnecessary LLM calls;
- semantic coordination is invoked only when it adds real value.

## 3. Greenfield status

July Workspace is a new implementation.

It has no requirement to preserve compatibility with:

- previous July code;
- previous schemas;
- previous task systems;
- previous terminal/runtime integrations;
- previous configuration formats;
- previous session state;
- previous prompt policies.

Historical July implementations are reference material only.

## 4. Differentiated value

### One workspace, many codebases

```text
/dm cashpoint
/room vna
/thread payment-42
```

No manual terminal/session juggling required.

### Cross-project communication

```text
cashpoint ↔ pay
cashpoint ↔ infra
```

No mandatory July LLM paraphrasing layer.

### Context isolation

```text
cashpoint identity
├── DM cashpoint
├── VNA/payment
├── VNA/refund
└── Grab/callback
```

Each conversation has isolated working context/session state.

### Durable continuity

Remote Claude/Codex sessions may disappear.

July conversations, work state, results and memory survive.

### Dependency coordination

```text
Thread A → Result READY → Thread B unblocked
```

No transcript copying required.

## 5. Non-goals

July Workspace is not:

- a Slack/Buzz clone;
- a general project-management suite;
- a generic workflow engine;
- Git hosting;
- a terminal multiplexer;
- an IDE;
- a memory SaaS;
- a replacement harness for Claude Code/Codex;
- a compatibility layer for previous July implementations.

## 6. Design philosophy

### Prompt-light
Only preserve instructions representing genuine product invariants or security boundaries.

### Runtime-first
Routing, persistence, dependency propagation and session binding are deterministic runtime responsibilities.

### Native-first
Use structured/native agent protocols where practical.

### Small abstractions
Do not build fallback stacks before there is evidence they are needed.

### Explicit context boundaries
A conversation never silently inherits another conversation's history.

### Results over transcripts
Share compact Result/capsule/evidence/reference data.

### SQLite first
No semantic/vector memory until actual scale proves a need.

### No terminal dependency
Terminal tools are optional presentation/integration layers.

### Greenfield freedom
When historical behavior conflicts with the cleaner design, prefer the new design.

## 7. Success criteria

July Workspace is successful when:

1. Single-project DM is close in token cost to using the coding agent directly.
2. Explicit routing requires no July LLM call.
3. One agent can participate in multiple threads without context leakage.
4. Thread A can publish a Result to Thread B without copying history.
5. Agent-to-agent messages can route without semantic mediation.
6. Session loss can recover from durable state.
7. July runs without Beads, Herdr, Zellij or tmux.
8. Core durable state has one source of truth: SQLite.
9. A fresh install does not need knowledge of any older July implementation.
