//! Measures agent auto-selection against a fixed dataset.
//!
//! Ignored by default: it needs a configured judgment provider and makes real
//! network calls, so it is a tool you run, not a gate that runs itself.
//!
//! ```text
//! TYPESAFE_API_KEY=... cargo test --test agent_routing_eval -- --ignored --nocapture
//! ```
//!
//! The number to watch is the wrong-auto-assignment rate. Automatic routing
//! should not be turned on by default until it is low enough to live with.

use july_workspace::adapter::JevDecisionEngine;
use july_workspace::application::{
    AgentCandidate, AgentCapabilities, AgentSelectionPolicy, AgentSelectionRequest, DecisionEngine,
    RoutingDecision, RoutingMode,
};
use july_workspace::domain::AgentId;
use serde_json::Value;
use std::collections::HashMap;
use std::time::Instant;

fn dataset() -> Value {
    let path =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/eval/agent-routing.json");
    let text = std::fs::read_to_string(&path).expect("read the eval dataset");
    serde_json::from_str(&text).expect("the eval dataset is valid JSON")
}

fn strings(value: &Value) -> Vec<String> {
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

fn candidates(dataset: &Value) -> Vec<AgentCandidate> {
    dataset["agents"]
        .as_array()
        .expect("the dataset lists agents")
        .iter()
        .map(|agent| AgentCandidate {
            agent_id: AgentId::new(),
            name: agent["name"].as_str().expect("an agent name").to_owned(),
            description: agent["description"].as_str().map(str::to_owned),
            capabilities: AgentCapabilities {
                skills: strings(&agent["skills"]),
                domains: strings(&agent["domains"]),
                languages: strings(&agent["languages"]),
                tools: strings(&agent["tools"]),
                tags: Vec::new(),
            },
            active_work_count: 0,
        })
        .collect()
}

#[derive(Default)]
struct Totals {
    cases: usize,
    top1_acceptable: usize,
    top3_recall: usize,
    unresolved: usize,
    wrong_auto: usize,
    failures: usize,
    millis: u128,
}

#[tokio::test]
#[ignore = "needs a configured judgment provider and real network calls"]
async fn agent_routing_meets_its_eval_dataset() {
    let dataset = dataset();
    let candidates = candidates(&dataset);
    let by_id: HashMap<AgentId, String> = candidates
        .iter()
        .map(|candidate| (candidate.agent_id, candidate.name.clone()))
        .collect();
    let mut engine = JevDecisionEngine::from_env();
    assert!(
        engine.is_configured(),
        "set TYPESAFE_API_KEY before running the eval"
    );
    // Measure the policy at its most permissive: what *would* be assigned.
    let policy = AgentSelectionPolicy {
        mode: RoutingMode::Automatic,
        ..AgentSelectionPolicy::default()
    };

    let mut totals = Totals::default();
    for case in dataset["cases"]
        .as_array()
        .expect("the dataset lists cases")
    {
        let task = case["task"].as_str().expect("a task").to_owned();
        let acceptable = strings(&case["acceptable"]);
        totals.cases += 1;

        let started = Instant::now();
        let decision = engine
            .choose_agent(AgentSelectionRequest {
                task: task.clone(),
                candidates: candidates.clone(),
            })
            .await;
        totals.millis += started.elapsed().as_millis();

        let decision = match decision {
            Ok(decision) => decision,
            Err(error) => {
                totals.failures += 1;
                println!("{task}\n  FAILED: {error}");
                continue;
            }
        };
        let name_of = |agent_id: &AgentId| by_id.get(agent_id).cloned().unwrap_or_default();
        let chosen = decision.selected.as_ref().map(&name_of);
        let top3: Vec<String> = decision
            .candidates
            .iter()
            .take(3)
            .map(|scored| name_of(&scored.agent_id))
            .collect();
        let verdict = policy.apply(&decision, &candidates);

        let hit = chosen
            .as_ref()
            .is_some_and(|name| acceptable.contains(name));
        if hit {
            totals.top1_acceptable += 1;
        }
        if top3.iter().any(|name| acceptable.contains(name)) {
            totals.top3_recall += 1;
        }
        match verdict {
            RoutingDecision::AutoSelected { .. } if !hit => totals.wrong_auto += 1,
            RoutingDecision::Unresolved => totals.unresolved += 1,
            _ => {}
        }

        println!(
            "{task}\n  chose {} ({:.2}) {} | verdict {:?} | top3 {}",
            chosen.as_deref().unwrap_or("-"),
            decision.confidence,
            if hit { "ok" } else { "MISS" },
            verdict,
            top3.join(", "),
        );
    }

    let rate = |count: usize| 100.0 * count as f64 / totals.cases.max(1) as f64;
    println!(
        "\ncases {} | top-1 acceptable {:.0}% | top-3 recall {:.0}% | unresolved {:.0}% \
         | wrong-auto {:.0}% | provider failures {:.0}% | {} ms avg",
        totals.cases,
        rate(totals.top1_acceptable),
        rate(totals.top3_recall),
        rate(totals.unresolved),
        rate(totals.wrong_auto),
        rate(totals.failures),
        totals.millis / totals.cases.max(1) as u128,
    );
    assert_eq!(totals.failures, 0, "the provider failed during the eval");
}
