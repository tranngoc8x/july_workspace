//! Work the bottom pane cannot do itself, handed back to whoever owns it.
//!
//! Codex wires every widget to a global `AppEventSender` that fans events across the whole TUI.
//! July's `App` is a reducer, so the pane collects its side effects in a queue instead and the
//! reducer drains them after each key. The queue is shared by clone, exactly like the sender it
//! replaces, so a view can hold one without borrowing the pane.
//!
//! Single-threaded on purpose: the TUI runs in one task, and `App` is already non-`Send` through
//! the `RefCell` in `run_tui_repl`.

use std::cell::RefCell;
use std::rc::Rc;

use crate::tui::app::ContextId;

/// How prominently a pane notice should read in the transcript.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NoticeLevel {
    /// Guidance, such as naming the command the user meant to type.
    Info,
    /// A rejected action, such as a submission over the input cap.
    Error,
}

/// One effect the pane is asking the surrounding app to carry out.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PaneEvent {
    /// Cancel the running turn.
    Interrupt,
    /// Put a line in the transcript.
    Notice {
        level: NoticeLevel,
        message: String,
    },
    /// Search the workspace for files matching the composer's `@` query.
    StartFileSearch(String),
    /// Fetch one entry from the persistent prompt log.
    ///
    /// ponytail: July never sends this - the composer's persistent history tier stays empty, so
    /// recall and reverse search run entirely over the in-memory entries. Wire it to the SQLite
    /// prompt log to make history survive restarts.
    LookupHistoryEntry {
        context: ContextId,
        log_id: u64,
        offset: usize,
    },
    /// The user chose how to answer a permission request, or dismissed it.
    PermissionResponse {
        request_id: crate::application::ChatPermissionRequestId,
        outcome: crate::domain::PermissionOutcome,
    },
    /// Hand an agent's answered questions back to the turn that asked them.
    UserInputAnswer {
        turn_id: String,
        answers: std::collections::HashMap<String, super::request_user_input::UserInputAnswer>,
    },
    /// Fetch a batch of entries from the persistent prompt log, for reverse search.
    LookupHistoryBatch {
        context: ContextId,
        log_id: u64,
        cursor: super::chat_composer_history::HistoryBatchCursor,
    },
}

/// A handle the pane and its views push [`PaneEvent`]s into.
#[derive(Clone, Default)]
pub(crate) struct PaneEventSender {
    queue: Rc<RefCell<Vec<PaneEvent>>>,
}

impl PaneEventSender {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn send(&self, event: PaneEvent) {
        self.queue.borrow_mut().push(event);
    }

    /// Ask the app to cancel the running turn.
    pub(crate) fn interrupt(&self) {
        self.send(PaneEvent::Interrupt);
    }

    /// Put a line in the transcript.
    pub(crate) fn notice(&self, level: NoticeLevel, message: impl Into<String>) {
        self.send(PaneEvent::Notice {
            level,
            message: message.into(),
        });
    }

    /// Takes everything queued since the last drain.
    pub(crate) fn drain(&self) -> Vec<PaneEvent> {
        std::mem::take(&mut *self.queue.borrow_mut())
    }

    /// Whether anything is waiting to be drained.
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.queue.borrow().is_empty()
    }
}

impl std::fmt::Debug for PaneEventSender {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PaneEventSender")
            .field("queued", &self.queue.borrow().len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::{NoticeLevel, PaneEvent, PaneEventSender};

    #[test]
    fn clones_share_one_queue() {
        let sender = PaneEventSender::new();
        let view = sender.clone();

        view.interrupt();
        sender.notice(NoticeLevel::Error, "too long");

        assert_eq!(
            sender.drain(),
            vec![
                PaneEvent::Interrupt,
                PaneEvent::Notice {
                    level: NoticeLevel::Error,
                    message: "too long".into()
                }
            ]
        );
        assert!(view.is_empty(), "drain empties the shared queue");
    }

    #[test]
    fn draining_twice_yields_nothing_the_second_time() {
        let sender = PaneEventSender::new();
        sender.send(PaneEvent::StartFileSearch("src/".into()));

        assert_eq!(sender.drain().len(), 1);
        assert!(sender.drain().is_empty());
    }
}
