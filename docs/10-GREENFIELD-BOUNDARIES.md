# July Workspace — Greenfield Boundaries

## 1. Decision

July Workspace is a greenfield project.

Previous July implementations are not migration sources and are not compatibility targets.

## 2. Explicitly not required

The new project does not need compatibility with:

- old command names;
- old SQLite/JSON schemas;
- old Beads state;
- old Herdr integration;
- old Zellij/tmux behavior;
- old runtime registries;
- old project registry formats;
- old hook scripts;
- old prompt/delegation policies;
- old session identifiers;
- old worker lifecycle semantics;
- old configuration files.

## 3. Historical code policy

Previous implementations may be inspected for:

- lessons learned;
- failure modes;
- useful test ideas;
- useful domain terminology;
- non-obvious business/project knowledge.

Do not copy a historical component merely because it already exists.

Before reusing code, ask:

```text
Would we design this component the same way in a greenfield Rust project today?
```

If the answer is no, redesign it.

## 4. Knowledge reuse

Project/business knowledge may be selectively re-created from historical material.

Examples worth preserving:

- external-system constraints;
- production caveats;
- non-obvious architectural decisions;
- intentionally unusual behavior;
- business terminology.

Do not blindly copy old project dossiers.

## 5. Implementation reuse

Default policy:

```text
reuse concepts selectively
rewrite implementation cleanly
```

This project should not attempt line-by-line porting from another language or runtime.

## 6. Architectural exclusions

Initial core must not introduce:

- Beads;
- terminal multiplexer ownership;
- headless CLI scraping;
- vector DB;
- Mem0;
- Letta;
- LangGraph;
- Redis/message broker;
- compatibility adapters for old July formats.

## 7. Repository policy

A new repository/codebase should begin with:

```text
Cargo.toml
src/
tests/
docs/
```

Do not import old runtime directories wholesale.

## 8. Data policy

New SQLite schema starts clean.

No migration scripts from historical July state are required.

If useful project knowledge needs transfer, do it explicitly through curated config/Markdown, not database migration.

## 9. Product naming

Canonical product name in documentation:

```text
July Workspace
```

Software versioning starts normally from a new project lifecycle, e.g.:

```text
0.1.0
```

Do not use `v2` as a permanent product identity.

## 10. Guiding rule

> Historical July is a source of lessons, not constraints.
