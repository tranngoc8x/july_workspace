//! Storage boundary for durable workspace state.

mod error;
mod records;
mod sqlite;

pub use error::StoreError;
pub use sqlite::SqliteStore;
