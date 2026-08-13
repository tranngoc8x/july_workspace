//! Public library boundaries for the July Workspace runtime.
//!
//! This crate starts with one package so boundaries can evolve without
//! introducing internal crate dependencies prematurely.

/// Pure workspace concepts and invariants.
pub mod domain;

/// Deterministic use cases coordinating domain concepts and ports.
pub mod application;

/// Minimal terminal presentation for the current roadmap phase.
pub mod cli;

/// Long-lived process and session lifecycle ownership.
pub mod runtime;

/// Boundary for external agent protocols and adapters.
pub mod transport;

/// Boundary for durable workspace state and persistence implementations.
pub mod storage;
