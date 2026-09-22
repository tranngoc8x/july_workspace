use crate::domain::{Agent, AgentId, RoomMember, WorkItem};
use serde_json::Value;

/// The only agent status that may receive routed work.
const ACTIVE_AGENT_STATUS: &str = "active";

/// What an agent is good at, as declared under `Agent.metadata.capabilities`.
///
/// Never inferred from the agent name: routing sees declarations, not guesses.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AgentCapabilities {
    pub skills: Vec<String>,
    pub domains: Vec<String>,
    pub languages: Vec<String>,
    pub tools: Vec<String>,
    pub tags: Vec<String>,
}

impl AgentCapabilities {
    /// Read `metadata.capabilities`. Missing or malformed entries read empty,
    /// so an agent registered before capabilities existed still routes safely.
    pub fn from_metadata(metadata: &Value) -> Self {
        let declared = &metadata["capabilities"];
        Self {
            skills: string_list(&declared["skills"]),
            domains: string_list(&declared["domains"]),
            languages: string_list(&declared["languages"]),
            tools: string_list(&declared["tools"]),
            tags: string_list(&declared["tags"]),
        }
    }

    /// An agent that declared nothing cannot be matched on capability alone.
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
            && self.domains.is_empty()
            && self.languages.is_empty()
            && self.tools.is_empty()
            && self.tags.is_empty()
    }
}

fn string_list(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// One agent still in the running for a routing decision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentCandidate {
    pub agent_id: AgentId,
    /// The `@name` a caller would have typed; also the id a judgment returns.
    pub name: String,
    pub capabilities: AgentCapabilities,
    pub active_work_count: usize,
}

/// Deterministic pre-filter: a judgment never sees an agent code can rule out.
///
/// Keeps agents that are active and are current members of the Room, in the
/// order `agents` was given, so a decision is reproducible.
// ponytail: runtime availability is not stored state, so it stays with the
// caller that owns sessions - add it here once `@auto` needs it.
pub fn resolve_room_candidates(
    agents: &[Agent],
    members: &[RoomMember],
    work: &[WorkItem],
) -> Vec<AgentCandidate> {
    agents
        .iter()
        .filter(|agent| agent.status == ACTIVE_AGENT_STATUS)
        .filter(|agent| {
            members
                .iter()
                .any(|member| member.agent_id == agent.id && member.left_at.is_none())
        })
        .map(|agent| AgentCandidate {
            agent_id: agent.id,
            name: agent.name.clone(),
            capabilities: AgentCapabilities::from_metadata(&agent.metadata),
            active_work_count: active_work_count(agent.id, work),
        })
        .collect()
}

fn active_work_count(agent_id: AgentId, work: &[WorkItem]) -> usize {
    work.iter()
        .filter(|item| item.owner_agent_id == Some(agent_id) && !item.status.is_terminal())
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{RoomId, WorkItemId, WorkScope, WorkStatus};
    use serde_json::json;

    fn agent(name: &str, status: &str, metadata: Value) -> Agent {
        Agent {
            id: AgentId::new(),
            name: name.to_owned(),
            project_root: "/tmp/project".into(),
            transport_type: "acp".into(),
            transport_config: json!({}),
            status: status.to_owned(),
            metadata,
            created_at: "2026-09-22T00:00:00Z".into(),
            updated_at: "2026-09-22T00:00:00Z".into(),
        }
    }

    fn member(room_id: RoomId, agent_id: AgentId, left_at: Option<&str>) -> RoomMember {
        RoomMember {
            room_id,
            agent_id,
            role: None,
            generation: 1,
            joined_at: "2026-09-22T00:00:00Z".into(),
            left_at: left_at.map(str::to_owned),
        }
    }

    fn work(room_id: RoomId, owner: AgentId, status: WorkStatus) -> WorkItem {
        WorkItem {
            id: WorkItemId::new(),
            scope: WorkScope::Room(room_id),
            title: "task".into(),
            goal: None,
            status,
            owner_agent_id: Some(owner),
            is_primary: false,
            created_at: "2026-09-22T00:00:00Z".into(),
            updated_at: "2026-09-22T00:00:00Z".into(),
            completed_at: status
                .is_terminal()
                .then(|| "2026-09-22T00:00:00Z".to_owned()),
        }
    }

    #[test]
    fn capabilities_come_from_declared_metadata() {
        let capabilities = AgentCapabilities::from_metadata(&json!({
            "capabilities": {
                "skills": ["api-development", "debugging"],
                "domains": ["backend"],
                "languages": ["rust"],
                "tools": ["redis"],
                "tags": ["oncall"],
            }
        }));
        assert_eq!(capabilities.skills, ["api-development", "debugging"]);
        assert_eq!(capabilities.domains, ["backend"]);
        assert_eq!(capabilities.languages, ["rust"]);
        assert_eq!(capabilities.tools, ["redis"]);
        assert_eq!(capabilities.tags, ["oncall"]);
        assert!(!capabilities.is_empty());
    }

    #[test]
    fn missing_or_malformed_capabilities_read_empty() {
        assert!(AgentCapabilities::from_metadata(&json!({})).is_empty());
        assert!(AgentCapabilities::from_metadata(&json!(null)).is_empty());
        let partial = AgentCapabilities::from_metadata(&json!({
            "capabilities": { "skills": "not-a-list", "domains": ["infra"] }
        }));
        assert!(partial.skills.is_empty());
        assert_eq!(partial.domains, ["infra"]);
    }

    #[test]
    fn only_active_agents_still_in_the_room_are_candidates() {
        let room_id = RoomId::new();
        let backend = agent("backend", "active", json!({}));
        let retired = agent("retired", "revoked", json!({}));
        let departed = agent("departed", "active", json!({}));
        let stranger = agent("stranger", "active", json!({}));
        let members = vec![
            member(room_id, backend.id, None),
            member(room_id, retired.id, None),
            member(room_id, departed.id, Some("2026-09-22T01:00:00Z")),
        ];
        let agents = vec![backend.clone(), retired, departed, stranger];

        let candidates = resolve_room_candidates(&agents, &members, &[]);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].agent_id, backend.id);
        assert_eq!(candidates[0].name, "backend");
    }

    #[test]
    fn workload_counts_only_unfinished_work_owned_by_the_candidate() {
        let room_id = RoomId::new();
        let backend = agent("backend", "active", json!({}));
        let other = agent("infra", "active", json!({}));
        let members = vec![
            member(room_id, backend.id, None),
            member(room_id, other.id, None),
        ];
        let work = vec![
            work(room_id, backend.id, WorkStatus::Working),
            work(room_id, backend.id, WorkStatus::Blocked),
            work(room_id, backend.id, WorkStatus::Done),
            work(room_id, other.id, WorkStatus::Open),
        ];

        let candidates = resolve_room_candidates(&[backend, other], &members, &work);

        assert_eq!(candidates[0].active_work_count, 2);
        assert_eq!(candidates[1].active_work_count, 1);
    }
}
