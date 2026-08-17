# Integration Tests

This directory is reserved for tests that verify contracts across the July
Workspace modules, especially application use cases, transport ports, and
SQLite storage. Current concrete coverage lives in `tests/core_sqlite.rs`,
`tests/dm_storage.rs`, `tests/direct_message.rs`, `tests/session_runtime.rs`,
`tests/phase4_storage.rs`, `tests/phase4_application.rs`, and
`tests/thread_runtime.rs`; add broader grouped suites here when later-phase
module boundaries justify the extra directory structure.
