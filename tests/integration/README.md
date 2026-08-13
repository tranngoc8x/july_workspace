# Integration Tests

This directory is reserved for tests that verify contracts across the July
Workspace modules, especially application use cases, transport ports, and
SQLite storage. Current concrete coverage lives in `tests/core_sqlite.rs`,
`tests/dm_storage.rs`, `tests/direct_message.rs`, and `tests/session_runtime.rs`;
add broader grouped suites here when later-phase module boundaries justify the
extra directory structure.
