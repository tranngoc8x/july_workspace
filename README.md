# July Workspace

July is a local-first terminal workspace for working with persistent coding agents across multiple repositories. Give each project an agent, talk to it directly, or bring several agents into a Room to coordinate a shared task.

July owns identity, conversation routing, durable work and session recovery. Coding-agent runtimes own reasoning and execution. Explicit mentions route directly without a supervisor model; optional `@auto` routing can ask a judgment service to choose an agent.

> **Status:** Under active development. The repository implements DM and Room conversations, structured collaboration, ACP runtime integration, a terminal UI, and recovery workflows. Provider access still requires a configured, authenticated runtime.

[Hướng dẫn tiếng Việt](docs/21-USER-GUIDE-VI.md)

## Install from source

Requirements:

- Rust **1.96.0**, pinned in [rust-toolchain.toml](rust-toolchain.toml), with Cargo and a native build toolchain.
- Node.js/npm for the npm-based ACP adapters, or Cargo for `claude-rust`.
- Authentication and any runtime prerequisites required by your chosen adapter.

SQLite is bundled through `rusqlite`; a separate SQLite installation is not required.

```bash
git clone https://github.com/tranngoc8x/july_workspace.git
cd july_workspace
cargo install --path . --locked
```

Ensure Cargo's binary directory is on `PATH`. Alternatively, `bash scripts/install.sh` builds a standalone installation in `~/.local/bin` with an ownership receipt for self-update; add that directory to `PATH`. Set `JULY_PREFIX` or pass `--prefix <directory>` to change its prefix.

For development, use `cargo run --` in this repository instead of the installed `july` binary.

### Install an ACP adapter

```bash
july setup
```

Or select adapters explicitly:

```bash
july setup --adapters codex,claude
```

The current [adapter catalog](src/adapter/catalog.rs) contains:

| Adapter ID | Package | Installer |
| --- | --- | --- |
| `codex` | `@agentclientprotocol/codex-acp` | npm |
| `claude` | `@agentclientprotocol/claude-agent-acp` | npm |
| `claude-rust` | `claude-code-acp-rs` | Cargo |
| `deepseek` | `@openma/deepseek-harness-acp` | npm; experimental |

Setup detects compatible installations and installs managed adapters when needed. Installing an adapter does not replace provider authentication.

## Quick start

### 1. Register a project

From an existing project directory, run the interactive wizard:

```bash
cd ~/work/order
july init
```

It asks for an agent name and an installed adapter, then starts a short agent turn to suggest an agent description. You can accept or edit the description, or enter one if the agent cannot provide it. Descriptions help optional agent routing.

For explicit registration without starting a model session:

```bash
july agent add order --project "$HOME/work/order" --adapter codex \
  --description "Order service, checkout and refund orchestration"
```

Use a project directory that already exists. Repeat registration for other projects:

```bash
july agent add pay --project "$HOME/work/pay" --adapter claude \
  --description "Payments, provider callbacks and refunds"
```

### 2. Talk to one agent

```bash
july
```

Inside July:

```text
/dm order
Explain the checkout flow and its main entry points.
```

Bare `july` opens the terminal UI when both stdin and stdout are terminals; otherwise it uses the line REPL. The TUI uses the normal terminal screen and scrollback, with a composer at the bottom.

`july dm order` is also available as a standalone conversation stream. Use bare `july` for workspace navigation and the full slash-command interface.

### 3. Bring agents into a Room

Run these administrative commands from your shell:

```bash
july room create commerce
july room member add commerce order
july room member add commerce pay
july
```

Inside July:

```text
/room commerce
@order @pay review the refund flow across both projects
```

A Room must exist before `/room` can enter it. Plain Room conversation does not require creating a Thread or Work item first.

### 4. Continue the conversation

```text
Check duplicate callbacks too.
@pay focus on provider retry behavior
/back
```

- Leading mentions select Room members. Follow-ups without mentions go to the same selected agents.
- A new mention replaces the selection. Each agent retains its own Room session when you switch recipients.
- Entering a Room starts with no selection. Messages without recipients are saved without waking agents.
- `/back` clears the selection first; with no selection, it returns to the previous context.
- `/new @pay` starts a fresh session generation for that agent and selects it. Room history and other agents' sessions remain intact; the runtime session starts on the next prompt.

Use `/help` for commands available in the current context, `/help <command>` for details, and `/exit` (or `/quit`) to leave.

## Working in the terminal UI

The TUI provides streamed Markdown, multiline input, command and mention completion, permission prompts, context history, and per-agent activity indicators. Activity shows whether an agent is working, running a tool, writing, or waiting for permission, together with time since its last update.

While a Room turn is running:

- **Enter** queues a new request.
- **Tab** queues a supplement to the active request; it applies after that turn, rather than changing an in-flight prompt.
- Queued follow-ups keep their recipients and run in order after the active turn finishes.
- Cancellation or failure holds the queue. Use `/queue resume` or `/queue discard` explicitly.

Completion popups and permission prompts handle their keys first. Follow-up queues are in memory for the current TUI process, not a durable background job queue.

On entering a Room, July shows a bounded overview of completed, failed/cancelled, unfinished, and interrupted work. Use:

```text
/continue @pay
```

This sends a new request to reconcile that agent's unfinished responsibilities from durable Room state. It asks the agent to verify uncertain external effects before acting; it does not blindly replay interrupted messages or automatically reopen failed/cancelled Work.

## Optional agent selection

Inside a Room:

```text
/route implement Redis caching for checkout
@auto implement Redis caching for checkout
```

`/route` previews a choice without dispatching the task. `@auto` requests dispatch to one eligible Room member. Explicit `@order` mentions bypass judgment. With one eligible candidate, selection is deterministic; choosing among multiple candidates uses the TypeSafe JEV adapter.

Configure `TYPESAFE_API_KEY` in the process environment or in `~/.july/.env` (`$JULY_HOME/.env` when overridden). `JULY_JEV_BASE_URL` and `JULY_JEV_MODEL` can also be set there; process environment values take precedence. The client uses `curl`.

Routing policy uses process environment variables:

| Variable | Default | Meaning |
| --- | --- | --- |
| `JULY_ROUTING_MODE` | `suggest` | `disabled`, `suggest`, or `automatic` |
| `JULY_ROUTING_AUTO_THRESHOLD` | `0.85` | Minimum judgment confidence for automatic selection |
| `JULY_ROUTING_SUGGEST_THRESHOLD` | `0.65` | Minimum confidence for a suggestion |

`@auto` is an explicit request to choose, so it can dispatch a confident selection even in `suggest` mode. `disabled` blocks automatic routing. Low confidence or a provider error does not dispatch a guessed target. Plain messages retain the Room recipient behavior described above.

An agent literally named `auto` takes precedence over the routing keyword. Update an agent's description with:

```bash
july agent update order --description "Order service and checkout APIs"
```

## Collaboration and isolation

An agent is a persistent logical identity, not a provider session ID. DM, Room-agent and Thread sessions have separate context boundaries. Runtime sessions may resume or be replaced while July retains workspace state.

Room members share explicitly published Room messages. Their private runtime transcripts are not shared wholesale. July's Room bridge carries agent-to-agent interactions through A2A; ACP connects July to the coding-agent runtime:

```text
User / project agent
        │ shared Room message
        ▼
July Room bridge ── A2A ──► target logical agent
                                    │ ACP
                                    ▼
                              coding runtime
```

Structured collaboration adds Work, Results, Artifacts, Decisions and Dependencies. Agents can accept, reject, counter or clarify requests; one agent has no implicit authority over another. Results and explicit artifacts can cross context boundaries without copying private transcripts.

### Work and advanced Threads

Use `/work` in a Room to inspect/open work contexts. Within a work context, July exposes work assignment/status/results and publication commands. Threads remain the underlying isolated conversation contexts; the older `/thread` commands are supported but hidden from the default help.

For an explicit isolated Thread, create it from the shell:

```bash
july thread create "Refund flow" --room commerce --member order --member pay
july thread list --room commerce
```

Then, inside the Room, open the returned Thread ID:

```text
/thread <thread-id> --agent order
```

`/thread new "Refund flow"` only creates a Thread with the local user; it does not enter it or add agents. Use the administrative command above when agent membership is needed.

## Command reference

Commands are context-sensitive. The [command registry](src/cli/registry.rs) drives interactive parsing and help.

| Context | Commands |
| --- | --- |
| All | `/dm <agent> [message]`, `/room <room>`, `/back`, `/agents`, `/rooms`, `/status`, `/help [command]`, `/exit` |
| Room or Work | `/members`, `/work [work]` |
| Room | `/route <task>`, `/new @agent`, `/continue @agent` |
| Room TUI | `/queue resume`, `/queue discard` |
| DM or Work | `/restart` |
| Work | `/results`, `/publish <result> [--to <thread>]` |
| Work | `/work assign <work-id> --agent <agent>`, `/work status <work-id> <status>`, `/work result <work-id> --status <status> --summary <summary>` |
| All | `/deliveries`, `/delivery retry <message-id> --agent <agent>`, `/decisions` |
| All | `/decision accept`, `/decision reject`, `/decision work` — use `/help` for required arguments |

Administrative commands run from the shell:

```bash
july agent list
july agent show order
july room list
july room members commerce
july room member remove commerce pay
july thread list --room commerce
july --json agent list
```

`--json` is for finite commands, not interactive chat. Membership removal changes who can participate; it is not needed to leave a Room in the UI.

## Local state and updates

| Location / variable | Purpose |
| --- | --- |
| `~/.july/workspace.db` | Default SQLite workspace database |
| `JULY_WORKSPACE_DB` | Override the database file path |
| `~/.july/` | Default adapter and runtime-management state root |
| `JULY_HOME` | Override that state root; **does not relocate the database** |
| `$JULY_HOME/.env` or `~/.july/.env` | Optional JEV provider configuration |

SQLite persists agents, membership, messages, Work, Results, Decisions, Dependencies, session bindings and recovery metadata. On restart, an explicit request can resume or replace the runtime session using bounded context. Interrupted prompts are not automatically resent.

`july update` checks the latest stable release, installs an available upgrade for a recognized standalone installation, then migrates the database and reconciles adapters. Reconciliation also runs when July is already current. Managed installations are directed to their package manager for a binary upgrade. Availability depends on installation ownership and release artifacts. For a source installation, rebuild/install from the desired checkout:

```bash
cargo install --path . --locked --force
```

## Codebase

```text
src/
├── domain/          entities, IDs and invariants
├── application/     use cases, collaboration and routing policy
├── storage/         SQLite persistence and migrations
├── runtime/         sessions, Room messaging, recovery and MCP bridge
├── transport/       ACP integration and A2A message mappings
├── adapter/         adapter catalog/installations and JEV client
├── cli/             shell commands, REPL, onboarding and updates
├── tui/             terminal UI, composer, rendering and follow-ups
├── update/          release discovery, installation and handoff
├── lib.rs           public module boundaries
└── main.rs          executable entry point

tests/               integration and boundary tests
docs/                user guide, architecture, plans and evaluations
```

The implementation uses Rust, Tokio, SQLite/rusqlite, the ACP SDK, Ratatui/Crossterm, Markdown rendering, JSON and tracing. CLI parsing is implemented in the repository rather than using Clap.

### Development checks

The [CI workflow](.github/workflows/ci.yml) runs:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
cargo build --release
```

Deterministic tests use mock/test runtimes; passing those checks does not prove live provider authentication or delivery.

## Further reading

- [Vietnamese user guide](docs/21-USER-GUIDE-VI.md)
- [Architecture](docs/01-ARCHITECTURE.md)
- [Domain model](docs/02-DOMAIN-MODEL.md)
- [Room agent communication via A2A](<docs/24-JULY WORKSPACE — ROOM AGENT COMMUNICATION VIA A2A.md>)
- [Implementation roadmap](docs/11-IMPLEMENTATION-ROADMAP.md)

Design and roadmap documents include historical phases and proposed behavior; use the current code and command registry to resolve differences. The collaboration scope is July-managed agents in the same Room; external-agent federation, cross-Room A2A routing and agent-to-agent DM are not exposed as supported workflows.

> **July routes conversations; agents own reasoning; results cross boundaries, transcripts don't.**
