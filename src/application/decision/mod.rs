//! Routing decisions: who should pick up work when no agent was named.
//!
//! Everything here is deterministic. Probabilistic judgment arrives later,
//! behind `DecisionEngine`, and only ever sees what this module hands it.

mod candidate;

pub use candidate::*;
