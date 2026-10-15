use crate::domain::{AgentId, WorkStatus};
use crate::storage::RoomOverview;

fn single_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn render_room_overview(overview: &RoomOverview) -> String {
    let mut lines = vec!["Room overview".to_owned()];
    for (label, group) in [("Done", 0), ("Failed / cancelled", 1), ("Unfinished", 2)] {
        let entries: Vec<_> = overview
            .work
            .iter()
            .filter(|entry| {
                let status = entry.work.status;
                let actual = if status == WorkStatus::Done {
                    0
                } else if status.is_terminal() {
                    1
                } else {
                    2
                };
                actual == group
            })
            .collect();
        lines.push(format!("{label}: {}", entries.len()));
        for entry in entries {
            let owner = entry.owner_name.as_deref().unwrap_or("unassigned");
            let result = entry
                .latest_result_summary
                .as_ref()
                .map(|summary| format!("; result: {}", single_line(summary)))
                .unwrap_or_default();
            lines.push(format!(
                "- {} [{}] @{} ({}){}",
                single_line(&entry.work.title),
                entry.work.status,
                single_line(owner),
                entry.work.id,
                result
            ));
        }
    }
    for turn in &overview.interrupted {
        lines.push(format!(
            "- Interrupted @{}: {} (message {}); external effects uncertain",
            single_line(&turn.agent_name),
            turn.status,
            turn.message_id
        ));
    }
    if overview.work_truncated || overview.interrupted_truncated {
        lines.push("More durable history exists; overview truncated.".into());
    }
    let pending = overview
        .work
        .iter()
        .any(|entry| !entry.work.status.is_terminal())
        || !overview.interrupted.is_empty();
    lines.push(if pending {
        "Next step: /continue @agent to reconcile unfinished work and verify uncertain effects."
            .into()
    } else {
        "Next step: Nothing to continue.".into()
    });
    lines.join("\n")
}

pub(super) fn continuation_request(overview: &RoomOverview, agent: AgentId) -> Option<String> {
    let pending =
        overview.work.iter().any(|entry| {
            entry.work.owner_agent_id == Some(agent) && !entry.work.status.is_terminal()
        }) || overview
            .interrupted
            .iter()
            .any(|turn| turn.agent_id == agent);
    if !pending {
        return None;
    }
    let mut lines = vec![
        "Continue from the durable Room state below. First reconcile existing work and results; preserve completed work and continue only unfinished responsibilities assigned to you.".to_owned(),
        "Do not replay earlier messages or blindly repeat actions. Interrupted turns may have produced uncertain external effects: verify their current state before taking another action. Report what is verified, what remains uncertain, and the next step. Do not reopen failed or cancelled Work automatically.".to_owned(),
    ];
    for entry in &overview.work {
        if entry.work.owner_agent_id == Some(agent) {
            lines.push(format!(
                "Work {} [{}]: {}",
                entry.work.id,
                entry.work.status,
                single_line(&entry.work.title)
            ));
            if let Some(summary) = &entry.latest_result_summary {
                lines.push(format!("Latest recorded result: {}", single_line(summary)));
            }
        }
    }
    // One explicit reply_to is the durable acknowledgement for this interruption.
    if let Some(turn) = overview
        .interrupted
        .iter()
        .find(|turn| turn.agent_id == agent)
    {
        lines.push(format!(
            "Interrupted message {} [{}]; external effects unverified. Reconcile this interruption only; other interrupted turns remain pending.",
            turn.message_id, turn.status
        ));
    }
    if overview.work_truncated || overview.interrupted_truncated {
        lines.push("This overview is bounded; inspect durable work and results for additional context before acting.".into());
    }
    Some(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::RoomMessageId;
    use crate::storage::RoomInterruptedTurn;

    #[test]
    fn continuation_is_new_scoped_and_warns_about_uncertain_effects() {
        let agent = AgentId::new();
        let overview = RoomOverview {
            work: vec![],
            work_truncated: false,
            interrupted: vec![RoomInterruptedTurn {
                message_id: RoomMessageId::new(),
                agent_id: agent,
                agent_name: "Ada".into(),
                status: "failed".into(),
            }],
            interrupted_truncated: false,
        };
        let request = continuation_request(&overview, agent).unwrap();
        assert!(request.contains("reconcile"));
        assert!(request.contains("Do not replay"));
        assert!(request.contains("external effects"));
        assert!(continuation_request(&overview, AgentId::new()).is_none());
        assert!(render_room_overview(&overview).contains("Room overview"));
    }

    #[test]
    fn completed_and_failed_work_are_reported_without_reopening_them() {
        use crate::domain::{RoomId, WorkItem, WorkItemId, WorkScope};
        use crate::storage::RoomOverviewWork;
        let agent = AgentId::new();
        let work = WorkItem {
            id: WorkItemId::new(),
            scope: WorkScope::Room(RoomId::new()),
            title: "Shipped".into(),
            goal: None,
            status: WorkStatus::Done,
            owner_agent_id: Some(agent),
            is_primary: false,
            created_at: "now".into(),
            updated_at: "now".into(),
            completed_at: Some("now".into()),
        };
        let overview = RoomOverview {
            work: vec![
                RoomOverviewWork {
                    work: work.clone(),
                    owner_name: Some("Ada".into()),
                    latest_result_summary: Some("Tests passed".into()),
                },
                RoomOverviewWork {
                    work: WorkItem {
                        id: WorkItemId::new(),
                        title: "Failed task".into(),
                        status: WorkStatus::Failed,
                        ..work
                    },
                    owner_name: Some("Ada".into()),
                    latest_result_summary: None,
                },
            ],
            work_truncated: false,
            interrupted: vec![],
            interrupted_truncated: false,
        };
        let rendered = render_room_overview(&overview);
        assert!(rendered.contains("Done: 1"));
        assert!(rendered.contains("Failed / cancelled: 1"));
        assert!(rendered.contains("Tests passed"));
        assert!(continuation_request(&overview, agent).is_none());
    }

    #[test]
    fn empty_overview_does_not_create_continuation() {
        let overview = RoomOverview {
            work: vec![],
            work_truncated: false,
            interrupted: vec![],
            interrupted_truncated: false,
        };
        assert!(continuation_request(&overview, AgentId::new()).is_none());
        assert!(render_room_overview(&overview).contains("Nothing to continue"));
    }

    #[test]
    fn continuation_covers_only_the_interruption_linked_by_the_new_message() {
        let agent = AgentId::new();
        let newest = RoomMessageId::new();
        let older = RoomMessageId::new();
        let overview = RoomOverview {
            work: vec![],
            work_truncated: false,
            interrupted_truncated: false,
            interrupted: [newest, older]
                .into_iter()
                .map(|message_id| RoomInterruptedTurn {
                    message_id,
                    agent_id: agent,
                    agent_name: "pay".into(),
                    status: "failed".into(),
                })
                .collect(),
        };
        let prompt = continuation_request(&overview, agent).unwrap();
        assert!(prompt.contains(&newest.to_string()));
        assert!(
            !prompt.contains(&older.to_string()),
            "unlinked interruption must remain pending: {prompt}"
        );
    }
}
