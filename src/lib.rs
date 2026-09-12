//! Public library boundaries for the July Workspace runtime.
//!
//! This crate starts with one package so boundaries can evolve without
//! introducing internal crate dependencies prematurely.

/// Pure workspace concepts and invariants.
pub mod domain;

/// Deterministic use cases coordinating domain concepts and ports.
pub mod application;

/// Danh mục và vòng đời của các ACP adapter cài trên máy.
pub mod adapter;

/// Minimal terminal presentation for the current roadmap phase.
pub mod cli;

/// Full-screen terminal ownership and restoration.
pub mod tui;

/// Long-lived process and session lifecycle ownership.
pub mod runtime;

/// ACP runtime transport and internal A2A Room communication mappings.
pub mod transport;

/// Boundary for durable workspace state and persistence implementations.
pub mod storage;
