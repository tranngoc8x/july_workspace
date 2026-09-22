use crate::application::{
    AgentCandidate, AgentSelectionDecision, AgentSelectionRequest, CandidateScore, DecisionError,
};
use serde_json::{Map, Value, json};

/// The one question July asks. Answers land under this key.
const QUESTION: &str = "which";

const INSTRUCTIONS: &str = "Exactly one of these agents is the best fit for `task`. \
Which one? Judge the capabilities each agent declares - skills, domains, languages and tools - \
against what the task actually needs, not the agent's name.";

/// Build the `/v1/systemone` body: a choice question over the candidates.
///
/// Only the task text and declared capabilities travel; no transcript, no Room
/// history, no credentials.
pub(super) fn request_body(request: &AgentSelectionRequest, model: &str) -> Value {
    let agents: Vec<Value> = request
        .candidates
        .iter()
        .map(|candidate| {
            let mut described = capabilities_of(candidate);
            described.insert("id".to_owned(), json!(candidate.name));
            described.insert("active_work".to_owned(), json!(candidate.active_work_count));
            Value::Object(described)
        })
        .collect();
    let criteria: Map<String, Value> = request
        .candidates
        .iter()
        .map(|candidate| {
            (
                candidate.name.clone(),
                Value::Object(capabilities_of(candidate)),
            )
        })
        .collect();

    json!({
        "state": { "task": request.task, "agents": agents },
        "model": model,
        "questions": {
            QUESTION: {
                "type": "choice",
                "instructions": INSTRUCTIONS,
                "criteria": criteria,
            }
        }
    })
}

fn capabilities_of(candidate: &AgentCandidate) -> Map<String, Value> {
    let capabilities = &candidate.capabilities;
    let mut described = Map::new();
    described.insert("skills".to_owned(), json!(capabilities.skills));
    described.insert("domains".to_owned(), json!(capabilities.domains));
    described.insert("languages".to_owned(), json!(capabilities.languages));
    described.insert("tools".to_owned(), json!(capabilities.tools));
    described.insert("tags".to_owned(), json!(capabilities.tags));
    described
}

/// Read the answer back into July's own vocabulary.
///
/// An answer naming an agent that was never a candidate is a provider fault,
/// not a routing result, so it fails loudly instead of assigning someone.
pub(super) fn decision(
    body: &Value,
    candidates: &[AgentCandidate],
) -> Result<AgentSelectionDecision, DecisionError> {
    let answer = body
        .get("answers")
        .and_then(|answers| answers.get(QUESTION))
        .ok_or_else(|| DecisionError::InvalidResponse(format!("no answer '{QUESTION}'")))?;
    let choice = answer
        .get("choice")
        .and_then(Value::as_str)
        .ok_or_else(|| DecisionError::InvalidResponse("answer has no 'choice'".to_owned()))?;
    let confidence = probability(answer.get("confidence"), "confidence")?;
    let selected = candidates
        .iter()
        .find(|candidate| candidate.name == choice)
        .map(|candidate| candidate.agent_id)
        .ok_or_else(|| {
            DecisionError::InvalidResponse(format!("chose '{choice}', which was not a candidate"))
        })?;

    let probabilities = answer.get("probabilities").and_then(Value::as_object);
    let mut scores: Vec<CandidateScore> = candidates
        .iter()
        .map(|candidate| CandidateScore {
            agent_id: candidate.agent_id,
            score: probabilities
                .and_then(|scored| scored.get(&candidate.name))
                .and_then(Value::as_f64)
                .unwrap_or(0.0) as f32,
        })
        .collect();
    scores.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    Ok(AgentSelectionDecision {
        selected: Some(selected),
        confidence,
        candidates: scores,
    })
}

fn probability(value: Option<&Value>, field: &str) -> Result<f32, DecisionError> {
    value
        .and_then(Value::as_f64)
        .filter(|value| (0.0..=1.0).contains(value))
        .map(|value| value as f32)
        .ok_or_else(|| {
            DecisionError::InvalidResponse(format!("'{field}' is not a probability from 0 to 1"))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::AgentCapabilities;
    use crate::domain::AgentId;

    fn candidate(name: &str, tools: &[&str]) -> AgentCandidate {
        AgentCandidate {
            agent_id: AgentId::new(),
            name: name.to_owned(),
            capabilities: AgentCapabilities {
                tools: tools.iter().map(|tool| (*tool).to_owned()).collect(),
                ..AgentCapabilities::default()
            },
            active_work_count: 1,
        }
    }

    #[test]
    fn the_request_carries_only_the_task_and_declared_capabilities() {
        let infra = candidate("infra", &["redis", "docker"]);
        let body = request_body(
            &AgentSelectionRequest {
                task: "fix Redis timeout".into(),
                candidates: vec![infra],
            },
            "jev-latest",
        );

        assert_eq!(body["state"]["task"], json!("fix Redis timeout"));
        assert_eq!(body["state"]["agents"][0]["id"], json!("infra"));
        assert_eq!(
            body["state"]["agents"][0]["tools"],
            json!(["redis", "docker"])
        );
        assert_eq!(body["state"]["agents"][0]["active_work"], json!(1));
        assert_eq!(body["model"], json!("jev-latest"));
        assert_eq!(body["questions"]["which"]["type"], json!("choice"));
        assert_eq!(
            body["questions"]["which"]["criteria"]["infra"]["tools"],
            json!(["redis", "docker"])
        );
    }

    #[test]
    fn an_answer_maps_back_to_agent_ids_ranked_best_first() {
        let backend = candidate("backend", &["postgres"]);
        let infra = candidate("infra", &["redis"]);
        let candidates = [backend.clone(), infra.clone()];
        let body = json!({
            "answers": { "which": {
                "choice": "infra",
                "confidence": 0.91,
                "probabilities": { "infra": 0.82, "backend": 0.18 }
            }}
        });

        let decided = decision(&body, &candidates).unwrap();

        assert_eq!(decided.selected, Some(infra.agent_id));
        assert_eq!(decided.confidence, 0.91);
        assert_eq!(decided.candidates[0].agent_id, infra.agent_id);
        assert_eq!(decided.candidates[1].agent_id, backend.agent_id);
        assert!(decided.candidates[0].score > decided.candidates[1].score);
    }

    #[test]
    fn an_answer_naming_an_agent_that_never_ran_is_refused() {
        let candidates = [candidate("backend", &[])];
        let body = json!({
            "answers": { "which": { "choice": "frontend", "confidence": 0.99 }}
        });

        assert!(matches!(
            decision(&body, &candidates).unwrap_err(),
            DecisionError::InvalidResponse(_)
        ));
    }

    #[test]
    fn a_missing_or_out_of_range_confidence_is_refused() {
        let candidates = [candidate("backend", &[])];
        let without = json!({ "answers": { "which": { "choice": "backend" }}});
        let absurd = json!({ "answers": { "which": { "choice": "backend", "confidence": 1.4 }}});
        let empty = json!({ "answers": {} });

        for body in [without, absurd, empty] {
            assert!(matches!(
                decision(&body, &candidates).unwrap_err(),
                DecisionError::InvalidResponse(_)
            ));
        }
    }
}
