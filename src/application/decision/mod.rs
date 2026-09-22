//! Routing decisions: who should pick up work when no agent was named.
//!
//! The deterministic half - candidates and policy - lives here. Probabilistic
//! judgment arrives behind `DecisionEngine`, and only ever sees what this
//! module hands it. July decides; an engine only advises.

mod candidate;
mod engine;
mod policy;

pub use candidate::*;
pub use engine::*;
pub use policy::*;
