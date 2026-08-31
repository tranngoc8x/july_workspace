# TUI Command and Mention Auto-Suggest Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: use
> `superpowers:subagent-driven-development` (recommended) or
> `superpowers:executing-plans`. Repository progress is tracked by the Beads
> dependency chain listed below; this plan intentionally does not duplicate it
> with Markdown checkboxes.

**Goal:** Add scoped slash-command and `@mention` suggestions to the existing
TUI footer, with `Tab` and first `Enter` completing suggestions and the next
`Enter` submitting.

**Architecture:** The CLI projects registry-owned canonical command names into
the typed TUI `Context`. `App` derives slash or mention matches from its current
input and reuses one completion operation; `ui` only renders the derived names
in the existing footer. Command parsing, execution, multiline editing, history,
permission handling, and non-TTY behavior remain unchanged.

**Tech Stack:** Rust 2024, Crossterm, `ratatui`, `ratatui-textarea`, Tokio,
existing unit/integration/PTY test harnesses.

**Spec:**
`docs/superpowers/specs/2026-08-31-tui-command-mention-auto-suggest-design.md`

## Global Constraints

- `src/cli/registry.rs::visible_for_scope` is the only source for advertised
  slash commands; do not duplicate the command catalog in TUI code.
- Suggest canonical names in registry order; exclude aliases, hidden
  `/thread*` commands, and commands unavailable in the active scope.
- Do not add dependencies, schemas, persistence, configuration, popup state,
  selection state, fuzzy ranking, argument completion, or animation.
- Keep `App` presentation-only; it edits input but never resolves or executes
  a command during completion.
- Permission modal keys, `Ctrl-C`, `Up`/`Down`, modified Enter, multiline soft
  wrapping, history, single-flight submission, and non-TTY CLI behavior stay
  unchanged.
- A unique suggestion adds exactly one trailing space and consumes the first
  plain `Enter`; the next plain `Enter` submits the completed input.
- Ambiguous matches with no longer common prefix consume `Tab`/`Enter` without
  editing input or dispatching.
- Preserve the unrelated tracked change in `.beads/interactions.jsonl`.

## Beads Dependency Chain

```text
JULY_WORKSPACE-sjc.1
  -> JULY_WORKSPACE-sjc.2
  -> JULY_WORKSPACE-sjc.3
  -> JULY_WORKSPACE-sjc.4
```

Claim and close each Bead only when its own acceptance criteria and focused
checks pass. Do not close parent `JULY_WORKSPACE-sjc` before `.4` completes.

## File Responsibility Map

- `src/cli/registry.rs`: canonical scope-filtered command metadata and its
  invariant tests.
- `src/cli/mod.rs`: convert `ReplContext::scope()` to TUI command names and
  attach them to initial and navigated TUI contexts.
- `src/tui/mod.rs`: accept the initial typed `Context` instead of constructing
  an unseeded root inside `run_app`.
- `src/tui/app.rs`: own typed context metadata, candidate derivation,
  completion, and key precedence.
- `src/tui/ui.rs`: render the shared one-row command/mention footer.
- `tests/cli_repl.rs`: prove Root/Room/DM/Work projection and retained CLI
  execution behavior.
- `tests/tui_terminal.rs`: update `run_app` callers and preserve PTY behavior.

---

## Task 1: Project Scoped Commands into TUI Context

**Bead:** `JULY_WORKSPACE-sjc.1`

**Files:**

- Modify: `src/cli/registry.rs:364-442`
- Modify: `src/tui/app.rs:44-69, 1238-1326`
- Modify: `src/cli/mod.rs:924-938, 2223-2274`
- Modify: `src/tui/mod.rs:232-245`
- Modify: `tests/cli_repl.rs:879-975`
- Modify: `tests/tui_terminal.rs:48-75`

**Interfaces:**

- Produces: `Context::with_commands(Vec<String>) -> Context`
- Produces: `Context::commands(&self) -> &[String]`
- Produces: private `visible_command_names(CommandScope) -> Vec<String>`
- Produces: `InactiveTuiBridge::initial_context(&self) -> Context`
- Changes: `tui::run_app(initial_context: Context, agents: Vec<String>, ...)
  -> Result<(), ShellError>`
- Preserves: `registry::resolve`, `InactiveTuiBridge::dispatch`, and all
  `AppCommand` variants.

### Step 1.1: Add registry characterization invariants

Add this test beside the existing registry tests:

```rust
#[test]
fn visible_commands_are_canonical_scope_filtered_and_not_hidden() {
    let names = |scope| {
        visible_for_scope(scope)
            .map(|spec| spec.name)
            .collect::<Vec<_>>()
    };

    assert_eq!(
        names(CommandScope::Root),
        ["/dm", "/room", "/back", "/rooms", "/agents", "/status", "/help", "/exit"]
    );
    assert_eq!(
        names(CommandScope::Room),
        ["/dm", "/room", "/back", "/rooms", "/agents", "/members", "/work", "/status", "/help", "/exit"]
    );
    assert_eq!(
        names(CommandScope::Dm),
        ["/dm", "/room", "/back", "/rooms", "/agents", "/status", "/restart", "/help", "/exit"]
    );
    assert_eq!(
        names(CommandScope::Thread),
        ["/dm", "/room", "/back", "/rooms", "/agents", "/members", "/work", "/results", "/status", "/publish", "/restart", "/help", "/exit"]
    );
    for scope in CommandScope::ALL {
        let visible = names(*scope);
        assert!(!visible.contains(&"/quit"));
        assert!(!visible.contains(&"/thread"));
        assert!(!visible.contains(&"/thread new"));
    }
}
```

Run:

```bash
cargo test --lib cli::registry::tests::visible_commands_are_canonical_scope_filtered_and_not_hidden -- --exact
```

Expected result: PASS. This is a characterization check for the existing
registry contract; the feature RED begins at the typed-context boundary.

### Step 1.2: Add RED typed-context replacement tests

Extend the context tests in `src/tui/app.rs` with exact command metadata:

```rust
#[test]
fn context_carries_visible_commands() {
    let context = Context::new(ContextId::new("dm:01"), "dm · Ada")
        .with_commands(vec!["/dm".into(), "/restart".into()]);

    assert_eq!(context.commands(), ["/dm", "/restart"]);
}

#[test]
fn matching_context_result_replaces_label_and_commands_atomically() {
    let mut app = App::new(
        Context::root().with_commands(vec!["/dm".into(), "/status".into()]),
    );
    app.reduce(AppEvent::Key(key(KeyCode::Char('x'))));
    app.reduce(AppEvent::Key(key(KeyCode::Enter)));

    app.reduce(AppEvent::CommandFinished {
        context: ContextId::root(),
        result: CommandResult::Context(
            Context::new(ContextId::new("dm:01"), "dm · Ada")
                .with_commands(vec!["/dm".into(), "/restart".into()]),
        ),
    });

    assert_eq!(app.context().label(), "dm · Ada");
    assert_eq!(app.context().commands(), ["/dm", "/restart"]);
}
```

Extend `stale_command_result_cannot_replace_the_active_context` so the stale
result contains a different command list and assert the active list is still
the original list.

Run:

```bash
cargo test --lib tui::app::tests::context_carries_visible_commands -- --exact
cargo test --lib tui::app::tests::matching_context_result_replaces_label_and_commands_atomically -- --exact
cargo test --lib tui::app::tests::stale_command_result_cannot_replace_the_active_context -- --exact
```

Expected RED result: `Context::with_commands` and `Context::commands` are not
defined.

### Step 1.3: Implement the minimum typed metadata

Keep existing constructors source-compatible:

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Context {
    id: ContextId,
    label: String,
    commands: Vec<String>,
}

impl Context {
    pub fn new(id: ContextId, label: impl Into<String>) -> Self {
        Self {
            id,
            label: label.into(),
            commands: Vec::new(),
        }
    }

    pub fn with_commands(mut self, commands: Vec<String>) -> Self {
        self.commands = commands;
        self
    }

    pub fn commands(&self) -> &[String] {
        &self.commands
    }
}
```

Do not add a scope enum to `Context`; the CLI already owns the authoritative
scope.

### Step 1.4: Project registry names at the CLI boundary

Add one private helper in `src/cli/mod.rs`:

```rust
fn visible_command_names(scope: CommandScope) -> Vec<String> {
    registry::visible_for_scope(scope)
        .map(|spec| spec.name.to_owned())
        .collect()
}

fn attach_visible_commands(context: crate::tui::app::Context, scope: CommandScope)
    -> crate::tui::app::Context
{
    context.with_commands(visible_command_names(scope))
}
```

In both root exits from `project_repl_context`, return:

```rust
Ok(attach_visible_commands(Context::root(), CommandScope::Root))
```

For the non-root return, attach names using the actual descriptor:

```rust
Ok(attach_visible_commands(
    Context::new(id, segments.join(" > ")),
    current.scope(),
))
```

This same projected `Context` is already carried by both
`CommandResult::Context` and `CommandResult::Output`; do not add a second event.

### Step 1.5: Seed initial Root context explicitly

Change the active TUI signature and construction:

```rust
pub async fn run_app(
    initial_context: Context,
    agents: Vec<String>,
    mut dispatch: impl FnMut(app::AppCommand) -> io::Result<()>,
    mut next_application_event: impl FnMut() -> io::Result<Option<app::AppEvent>>,
) -> Result<(), ShellError> {
    let _signals = ExitSignals::install().map_err(ShellError::Operation)?;
    let mut guard = TerminalGuard::enter(io::stdout(), CrosstermRawMode)?;
    let mut app = App::new(initial_context);
    app.reduce(app::AppEvent::Agents(agents));
    // Keep the current terminal, event-loop, dispatch, and restoration bodies
    // unchanged after replacing the App constructor.
}
```

The comment in this excerpt describes preserved source; do not add it to the
implementation. The production edit is the new parameter plus replacing
`App::new(Context::root())` with `App::new(initial_context)`.

In `run_tui_repl`, pass:

```rust
let initial_context = bridge.borrow().initial_context();
let interaction = crate::tui::run_app(
    initial_context,
    agents,
    |command| bridge.borrow().dispatch(command).map_err(io::Error::other),
    || {
        bridge
            .borrow_mut()
            .try_next_event()
            .map_err(io::Error::other)
    },
)
.await;
```

Add the bridge accessor and reuse the same CLI helper used by navigation:

```rust
pub fn initial_context(&self) -> crate::tui::app::Context {
    attach_visible_commands(crate::tui::app::Context::root(), CommandScope::Root)
}
```

Update the two `tests/tui_terminal.rs` calls by inserting `Context::root()` as
the first argument. Do not seed registry data in PTY restoration fixtures;
they test terminal lifecycle, not CLI projection.

### Step 1.6: Add the bridge projection regression

Add `inactive_tui_bridge_projects_visible_commands_for_root_room_dm_and_work`
beside the existing navigation test, then assert these ordered lists after
Root, Room, DM, `/back`, and Work navigation:

```rust
const ROOT: &[&str] =
    &["/dm", "/room", "/back", "/rooms", "/agents", "/status", "/help", "/exit"];
const ROOM: &[&str] =
    &["/dm", "/room", "/back", "/rooms", "/agents", "/members", "/work", "/status", "/help", "/exit"];
const DM: &[&str] =
    &["/dm", "/room", "/back", "/rooms", "/agents", "/status", "/restart", "/help", "/exit"];
const WORK: &[&str] =
    &["/dm", "/room", "/back", "/rooms", "/agents", "/members", "/work", "/results", "/status", "/publish", "/restart", "/help", "/exit"];
```

Use this complete test structure:

```rust
#[tokio::test]
async fn inactive_tui_bridge_projects_visible_commands_for_root_room_dm_and_work() {
    let workspace = TestWorkspace::new();
    let room = workspace.seed_room("vna");
    let agent = workspace.seed_agent("codex");
    workspace.add_member(&room, &agent);
    let thread = workspace.seed_thread(&room, "work", &[&agent]);
    let mut bridge = InactiveTuiBridge::open(&workspace.database).await.unwrap();
    let mut app = App::new(bridge.initial_context());
    let names = |app: &App| {
        app.context()
            .commands()
            .iter()
            .cloned()
            .collect::<Vec<_>>()
    };

    assert_eq!(names(&app), ROOT.iter().map(|name| (*name).to_owned()).collect::<Vec<_>>());
    for (input, expected) in [
        ("/room vna".to_owned(), ROOM),
        ("/dm codex".to_owned(), DM),
        ("/back".to_owned(), ROOM),
        (format!("/thread {thread} --agent codex"), WORK),
    ] {
        let event = bridge.execute(tui_command(&mut app, &input)).await.unwrap();
        app.reduce(event);
        assert_eq!(
            names(&app),
            expected.iter().map(|name| (*name).to_owned()).collect::<Vec<_>>()
        );
    }

    bridge.shutdown().await.unwrap();
}
```

The constants immediately above this test are the exact `ROOT`, `ROOM`, `DM`,
and `WORK` values shown in this step. Do not hardcode those names in
`Context::root()`.

After a failed navigation, compare the full cloned `Context` so label and
command metadata are both proven unchanged. Retain the existing exact provider
command and permission assertions to prove dispatch remains authoritative.

Run:

```bash
cargo test --test cli_repl inactive_tui_bridge_projects_visible_commands_for_root_room_dm_and_work -- --exact --nocapture
cargo test --test cli_repl inactive_tui_bridge_reuses_repl_navigation_exact_chat_and_raw_permission_event -- --exact --nocapture
cargo test --test cli_repl inactive_tui_bridge_exposes_successful_command_output_to_app -- --exact --nocapture
cargo test --test tui_terminal --no-run
```

Expected GREEN result: all commands exit `0`.

### Step 1.7: Format, review, and commit slice 1

Run:

```bash
cargo fmt --all -- --check
git diff --check
git diff -- src/cli/registry.rs src/cli/mod.rs src/tui/mod.rs src/tui/app.rs tests/cli_repl.rs tests/tui_terminal.rs
```

Verify the diff contains no TUI-owned scope inference, parser changes, or new
event variant. Then commit only slice-1 files:

```bash
git add src/cli/registry.rs src/cli/mod.rs src/tui/mod.rs src/tui/app.rs tests/cli_repl.rs tests/tui_terminal.rs
git commit -m "feat(JULY_WORKSPACE-sjc.1): project TUI commands"
```

Close `JULY_WORKSPACE-sjc.1` only after the focused checks and review pass.

---

## Task 2: Implement Shared Slash and Mention Completion

**Bead:** `JULY_WORKSPACE-sjc.2`

**Files:**

- Modify: `src/tui/app.rs:471-540, 615-671, 1330-1415`
- Modify: `tests/cli_repl.rs:279-292`

**Interfaces:**

- Consumes: `Context::commands() -> &[String]` from Task 1.
- Produces: private `command_prefix(&self) -> Option<String>`.
- Produces: private `active_completion(&self) -> Option<(String, Vec<&str>)>`.
- Produces: private `complete(&mut self) -> bool`; `true` means candidates were
  active, even when no text changed.
- Preserves: public `completions(&self) -> Vec<&str>` for the renderer.

### Step 2.1: Add RED command and Enter completion tests

Create a test helper inside `src/tui/app.rs` tests:

```rust
fn app_with_commands(commands: &[&str]) -> App {
    App::new(
        Context::root().with_commands(commands.iter().map(|name| (*name).into()).collect()),
    )
}
```

Add focused tests with these exact behaviors:

```rust
#[test]
fn unique_command_uses_first_enter_to_complete_and_second_to_submit() {
    let mut app = app_with_commands(&["/status", "/start"]);
    for character in "/stat".chars() {
        app.reduce(AppEvent::Key(key(KeyCode::Char(character))));
    }

    assert!(app.reduce(AppEvent::Key(key(KeyCode::Enter))).is_empty());
    assert_eq!(app.input(), "/status ");
    assert!(app.completions().is_empty());
    assert_eq!(
        app.reduce(AppEvent::Key(key(KeyCode::Enter))),
        vec![AppCommand::Execute {
            context: ContextId::root(),
            input: "/status ".into(),
        }]
    );
}

#[test]
fn ambiguous_command_without_more_common_prefix_consumes_enter() {
    let mut app = app_with_commands(&["/status", "/start"]);
    for character in "/sta".chars() {
        app.reduce(AppEvent::Key(key(KeyCode::Char(character))));
    }

    assert!(app.reduce(AppEvent::Key(key(KeyCode::Enter))).is_empty());
    assert_eq!(app.input(), "/sta");
    assert_eq!(app.completions(), ["/status", "/start"]);
    assert!(!app.turn_active());
}
```

Add the remaining focused tests:

```rust
#[test]
fn tab_completes_shared_prefix_and_unique_command() {
    let mut app = app_with_commands(&["/status", "/start"]);
    for character in "/st".chars() {
        app.reduce(AppEvent::Key(key(KeyCode::Char(character))));
    }
    app.reduce(AppEvent::Key(key(KeyCode::Tab)));
    assert_eq!(app.input(), "/sta");

    app.reduce(AppEvent::Key(key(KeyCode::Char('t'))));
    app.reduce(AppEvent::Key(key(KeyCode::Tab)));
    assert_eq!(app.input(), "/status ");
}

#[test]
fn slash_completion_preserves_leading_blanks_and_supports_multiword_names() {
    let mut leading = app_with_commands(&["/status"]);
    for character in "  /stat".chars() {
        leading.reduce(AppEvent::Key(key(KeyCode::Char(character))));
    }
    leading.reduce(AppEvent::Key(key(KeyCode::Tab)));
    assert_eq!(leading.input(), "  /status ");

    let mut multiword = app_with_commands(&["/work new"]);
    for character in "/work n".chars() {
        multiword.reduce(AppEvent::Key(key(KeyCode::Char(character))));
    }
    multiword.reduce(AppEvent::Key(key(KeyCode::Tab)));
    assert_eq!(multiword.input(), "/work new ");
}

#[test]
fn slash_completion_ignores_arguments_and_multiline_input() {
    let mut argument = app_with_commands(&["/status"]);
    for character in "/status argument".chars() {
        argument.reduce(AppEvent::Key(key(KeyCode::Char(character))));
    }
    assert!(argument.completions().is_empty());

    let mut multiline = app_with_commands(&["/status"]);
    for character in "/stat".chars() {
        multiline.reduce(AppEvent::Key(key(KeyCode::Char(character))));
    }
    multiline.reduce(AppEvent::Key(alt_key(KeyCode::Enter)));
    assert!(multiline.completions().is_empty());
}

#[test]
fn exact_command_enter_appends_space_before_submit() {
    let mut app = app_with_commands(&["/status"]);
    for character in "/status".chars() {
        app.reduce(AppEvent::Key(key(KeyCode::Char(character))));
    }
    assert!(app.reduce(AppEvent::Key(key(KeyCode::Enter))).is_empty());
    assert_eq!(app.input(), "/status ");
    assert_eq!(app.reduce(AppEvent::Key(key(KeyCode::Enter))).len(), 1);
}

#[test]
fn enter_completes_agent_mention() {
    let mut app = App::new(Context::root());
    app.reduce(AppEvent::Agents(vec!["cashpoint".into(), "cashflow".into()]));
    for character in "@cashf".chars() {
        app.reduce(AppEvent::Key(key(KeyCode::Char(character))));
    }
    assert!(app.reduce(AppEvent::Key(key(KeyCode::Enter))).is_empty());
    assert_eq!(app.input(), "@cashflow ");
}
```

Run:

```bash
cargo test --lib tui::app::tests::unique_command_uses_first_enter_to_complete_and_second_to_submit -- --exact
cargo test --lib tui::app::tests::ambiguous_command_without_more_common_prefix_consumes_enter -- --exact
cargo test --lib tui::app::tests::tab_completes_shared_prefix_and_unique_command -- --exact
cargo test --lib tui::app::tests::slash_completion_preserves_leading_blanks_and_supports_multiword_names -- --exact
cargo test --lib tui::app::tests::slash_completion_ignores_arguments_and_multiline_input -- --exact
cargo test --lib tui::app::tests::exact_command_enter_appends_space_before_submit -- --exact
cargo test --lib tui::app::tests::enter_completes_agent_mention -- --exact
```

Expected RED result: slash candidates are empty and plain Enter dispatches on
the first press.

### Step 2.2: Derive the active completion without state

Implement command detection without trimming trailing whitespace:

```rust
fn command_prefix(&self) -> Option<String> {
    let input = self.input();
    if input.contains('\n') {
        return None;
    }
    input.trim_start().starts_with('/').then(|| input.trim_start().to_owned())
}

fn active_completion(&self) -> Option<(String, Vec<&str>)> {
    if let Some(prefix) = self.command_prefix() {
        let matches: Vec<_> = self
            .context
            .commands()
            .iter()
            .map(String::as_str)
            .filter(|name| name.starts_with(&prefix))
            .collect();
        if !matches.is_empty() {
            return Some((prefix, matches));
        }
    }

    let prefix = self.mention_prefix()?;
    let matches: Vec<_> = self
        .agents
        .iter()
        .filter(|agent| agent.starts_with(&prefix) && agent.len() > prefix.len())
        .map(String::as_str)
        .collect();
    (!matches.is_empty()).then_some((prefix, matches))
}

pub fn completions(&self) -> Vec<&str> {
    self.active_completion()
        .map(|(_, matches)| matches)
        .unwrap_or_default()
}
```

The mention fallback is intentional: `/dm @ca` has no slash-name match, so the
existing trailing mention completion still works. Do not sort either source.

### Step 2.3: Reuse the byte-safe common-prefix algorithm

Replace `complete_mention` with:

```rust
fn complete(&mut self) -> bool {
    let Some((prefix, matches)) = self.active_completion() else {
        return false;
    };
    let first = matches[0];
    let shared = matches.iter().skip(1).fold(first.len(), |shared, other| {
        let mut end = 0;
        for (index, character) in first[..shared].char_indices() {
            let next = index + character.len_utf8();
            if other.len() < next
                || other.as_bytes()[index..next] != first.as_bytes()[index..next]
            {
                break;
            }
            end = next;
        }
        end
    });
    let suffix = first[prefix.len()..shared].to_owned();
    let single = matches.len() == 1;

    if !suffix.is_empty() {
        self.input.insert_str(suffix);
    }
    if single {
        self.input.insert_str(" ");
    }
    true
}
```

If Rust's borrow checker retains the `matches` borrow across the TextArea
mutation, compute owned `suffix` and `single` inside a smaller block and mutate
after that block. Do not clone the full candidate list into `App` state.

### Step 2.4: Apply the approved key precedence

Replace only the current Tab/plain-Enter arms:

```rust
KeyCode::Tab if key.modifiers == KeyModifiers::NONE => {
    self.complete();
}
KeyCode::Enter if key.modifiers == KeyModifiers::NONE => {
    if !self.complete() {
        return self.submit();
    }
}
```

Leave permission, Ctrl-C, Up/Down, modified Enter, Ctrl-J, Esc, Ctrl-D, and the
generic textarea branch in their existing order.

### Step 2.5: Make the bridge test helper tolerate two-press commands

Update `tests/cli_repl.rs::tui_command` so full canonical commands can complete
before dispatch:

```rust
fn tui_command(app: &mut App, input: &str) -> AppCommand {
    for character in input.chars() {
        app.reduce(AppEvent::Key(KeyEvent::new(
            KeyCode::Char(character),
            KeyModifiers::NONE,
        )));
    }
    for _ in 0..2 {
        if let Some(command) = app
            .reduce(AppEvent::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            )))
            .pop()
        {
            return command;
        }
    }
    panic!("non-blank input did not emit a command after completion");
}
```

This helper is only for complete fixture inputs. Ambiguous behavior remains
covered directly by reducer tests.

### Step 2.6: Run focused regressions and commit slice 2

Run:

```bash
cargo test --lib tui::app::tests::unique_command_uses_first_enter_to_complete_and_second_to_submit -- --exact
cargo test --lib tui::app::tests::ambiguous_command_without_more_common_prefix_consumes_enter -- --exact
cargo test --lib tui::app::tests::tab_completes_an_agent_mention_and_the_footer_lists_the_candidates -- --exact
cargo test --lib tui::app::tests::arrow_keys_ -- --nocapture
cargo test --lib tui::app::tests::permission_ -- --nocapture
cargo test --test cli_repl inactive_tui_bridge_reuses_repl_navigation_exact_chat_and_raw_permission_event -- --exact --nocapture
cargo fmt --all -- --check
git diff --check
```

Review specifically that `complete()` returns `true` for ambiguous no-progress
matches, exact command completion appends one space, and the next Enter still
routes through the unchanged `submit()` function.

Commit:

```bash
git add src/tui/app.rs tests/cli_repl.rs
git commit -m "feat(JULY_WORKSPACE-sjc.2): complete TUI suggestions"
```

Close `JULY_WORKSPACE-sjc.2` only after every focused regression passes.

---

## Task 3: Render the Shared Footer

**Bead:** `JULY_WORKSPACE-sjc.3`

**Files:**

- Modify: `src/tui/ui.rs:57-70, 141-190`

**Interfaces:**

- Consumes: `App::completions() -> Vec<&str>` from Task 2.
- Produces no new production interface or state.

### Step 3.1: Add the RED rendered-buffer regression

Add one test that renders separate app states with `TestBackend::new(80, 12)`
and collects footer row `y = 11`:

```rust
fn row(terminal: &Terminal<TestBackend>, y: u16) -> String {
    (0..terminal.backend().buffer().area.width)
        .map(|x| terminal.backend().buffer().cell((x, y)).unwrap().symbol())
        .collect::<String>()
        .trim_end()
        .to_owned()
}

#[test]
fn footer_renders_default_mentions_commands_and_error_priority() {
    let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();

    let app = App::new(Context::root());
    terminal.draw(|frame| render(frame, &app)).unwrap();
    assert_eq!(row(&terminal, 11), "July workspace · /exit to leave");

    let mut commands = App::new(
        Context::root().with_commands(vec!["/dm".into(), "/status".into()]),
    );
    for character in "/d".chars() {
        commands.reduce(AppEvent::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char(character),
            crossterm::event::KeyModifiers::NONE,
        )));
    }
    terminal.draw(|frame| render(frame, &commands)).unwrap();
    assert_eq!(row(&terminal, 11), "Enter/Tab  /dm");
    assert_eq!(terminal.backend().buffer().cell((0, 11)).unwrap().fg, Color::Cyan);

    let mut mentions = App::new(Context::root());
    mentions.reduce(AppEvent::Agents(vec!["cashpoint".into(), "cashflow".into()]));
    for character in "@cash".chars() {
        mentions.reduce(AppEvent::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char(character),
            crossterm::event::KeyModifiers::NONE,
        )));
    }
    terminal.draw(|frame| render(frame, &mentions)).unwrap();
    assert_eq!(row(&terminal, 11), "Enter/Tab  cashpoint  cashflow");

    let mut error = App::new(
        Context::root().with_commands(vec!["/dm".into(), "/status".into()]),
    );
    error.reduce(AppEvent::Key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('x'),
        crossterm::event::KeyModifiers::NONE,
    )));
    error.reduce(AppEvent::Key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Enter,
        crossterm::event::KeyModifiers::NONE,
    )));
    error.reduce(AppEvent::CommandFinished {
        context: ContextId::root(),
        result: CommandResult::Failed("boom".into()),
    });
    for character in "/d".chars() {
        error.reduce(AppEvent::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char(character),
            crossterm::event::KeyModifiers::NONE,
        )));
    }
    terminal.draw(|frame| render(frame, &error)).unwrap();
    assert_eq!(row(&terminal, 11), "! boom");
    assert_eq!(
        terminal.backend().buffer().cell((0, 11)).unwrap().fg,
        ERROR_COLOR
    );
}
```

Import `CommandResult`, `ContextId`, and `ERROR_COLOR` into the test module for
the final state. The exact `! boom` assertion also proves candidates do not
leak through error priority.

Run:

```bash
cargo test --lib tui::ui::tests::footer_renders_default_mentions_commands_and_error_priority -- --exact
```

Expected RED result: suggestion rows still begin with `Tab`.

### Step 3.2: Update only the footer copy

Keep the current error/suggestion/default match ordering and cyan style. Change
the suggestion text and comment only:

```rust
// While a `/` command or `@` mention is being typed, the footer lists matches.
(None, false) => Line::from(Span::styled(
    format!("Enter/Tab  {}", completions.join("  ")),
    Style::default().fg(Color::Cyan),
)),
```

Do not change layout constraints, footer height, wrapping, or transcript/input
geometry.

### Step 3.3: Run UI and geometry regressions, then commit slice 3

Run:

```bash
cargo test --lib tui::ui::tests::footer_renders_default_mentions_commands_and_error_priority -- --exact
cargo test --lib tui::ui::tests::tiny_terminal_render_is_bounded_and_keeps_the_july_label -- --exact
cargo test --lib tui::ui::tests::input_surface_has_horizontal_and_vertical_padding -- --exact
cargo fmt --all -- --check
git diff --check
```

Commit:

```bash
git add src/tui/ui.rs
git commit -m "feat(JULY_WORKSPACE-sjc.3): show TUI suggestions"
```

Close `JULY_WORKSPACE-sjc.3` only after the focused UI checks pass.

---

## Task 4: Independent Review and Full Verification

**Bead:** `JULY_WORKSPACE-sjc.4`

**Files:**

- Review: all files changed by Tasks 1-3.
- Update only if review finds a requirement violation or regression.

**Interfaces:**

- Consumes the complete approved behavior.
- Produces verification evidence and a clean handoff; no new feature API.

### Step 4.1: Run direct acceptance checks

Run the exact final test names created in Task 2:

```bash
cargo test --lib tui::app::tests::unique_command_uses_first_enter_to_complete_and_second_to_submit -- --exact
cargo test --lib tui::app::tests::ambiguous_command_without_more_common_prefix_consumes_enter -- --exact
cargo test --test cli_repl inactive_tui_bridge_projects_visible_commands_for_root_room_dm_and_work -- --exact --nocapture
cargo test --lib tui::ui::tests::footer_renders_default_mentions_commands_and_error_priority -- --exact
```

Record the exit code for every command in the Bead notes.

### Step 4.2: Request independent code review

Give the reviewer the spec, plan, and `git diff` from the first implementation
commit through Task 3. Require explicit checks for:

- duplicated registry/command grammar;
- first-Enter completion versus second-Enter dispatch;
- ambiguous completion returning active/no-dispatch;
- exact and trailing-space behavior;
- Root/Room/DM/Work metadata and stale-result preservation;
- multiline/history/permission key precedence;
- unrequested popup, state, dependency, or refactor scope.

Resolve every Critical, Important, and Minor finding with a focused regression
before continuing. Re-run the task-specific check affected by each fix.

### Step 4.3: Run all repository gates

Run:

```bash
cargo fmt --all -- --check
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets --all-features -- -D warnings
git diff --check
git status --short
```

Expected result: all commands exit `0`. `git status --short` may still show the
pre-existing `.beads/interactions.jsonl`; do not stage, restore, or rewrite it.

### Step 4.4: Close Beads and hand off conservatively

After review and gates pass:

```bash
bd close JULY_WORKSPACE-sjc.1 JULY_WORKSPACE-sjc.2 JULY_WORKSPACE-sjc.3 JULY_WORKSPACE-sjc.4 --reason="Implemented approved scoped command and mention auto-suggest; focused regressions, independent review, and full Rust gates passed."
bd close JULY_WORKSPACE-sjc --reason="Command and mention auto-suggest implementation and verification complete."
git status --short
```

Do not push or sync remote state without separate authorization from Tony.
