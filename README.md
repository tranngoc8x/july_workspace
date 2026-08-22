# July Workspace

**A workspace for coding agents that work across multiple projects.**

> **Results cross boundaries. Transcripts don't.**

July Workspace explores a simple idea:

**What if every project had its own persistent coding agent — and those agents could collaborate without sharing their entire conversation histories?**

## The Problem

When working with multiple coding agents across multiple repositories, context quickly becomes fragmented.

You end up:

- switching between agent sessions;
- repeatedly explaining project context;
- manually transferring information between projects;
- losing continuity when sessions restart;
- mixing unrelated project context together.

July Workspace aims to provide a shared workspace around those agents.

## How July Works

Each project owns its own persistent agent.

```text
July Workspace

├── Project A → Agent A
├── Project B → Agent B
└── Project C → Agent C
```

You can talk directly to a project agent, or let agents collaborate when work crosses project boundaries.

But collaboration does **not** mean sharing entire conversation histories.

Instead, agents exchange only explicit information such as:

```text
Results
Context
Decisions
References
Dependencies
```

So:

```text
Agent A transcript  ──X──> Agent B

Agent A result      ─────> Agent B
```

That is the core idea behind July Workspace.

## Core Principles

**Project-owned agents**

Each project has its own agent identity and context.

**Persistent workspace**

Project state should survive individual coding-agent sessions.

**Isolated conversations**

Agents should not automatically inherit unrelated conversation history.

**Structured collaboration**

Agents exchange useful results instead of dumping transcripts into each other's context.

**Agent-agnostic**

July is designed to sit around coding agents such as Codex, Claude Code, and other compatible agents rather than replacing them.

## Current Status

July Workspace is currently under active development. Core roadmap Phases 0-5
are implemented and tested: durable agent/DM state, ACP
session lifecycle, Room/Thread membership, atomic Thread creation with primary
Work, targeted isolated Thread sessions, explicit Agent-to-Agent DM, explicit
Thread mentions with dynamic member join, and durable per-target offline
delivery. Startup reconciles persisted `PENDING` deliveries to `FAILED` before
the storage worker is ready; an explicit failed-only retry after restart reuses
the stored target/body and preserves Thread capsule progress. Delivery remains
at-least-once. The executable remains intentionally limited to `july dm <agent>`;
no Agent-to-Agent messaging CLI exists yet.

Next milestones focus on:

- structured Results and publishing
- dependencies between agents
- session recovery
- developer-friendly CLI workflows

Expect breaking changes while the project is still experimental.

## Why July?

Coding agents are becoming increasingly capable.

The interesting problem is no longer only:

> **How do we make one agent smarter?**

It is also:

> **How do multiple capable agents work together across real projects without creating one giant shared context?**

July Workspace is an experiment in solving that problem.

## Try It

The project is still early, but feedback is very welcome.

If this direction sounds interesting, try the project, open an issue, or ⭐ star the repository to follow its development.

---

**Experimental · Open Source · Active Development**
