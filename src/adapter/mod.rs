//! Boundary cho danh mục và vòng đời của các ACP adapter cài trên máy.

mod catalog;

pub use catalog::{ADAPTERS, AdapterSpec, Installer, Tier, find};
