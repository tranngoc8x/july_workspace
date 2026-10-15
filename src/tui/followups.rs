use std::collections::VecDeque;

#[derive(Default)]
pub(super) struct Followups {
    items: VecDeque<String>,
    pub held: bool,
}

impl Followups {
    pub fn push(&mut self, text: String) {
        self.items.push_back(text);
    }
    pub fn pop(&mut self) -> Option<String> {
        if self.held {
            None
        } else {
            self.items.pop_front()
        }
    }
    pub fn len(&self) -> usize {
        self.items.len()
    }
}

pub(super) fn prepare(
    text: &str,
    supplement: bool,
    recipients: Option<&str>,
) -> Result<String, &'static str> {
    if text.trim_start().starts_with('/') {
        return Err("Commands cannot be queued; wait until the turn finishes");
    }
    let parsed = crate::cli::mention::parse(text);
    if !supplement && parsed.is_some() {
        return Ok(text.to_owned());
    }
    let explicit = parsed.as_ref().map(|mentions| {
        mentions
            .agents
            .iter()
            .map(|name| format!("@{name}"))
            .collect::<Vec<_>>()
            .join(" ")
    });
    let target = explicit.as_deref().or(recipients).unwrap_or_default();
    let body = parsed.as_ref().map_or(text, |mentions| mentions.prompt);
    let prefix = if target.is_empty() {
        String::new()
    } else {
        format!("{target} ")
    };
    Ok(if supplement {
        format!(
            "{prefix}Supplement to the request active when this was queued (apply after that turn):\n{body}"
        )
    } else {
        format!("{prefix}{body}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_fifo_without_coalescing_or_replaying() {
        let mut queue = Followups::default();
        queue.push("first".into());
        queue.push("second".into());
        assert_eq!(queue.pop().as_deref(), Some("first"));
        assert_eq!(queue.pop().as_deref(), Some("second"));
        assert_eq!(queue.pop(), None);
    }

    #[test]
    fn held_queue_retains_every_item_until_explicit_resume() {
        let mut queue = Followups::default();
        queue.push("saved".into());
        queue.held = true;
        assert_eq!(queue.pop(), None);
        assert_eq!(queue.len(), 1);
        queue.held = false;
        assert_eq!(queue.pop().as_deref(), Some("saved"));
    }

    #[test]
    fn supplement_keeps_explicit_mentions_at_the_front() {
        let text = prepare("@pay @order verify retries", true, Some("@other")).unwrap();
        assert!(text.starts_with("@pay @order "), "{text}");
        assert!(text.contains("Supplement to the request active when this was queued"));
        assert!(text.ends_with("verify retries"));
    }

    #[test]
    fn implicit_followup_freezes_recipients() {
        assert_eq!(
            prepare("another request", false, Some("@order")).unwrap(),
            "@order another request"
        );
        assert!(
            prepare("more detail", true, Some("@order"))
                .unwrap()
                .starts_with("@order Supplement")
        );
    }

    #[test]
    fn new_request_stays_verbatim_with_explicit_recipient() {
        assert_eq!(
            prepare("  @pay request\nwith details", false, Some("@order")).unwrap(),
            "  @pay request\nwith details"
        );
    }

    #[test]
    fn slash_commands_are_never_queued() {
        assert!(prepare(" /exit", true, None).is_err());
        assert!(prepare("/room other", false, None).is_err());
    }
}
