//! TypeSafe (JEV) as a `DecisionEngine`.
//!
//! The wire contract lives here and nowhere else: the application layer only
//! ever sees `DecisionEngine`, `AgentSelectionDecision` and `DecisionError`.

mod client;
mod engine;
mod mapper;

pub use client::JevClient;
pub use engine::JevDecisionEngine;
