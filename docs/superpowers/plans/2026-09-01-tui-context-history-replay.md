# TUI Context History Replay Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the full-screen TUI transcript with the selected DM or Work conversation's 50 most recent stored messages without leaking contexts or changing non-TTY behavior.

**Architecture:** Forward the existing bounded SQLite query through the storage worker, project domain messages into a presentation-only `ContextSnapshot`, and carry that snapshot through the existing stale-guarded `CommandFinished` result. Apply the new context and transcript atomically in the reducer; keep hydration synchronous, TUI-only, and independent from ACP recovery.

**Tech Stack:** Rust 2024, Tokio, rusqlite, Ratatui, existing `MarkdownStream`, existing ACP test fixture.

**Spec:** `docs/superpowers/specs/2026-09-01-tui-context-history-replay-design.md`

## Global Constraints

- Display exactly the newest 50 messages, ordered chronologically by `(created_at, id)`.
- Use exact truncation text `… showing 50 most recent messages …` with `SYSTEM_COLOR`.
- Domain `Message` values stop at the CLI projection boundary; the TUI reducer receives presentation DTOs only.
- A snapshot replaces only context, transcript, scroll offset, and follow-tail state; it must not reset the editor, prompt history, agents, completion state, permission state, viewport, tick, or exit state.
- Hydration adds no ACP send, recovery-capsule call, session replacement, schema, migration, dependency, cache, pagination, background loader, or configuration.
- Mention entry performs exactly its existing single prompt send; the stripped prompt appears once if storage contains it.
- Command/send errors win over simultaneous history-read errors.
- Standalone `july dm`, line REPL, finite CLI, JSON, and unchanged-context chat retain their current behavior.
- Use Bead `JULY_WORKSPACE-sca.4`; do not start `JULY_WORKSPACE-sca.3` before it closes.

## File Map

- Modify `src/runtime/storage_worker.rs`: expose the existing bounded SQLite query through the worker command channel.
- Modify `src/tui/app.rs`: add presentation DTOs and atomically apply snapshots in the reducer.
- Modify `src/cli/mod.rs`: project stored messages, detect successful context-stack changes, and attach snapshots to TUI command results.
- Modify `tests/cli_repl.rs`: prove DM/Thread isolation, mention ordering, unchanged-context behavior, and non-TTY compatibility.
- No new source module, schema, migration, dependency, or configuration file.

---

### Task 1: Forward bounded recent-message reads through the storage worker

**Files:**
- Modify: `src/runtime/storage_worker.rs:25-190`
- Modify: `src/runtime/storage_worker.rs:264-578`
- Modify: `src/runtime/storage_worker.rs:1374-1376`
- Test: `src/runtime/storage_worker.rs` crate-private test module

**Interfaces:**
- Consumes: `SqliteStore::list_recent_messages_after(ConversationId, Option<&Message>, usize) -> Result<(Vec<Message>, bool), StoreError>`.
- Produces: `StorageHandle::list_recent_messages(ConversationId, usize) -> Result<(Vec<Message>, bool), RuntimeError>` for Task 3.

- [ ] **Step 1: Write the failing worker test**

Append a crate-private Tokio test that creates two conversations, inserts 52 same-timestamp messages in the requested conversation and one sentinel in the other conversation, then calls the missing handle method:

```rust
#[cfg(test)]
mod tests {
use super::*;
use crate::domain::{ConversationKind, MemberType};

#[tokio::test]
async fn bounded_recent_message_request_preserves_order_limit_and_conversation() {
    let directory = std::env::temp_dir().join(format!(
        "july-storage-worker-test-{}",
        ulid::Ulid::generate()
    ));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("workspace.db");
    let requested = conversation(ConversationId::new());
    let unrelated = conversation(ConversationId::new());
    let store = SqliteStore::open(&path).unwrap();
    store.insert_conversation(&requested).unwrap();
    store.insert_conversation(&unrelated).unwrap();
    for number in 1_u128..=52 {
        store.insert_message(&message(number, requested.id)).unwrap();
    }
    store.insert_message(&message(100, unrelated.id)).unwrap();
    drop(store);

    let mut worker = StorageWorker::open(&path).unwrap();
    let (messages, truncated) = worker
        .handle()
        .list_recent_messages(requested.id, 50)
        .await
        .unwrap();

    assert!(truncated);
    assert_eq!(messages.len(), 50);
    assert!(messages
        .iter()
        .all(|message| message.conversation_id == requested.id));
    assert_eq!(messages.first().unwrap().body, "message-03");
    assert_eq!(messages.last().unwrap().body, "message-52");
    worker.shutdown().await.unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}
```

Use these local helpers so equal timestamps prove stable ID ordering:

```rust
fn conversation(id: ConversationId) -> Conversation {
    Conversation {
        id,
        kind: ConversationKind::Dm,
        room_id: None,
        title: None,
        goal: None,
        parent_conversation_id: None,
        origin_conversation_id: None,
        status: "open".into(),
        created_at: "2026-09-01T10:00:00Z".into(),
        updated_at: "2026-09-01T10:00:00Z".into(),
    }
}

fn message(number: u128, conversation_id: ConversationId) -> Message {
    Message {
        id: ulid::Ulid::from(number).into(),
        conversation_id,
        sender_type: MemberType::User,
        sender_id: "tony".into(),
        body: format!("message-{number:02}"),
        reply_to: None,
        metadata: serde_json::Value::Null,
        created_at: "2026-09-01T10:00:00Z".into(),
    }
}
}
```

- [ ] **Step 2: Run the test and verify RED**

Run:

```bash
cargo test --lib runtime::storage_worker::tests::bounded_recent_message_request_preserves_order_limit_and_conversation -- --exact
```

Expected: compile failure `no method named list_recent_messages` on `StorageHandle`.

- [ ] **Step 3: Add the minimum command, handle method, and dispatch arm**

Add beside `ListMessages`:

```rust
ListRecentMessages(ConversationId, usize, Reply<(Vec<Message>, bool)>),
```

Add beside `StorageHandle::list_messages`:

```rust
pub(crate) async fn list_recent_messages(
    &self,
    conversation_id: ConversationId,
    limit: usize,
) -> Result<(Vec<Message>, bool), RuntimeError> {
    self.request(|reply| Command::ListRecentMessages(conversation_id, limit, reply))
        .await
}
```

Add beside the `Command::ListMessages` worker arm:

```rust
Command::ListRecentMessages(conversation_id, limit, reply) => {
    let _ = reply.send(store.list_recent_messages_after(conversation_id, None, limit));
}
```

- [ ] **Step 4: Verify GREEN and recovery independence**

Run:

```bash
cargo test --lib runtime::storage_worker::tests::bounded_recent_message_request_preserves_order_limit_and_conversation -- --exact
cargo test --test recovery_capsule no_checkpoint_emits_newest_twenty_chronologically_with_stable_id_ties -- --exact
```

Expected: both tests pass; recovery still uses its independent limit of 20.

- [ ] **Step 5: Commit the storage slice**

```bash
git add src/runtime/storage_worker.rs
git commit -m "feat(JULY_WORKSPACE-sca.4): expose bounded message history"
```

---

### Task 2: Add typed snapshots and atomic transcript replacement

**Files:**
- Modify: `src/tui/app.rs:94-143`
- Modify: `src/tui/app.rs:793-857`
- Test: `src/tui/app.rs` existing unit-test module

**Interfaces:**
- Consumes: existing `Context`, `ContextId`, `MarkdownStream::{default, push, finish, push_plain}`, `USER_COLOR`, `SYSTEM_COLOR`, and stale `pending == origin` guard.
- Produces: `HistoryAuthor`, `HistoryEntry`, `History`, `ContextSnapshot`, and snapshot-bearing `CommandResult` variants for Task 3.

- [ ] **Step 1: Write reducer tests against the desired DTOs**

Add focused tests with one helper:

```rust
fn snapshot(
    context: Context,
    history: Result<History, String>,
    history_fallback: Option<HistoryEntry>,
) -> ContextSnapshot {
    ContextSnapshot {
        context,
        history,
        history_fallback,
    }
}
```

The first RED test must submit `@ada hello`, then reduce this result:

```rust
CommandResult::SubmittedWithContext(snapshot(
        Context::new(ContextId::new("dm:01"), "dm::ada"),
        Ok(History {
            entries: vec![HistoryEntry {
                author: HistoryAuthor::User,
                body: "hello".into(),
            }],
            truncated: false,
        }),
        Some(HistoryEntry {
            author: HistoryAuthor::User,
            body: "hello".into(),
        }),
    ))
```

Implement that case plus the remaining reducer contract with these complete
tests; `key` and `foreground_of` already exist in the test module:

```rust
fn pending_app(context: Context, input: &str) -> App {
    let mut app = App::new(context);
    for character in input.chars() {
        app.reduce(AppEvent::Key(key(KeyCode::Char(character))));
    }
    app.reduce(AppEvent::Key(key(KeyCode::Enter)));
    app
}

#[test]
fn submitted_snapshot_replaces_routing_echo_once_and_keeps_turn_active() {
    let mut app = pending_app(Context::root(), "@ada hello");
    app.reduce(AppEvent::CommandFinished {
        context: ContextId::root(),
        result: CommandResult::SubmittedWithContext(snapshot(
            Context::new(ContextId::new("dm:01"), "dm::ada"),
            Ok(History {
                entries: vec![HistoryEntry {
                    author: HistoryAuthor::User,
                    body: "hello".into(),
                }],
                truncated: false,
            }),
            Some(HistoryEntry {
                author: HistoryAuthor::User,
                body: "hello".into(),
            }),
        )),
    });
    assert_eq!(app.context().label(), "dm::ada");
    assert_eq!(app.transcript().matches("› hello").count(), 1);
    assert!(!app.transcript().contains("@ada"));
    assert!(app.turn_active());
}

#[test]
fn context_history_replaces_old_text_in_order_with_exact_marker_color() {
    let mut app = pending_app(Context::root(), "/dm ada");
    app.reduce(AppEvent::CommandFinished {
        context: ContextId::root(),
        result: CommandResult::ContextWithHistory(snapshot(
            Context::new(ContextId::new("dm:01"), "dm::ada"),
            Ok(History {
                entries: vec![
                    HistoryEntry {
                        author: HistoryAuthor::User,
                        body: "question".into(),
                    },
                    HistoryEntry {
                        author: HistoryAuthor::Agent,
                        body: "**answer**".into(),
                    },
                ],
                truncated: true,
            }),
            None,
        )),
    });
    let transcript = app.transcript();
    assert_eq!(
        transcript.lines().next(),
        Some("… showing 50 most recent messages …")
    );
    assert!(transcript.find("› question").unwrap() < transcript.find("answer").unwrap());
    assert!(!transcript.contains("/dm ada"));
    assert_eq!(
        foreground_of(
            &app.transcript_text(),
            "… showing 50 most recent messages …"
        ),
        Some(SYSTEM_COLOR)
    );
}

#[test]
fn stale_snapshot_changes_neither_context_nor_transcript() {
    let mut app = pending_app(Context::root(), "/dm ada");
    let before = app.transcript();
    app.reduce(AppEvent::CommandFinished {
        context: ContextId::new("stale"),
        result: CommandResult::ContextWithHistory(snapshot(
            Context::new(ContextId::new("dm:poison"), "poison"),
            Ok(History {
                entries: vec![HistoryEntry {
                    author: HistoryAuthor::Agent,
                    body: "poison".into(),
                }],
                truncated: false,
            }),
            None,
        )),
    });
    assert_eq!(app.context(), &Context::root());
    assert_eq!(app.transcript(), before);
    assert_eq!(app.error(), Some("ignored stale command result for stale"));
}

#[test]
fn history_failure_uses_fallback_and_trims_the_history_error() {
    let mut app = pending_app(Context::root(), "@ada hello");
    app.reduce(AppEvent::CommandFinished {
        context: ContextId::root(),
        result: CommandResult::SubmittedWithContext(snapshot(
            Context::new(ContextId::new("dm:01"), "dm::ada"),
            Err("history unavailable\n".into()),
            Some(HistoryEntry {
                author: HistoryAuthor::User,
                body: "hello".into(),
            }),
        )),
    });
    assert_eq!(app.transcript(), "› hello");
    assert_eq!(app.error(), Some("history unavailable"));
    assert!(app.turn_active());
}

#[test]
fn failed_in_context_prefers_send_error_and_has_no_unproven_fallback() {
    let mut app = pending_app(Context::root(), "@ada hello");
    app.reduce(AppEvent::CommandFinished {
        context: ContextId::root(),
        result: CommandResult::FailedInContext {
            error: "send failed\n".into(),
            snapshot: snapshot(
                Context::new(ContextId::new("dm:01"), "dm::ada"),
                Err("history failed".into()),
                None,
            ),
        },
    });
    assert_eq!(app.context().label(), "dm::ada");
    assert!(app.transcript().is_empty());
    assert_eq!(app.error(), Some("send failed"));
    assert_eq!(app.turn_state(), TurnState::Idle);
}

#[test]
fn empty_root_snapshot_clears_chat_but_preserves_prompt_history() {
    let dm = Context::new(ContextId::new("dm:01"), "dm::ada");
    let mut app = App::new(dm.clone());
    app.reduce(AppEvent::Chat(ChatEvent::TextDelta("old agent text".into())));
    app.reduce(AppEvent::Chat(ChatEvent::TurnCompleted));
    for character in "/back".chars() {
        app.reduce(AppEvent::Key(key(KeyCode::Char(character))));
    }
    app.reduce(AppEvent::Key(key(KeyCode::Enter)));
    app.reduce(AppEvent::CommandFinished {
        context: dm.id().clone(),
        result: CommandResult::ContextWithHistory(snapshot(
            Context::root(),
            Ok(History {
                entries: Vec::new(),
                truncated: false,
            }),
            None,
        )),
    });
    assert!(app.transcript().is_empty());
    app.reduce(AppEvent::Key(key(KeyCode::Up)));
    assert_eq!(app.input(), "/back");
}
```

- [ ] **Step 2: Run the reducer tests and verify RED**

Run:

```bash
cargo test --lib tui::app::tests::submitted_snapshot_replaces_routing_echo_once_and_keeps_turn_active -- --exact
```

Expected: compile failure because `HistoryEntry`, `ContextSnapshot`, and the new result shape do not exist.

- [ ] **Step 3: Add DTOs and minimal reducer helpers**

Add the presentation types exactly:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HistoryAuthor {
    User,
    Agent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryEntry {
    pub author: HistoryAuthor,
    pub body: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct History {
    pub entries: Vec<HistoryEntry>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextSnapshot {
    pub context: Context,
    pub history: Result<History, String>,
    pub history_fallback: Option<HistoryEntry>,
}
```

Keep every existing result variant and add three snapshot-bearing variants, so
this commit compiles without changing any producer or existing behavior:

```rust
SubmittedWithContext(ContextSnapshot),
ContextWithHistory(ContextSnapshot),
FailedInContext {
    error: String,
    snapshot: ContextSnapshot,
},
```

Reuse one user renderer from both submit and hydration:

```rust
fn push_user_entry(&mut self, body: &str) {
    self.markdown.push_plain(
        body.lines()
            .map(|line| format!("› {line}"))
            .collect::<Vec<_>>()
            .join("\n"),
        USER_COLOR,
    );
}
```

Implement `push_history_entry` with `push_user_entry` for User and
`markdown.push(&body); markdown.finish();` for Agent. Implement
`apply_snapshot(ContextSnapshot) -> Option<String>` so it replaces `context`,
sets `markdown = MarkdownStream::default()`, `scroll_offset = 0`, and
`follow_tail = true`, then renders the exact marker, entries, or fallback.

Update `reduce_command_result` after the existing stale guard:

```rust
match result {
    CommandResult::Submitted => {}
    CommandResult::SubmittedWithContext(snapshot) => {
        self.error = self.apply_snapshot(snapshot);
    }
    CommandResult::Context(context) => {
        self.context = context;
        self.turn = TurnState::Idle;
    }
    CommandResult::ContextWithHistory(snapshot) => {
        self.error = self.apply_snapshot(snapshot);
        self.turn = TurnState::Idle;
    }
    CommandResult::Output { context, output } => {
        self.context = context;
        self.freeze_stream();
        self.markdown.push_plain(output, COMMAND_OUTPUT_COLOR);
        self.turn = TurnState::Idle;
    }
    CommandResult::Failed(error) => {
        self.error = Some(error.trim_end().to_owned());
        self.turn = TurnState::Idle;
    }
    CommandResult::FailedInContext { error, snapshot } => {
        let _ = self.apply_snapshot(snapshot);
        self.error = Some(error.trim_end().to_owned());
        self.turn = TurnState::Idle;
    }
}
```

- [ ] **Step 4: Verify GREEN and all reducer regressions**

Run:

```bash
cargo test --lib tui::app::tests
```

Expected: all TUI app tests pass, including the new snapshot, stale, color, fallback, and error-precedence cases.

- [ ] **Step 5: Commit the reducer slice**

```bash
git add src/tui/app.rs
git commit -m "feat(JULY_WORKSPACE-sca.4): replace TUI context history"
```

---

### Task 3: Hydrate only successful TUI context changes in the CLI bridge

**Files:**
- Modify: `src/cli/mod.rs:37-40`
- Modify: `src/cli/mod.rs:1056-1185`
- Modify: `src/cli/mod.rs:1363-1620`
- Modify: `src/cli/mod.rs:2250-2304`
- Test: `tests/cli_repl.rs:920-1120`

**Interfaces:**
- Consumes: `StorageHandle::list_recent_messages` from Task 1 and all presentation DTOs from Task 2.
- Produces: TUI-only `ContextSnapshot` projection and snapshot-bearing bridge results; non-TTY flow remains unchanged.

- [ ] **Step 1: Write failing bridge isolation tests**

Extend the test imports with `MemberType`, `Message`, and `MessageId`, then add:

```rust
fn seed_message(
    &self,
    conversation_id: ConversationId,
    sender_type: MemberType,
    sender_id: &str,
    body: &str,
    created_at: &str,
) {
    SqliteStore::open(&self.database)
        .unwrap()
        .insert_message(&Message {
            id: MessageId::new(),
            conversation_id,
            sender_type,
            sender_id: sender_id.into(),
            body: body.into(),
            reply_to: None,
            metadata: serde_json::Value::Null,
            created_at: created_at.into(),
        })
        .unwrap();
}
```

Add `inactive_tui_bridge_replaces_history_by_exact_conversation`:

1. Seed a DM for `codex` with bodies `dm-user` and `dm-agent`.
2. Seed a Room Thread with bodies `thread-user` and `thread-agent`.
3. Open the bridge and reduce `/dm codex`; assert only DM bodies are present.
4. Reduce `/back`, then `/room vna`, then `/thread <id> --agent codex`; assert only Thread bodies are present.
5. Reduce `/back`; assert the Room transcript is empty.

Use this exact test body:

```rust
#[tokio::test]
async fn inactive_tui_bridge_replaces_history_by_exact_conversation() {
    let workspace = TestWorkspace::new();
    let room = workspace.seed_room("vna");
    let agent = workspace.seed_agent("codex");
    workspace.add_member(&room, &agent);
    let thread = workspace.seed_thread(&room, "work", &[&agent]);
    let dm = SqliteStore::open(&workspace.database)
        .unwrap()
        .get_or_create_dm("local-user", agent.id, NOW)
        .unwrap();
    workspace.seed_message(
        dm.id,
        MemberType::User,
        "local-user",
        "dm-user",
        "2026-09-01T10:00:01Z",
    );
    workspace.seed_message(
        dm.id,
        MemberType::Agent,
        &agent.id.to_string(),
        "dm-agent",
        "2026-09-01T10:00:02Z",
    );
    workspace.seed_message(
        thread,
        MemberType::User,
        "local-user",
        "thread-user",
        "2026-09-01T10:00:03Z",
    );
    workspace.seed_message(
        thread,
        MemberType::Agent,
        &agent.id.to_string(),
        "thread-agent",
        "2026-09-01T10:00:04Z",
    );

    let mut bridge = InactiveTuiBridge::open(&workspace.database).await.unwrap();
    let mut app = App::new(bridge.initial_context());
    let event = bridge
        .execute(tui_command(&mut app, "/dm codex"))
        .await
        .unwrap();
    app.reduce(event);
    assert!(app.transcript().contains("dm-user"));
    assert!(app.transcript().contains("dm-agent"));
    assert!(!app.transcript().contains("thread-user"));

    for input in [
        "/back".to_owned(),
        "/room vna".to_owned(),
        format!("/thread {thread} --agent codex"),
    ] {
        let event = bridge.execute(tui_command(&mut app, &input)).await.unwrap();
        app.reduce(event);
    }
    assert!(app.transcript().contains("thread-user"));
    assert!(app.transcript().contains("thread-agent"));
    assert!(!app.transcript().contains("dm-user"));

    let event = bridge
        .execute(tui_command(&mut app, "/back"))
        .await
        .unwrap();
    app.reduce(event);
    assert!(app.transcript().is_empty());
    bridge.shutdown().await.unwrap();
}
```

Add the successful mention ordering test:

```rust
#[tokio::test]
async fn inactive_tui_bridge_mentions_hydrate_before_live_deltas() {
    let workspace = TestWorkspace::new();
    workspace.seed_acp_agent("codex", &["--no-permission"]);
    let mut bridge = InactiveTuiBridge::open(&workspace.database).await.unwrap();
    let mut app = App::new(bridge.initial_context());

    let submitted = bridge
        .execute(tui_command(&mut app, "@codex stripped prompt"))
        .await
        .unwrap();
    app.reduce(submitted);
    assert!(app.context().label().contains("dm::codex"));
    assert_eq!(app.transcript().matches("› stripped prompt").count(), 1);
    assert!(!app.transcript().contains("@codex"));

    let next = tokio::time::timeout(Duration::from_secs(1), bridge.next_event())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(
        next,
        AppEvent::Chat(ChatEvent::TextDelta(ref text)) if text == "fixture reply"
    ));
    bridge.shutdown().await.unwrap();
}
```

Add a real ACP failure regression. This exercises the production mention
entry, persisted prompt, hydrated context, and subsequent failure ordering;
the runtime represents prompt protocol errors as `TurnFailed`, not a
synchronous `chat.send` error:

```rust
#[tokio::test]
async fn inactive_tui_bridge_mention_installs_history_before_protocol_failure() {
    let workspace = TestWorkspace::new();
    workspace.seed_acp_agent("codex", &["--protocol-error"]);
    let mut bridge = InactiveTuiBridge::open(&workspace.database).await.unwrap();
    let mut app = App::new(bridge.initial_context());

    let submitted = bridge
        .execute(tui_command(&mut app, "@codex persisted once"))
        .await
        .unwrap();
    app.reduce(submitted);
    assert!(app.context().label().contains("dm::codex"));
    assert_eq!(app.transcript().matches("› persisted once").count(), 1);

    let failed = tokio::time::timeout(Duration::from_secs(1), bridge.next_event())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(
        failed,
        AppEvent::Chat(ChatEvent::TurnFailed(
            july_workspace::application::ChatFailureKind::Protocol
        ))
    ));
    app.reduce(failed);
    assert_eq!(app.transcript().matches("› persisted once").count(), 1);
    bridge.shutdown().await.unwrap();
}
```

Add the unchanged-context regression:

```rust
#[tokio::test]
async fn inactive_tui_bridge_unchanged_context_submission_does_not_reload_history() {
    let workspace = TestWorkspace::new();
    workspace.seed_acp_agent("codex", &["--protocol-error"]);
    let mut bridge = InactiveTuiBridge::open(&workspace.database).await.unwrap();
    let mut app = App::new(bridge.initial_context());
    let opened = bridge
        .execute(tui_command(&mut app, "/dm codex"))
        .await
        .unwrap();
    app.reduce(opened);
    app.reduce(AppEvent::Chat(ChatEvent::TextDelta("local sentinel".into())));
    app.reduce(AppEvent::Chat(ChatEvent::TurnCompleted));

    let submitted = bridge
        .execute(tui_command(&mut app, "ordinary prompt"))
        .await
        .unwrap();
    assert!(matches!(
        &submitted,
        AppEvent::CommandFinished {
            result: CommandResult::Submitted,
            ..
        }
    ));
    app.reduce(submitted);
    assert!(app.transcript().contains("local sentinel"));
    bridge.shutdown().await.unwrap();
}
```

The synchronous `FailedInContext` plus simultaneous history-read failure has
no deterministic ACP fixture hook. Its precedence is proved in the reducer
test without adding test-only production configuration; the real protocol
failure path above proves the observable runtime ordering.

- [ ] **Step 2: Run one bridge test and verify RED**

Run:

```bash
cargo test --test cli_repl inactive_tui_bridge_replaces_history_by_exact_conversation -- --exact
```

Expected: test fails because navigation results carry no stored history.

- [ ] **Step 3: Add the TUI-only history projection helper**

Add `const TUI_HISTORY_LIMIT: usize = 50;` beside the existing CLI constants.

Add beside `project_repl_context`:

```rust
async fn project_context_snapshot<R: crate::application::CollaborationRuntime>(
    service: &mut CollaborationService<R>,
    workspace: &WorkspaceRuntime<AcpTransport>,
    contexts: &[ReplContext],
    history_fallback: Option<crate::tui::app::HistoryEntry>,
) -> Result<crate::tui::app::ContextSnapshot, CliError> {
    use crate::tui::app::{ContextSnapshot, History, HistoryAuthor, HistoryEntry};

    let context = project_repl_context(service, contexts).await?;
    let history = match contexts.last().and_then(ReplContext::conversation_id) {
        Some(conversation_id) => workspace
            .storage()
            .list_recent_messages(conversation_id, TUI_HISTORY_LIMIT)
            .await
            .map(|(messages, truncated)| History {
                entries: messages
                    .into_iter()
                    .map(|message| HistoryEntry {
                        author: match message.sender_type {
                            MemberType::User => HistoryAuthor::User,
                            MemberType::Agent => HistoryAuthor::Agent,
                        },
                        body: message.body,
                    })
                    .collect(),
                truncated,
            })
            .map_err(|error| error.to_string()),
        None => Ok(History {
            entries: Vec::new(),
            truncated: false,
        }),
    };
    Ok(ContextSnapshot {
        context,
        history,
        history_fallback,
    })
}
```

Call this helper only when `pending_origin` is `Some`, so line REPL and named commands perform no hydration read.

- [ ] **Step 4: Track actual context-stack changes, not attempted navigation**

Replace the attempt-based `navigated` assignment with `context_changed`, initially `false`. For each request, capture `let context_depth = contexts.len();` before routing. After a registered command or mention route returns, set:

```rust
context_changed = contexts.len() != context_depth;
```

This is sufficient because every successful entry pushes one descriptor and `/back` pops one; failed entry restores the original depth and a repeated mention keeps it unchanged. It avoids `PartialEq`, a routing enum, and snapshots for `/dm missing`.

At the loop top, if `context_changed` is true, project one snapshot. Produce:

- `ContextWithHistory(snapshot)` for successful navigation;
- `FailedInContext { error, snapshot }` when navigation succeeded but a later operation failed;
- existing `Output` when no context changed and stdout exists;
- existing `Failed(error)` when no context changed.

Reset `context_changed = false` immediately after emitting the result.

- [ ] **Step 5: Handle mention send ordering and fallback**

For mention routing, record stack depth before `route_mentions`. If it entered a context, attempt `chat.send(prompt.clone(), timestamp())` first, then build the snapshot.

On successful send and TUI origin, branch before calling the async helper so
same-context mention sends perform no history read:

```rust
use crate::tui::app::{CommandResult, HistoryAuthor, HistoryEntry};

let result = if entered {
    context_changed = false;
    CommandResult::SubmittedWithContext(
        project_context_snapshot(
            service,
            workspace,
            contexts,
            Some(HistoryEntry {
                author: HistoryAuthor::User,
                body: prompt.clone(),
            }),
        )
        .await?,
    )
} else {
    CommandResult::Submitted
};
```

Emit `result`, then call `drain_repl_turn`. For same-context mention and ordinary chat, emit the existing `Submitted` variant.

On send failure, write the send error and leave `pending_origin` plus `context_changed` for the loop top. The loop-top result builds a snapshot with no fallback, applies the new context if entry succeeded, and lets the send error override any history error.

- [ ] **Step 6: Verify bridge GREEN and compatibility**

Run:

```bash
cargo test --test cli_repl inactive_tui_bridge_replaces_history_by_exact_conversation -- --exact
cargo test --test cli_repl inactive_tui_bridge_mentions_hydrate_before_live_deltas -- --exact
cargo test --test cli_repl inactive_tui_bridge_mention_installs_history_before_protocol_failure -- --exact
cargo test --test cli_repl inactive_tui_bridge_unchanged_context_submission_does_not_reload_history -- --exact
cargo test --test cli_repl repl_single_mention_opens_direct_work_and_sends_the_prompt -- --exact
cargo test --test cli_repl repl_switches_agents_without_merging_dm_history_or_bindings -- --exact
cargo test --lib cli::tests::tui_dispatch_requires_both_standard_streams_to_be_terminals -- --exact
cargo test --test cli_dm
```

Expected: all focused bridge and legacy non-TTY suites pass.

- [ ] **Step 7: Commit the bridge slice**

```bash
git add src/cli/mod.rs tests/cli_repl.rs
git commit -m "feat(JULY_WORKSPACE-sca.4): hydrate TUI conversation history"
```

---

### Task 4: Review the complete behavior and run closure gates

**Files:**
- Review: `src/runtime/storage_worker.rs`
- Review: `src/tui/app.rs`
- Review: `src/cli/mod.rs`
- Review: `tests/cli_repl.rs`
- Update after successful gates: Bead `JULY_WORKSPACE-sca.4`

**Interfaces:**
- Consumes: the complete bounded storage -> CLI projection -> stale-guarded reducer flow.
- Produces: closure evidence and unblocks `JULY_WORKSPACE-sca.3` only after every gate passes.

- [ ] **Step 1: Review scope and behavior against the spec**

Run:

```bash
git diff 9a6488f...HEAD -- src/runtime/storage_worker.rs src/tui/app.rs src/cli/mod.rs tests/cli_repl.rs
```

Confirm there is no new schema, dependency, configuration, background task, cache, pagination, ACP send, recovery call, or `Message` field in TUI state. Confirm history reads occur only behind a TUI `pending_origin` and every failed-before-navigation result uses bare `Failed(error)`.

- [ ] **Step 2: Run formatting and focused regression suites**

```bash
cargo fmt --all -- --check
cargo test --lib runtime::storage_worker::tests
cargo test --lib tui::app::tests
cargo test --test cli_repl
cargo test --test cli_dm
```

Expected: every command exits 0 with no ignored new failure.

- [ ] **Step 3: Run full workspace gates**

```bash
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets --all-features -- -D warnings
git diff --check 9a6488f...HEAD
```

Expected: all tests pass, Clippy emits no warnings, and the diff has no whitespace errors.

- [ ] **Step 4: Request independent code review and fix only in-scope findings**

Review specifically:

- conversation isolation and newest-50 ordering;
- stale snapshot rejection;
- mention prompt persistence/rendering exactly once;
- command/send versus history-error precedence;
- no non-TTY or ACP/recovery behavior drift.

If a fix changes behavior, add a failing regression first, observe RED, implement the minimum fix, and rerun the focused plus full gates.

- [ ] **Step 5: Record closure evidence and close the Bead**

```bash
bd update JULY_WORKSPACE-sca.4 --notes="Implemented bounded newest-50 TUI history hydration with atomic context replacement, stale-result isolation, mention ordering, and non-TTY compatibility. Focused and full Rust gates passed; see commits after 9a6488f."
bd close JULY_WORKSPACE-sca.4 --reason="Implemented and verified approved TUI context history replay design"
bd ready
git status --short
```

Expected: `JULY_WORKSPACE-sca.4` is closed, `JULY_WORKSPACE-sca.3` becomes ready, and the worktree is clean.
