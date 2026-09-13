# Plan 25 — Part 1: runtime architecture audit

Source: [plan 25](25-JULY-RUNTIME-VERSIONING-SYSTEM-UPDATE-PLAN.md), Workstream A.
Audit baseline: `9880a20da776dc2e2d4394c8d5d0e08dce1be093`.
Tracking: `JULY_WORKSPACE-qqg`, audit `JULY_WORKSPACE-qqg.1`.
This part documents repository behavior only; it does not implement version reconciliation or self-update.

## Seven required answers

| Question | Current behavior and evidence |
| --- | --- |
| Where is AdapterSpec? | `src/adapter/catalog.rs:23` declares `AdapterSpec`; `ADAPTERS` starts at line 36. |
| Static or configured? | Compile-time constant catalog, exported through `src/adapter/mod.rs:6`. No remote manifest is needed. Pins are Codex 1.10.0, Claude 0.70.0, Claude Rust 0.1.22, DeepSeek 0.4.26. The plan's old Codex 1.6.2 example is not the current pin. |
| What does the installer consume? | `src/adapter/store.rs:54` builds npm `install --prefix <root> package@version` or cargo `install package --version version --root <root>`. `SystemInstaller::install` at line 88 executes separate process arguments. |
| What does setup consume? | `src/cli/setup.rs:68` selects catalog entries, then lines 97–124 unconditionally install each selection, probe ACP identity and save it. Exact equality at line 198 affects display only; it does not skip an install. |
| What executable launches? | `AdapterStore::config_for`, `src/adapter/store.rs:241`, copies the recorded identity's binary path into agent configuration. `src/runtime/direct_message.rs:386` parses that stored configuration; `src/transport/acp.rs:137` passes its executable directly to the SDK. Runtime requires an absolute existing file at lines 427–436. It does not resolve adapter IDs through PATH on each launch. |
| Managed directory or PATH? | `src/adapter/store.rs:118` uses JULY_HOME or HOME/.july. Managed npm binaries live under adapters/node_modules/.bin; cargo binaries under adapters/bin (line 135). Installer programs npm/cargo use process PATH lookup; this is distinct from adapter resolution. Explicit user config can supply a different executable. |
| How is version detected? | `installed_version`, `src/adapter/store.rs:146`, reads npm package.json or cargo .crates2.json. Setup display uses that metadata. `probe_agent_identity`, `src/transport/acp.rs:990`, separately obtains ACP initialize identity with a 30-second timeout. No adapter --version SemVer detection exists in these paths. |

## End-to-end behavior and integration risks

Current flow: catalog → setup selection → package install → managed binary path → ACP initialize → adapters/identities.json → agent add/init configuration → persisted agent transport_config → runtime parser → SDK process launch → exact ACP identity verification.

`src/cli/mod.rs:3749` and `:3826` generate transport configuration from the adapter store. The corresponding `--config` paths accept user-provided JSON. Both are persisted in the same transport_config field; there is no ownership marker in `config_for`. Do not infer ownership from JSON shape alone or overwrite custom executable, arguments, environment, project root, state directory or model preferences.

`src/transport/acp.rs:490` compares both handshake name and version exactly against persisted expectations. Updating identities.json alone does not refresh existing agents' persisted expectations. Reconciliation must include an explicit, narrowly scoped solution for generated identity expectations before claiming an upgraded existing agent still works. Preserve identity/protocol validation and custom configuration; package SemVer and ACP-reported identity are distinct contracts.

Detection and launch must use the same selected executable. Preserve explicit configured paths. For onboarding, define and test managed-path/PATH candidate precedence before adding reuse. A compatible PATH executable is not useful if subsequent setup still records or launches a different managed binary. Probe failure and unparseable output must remain distinct from missing installation. Bound process time and output; do not let a UI redraw spawn repeated unbounded probes.

For initial version requirements, keep current install pins. Codex's documented range is >=1.10.0, <2.0.0. The 0.x adapters need explicit conservative compatibility bounds and tests in Part 2; do not infer all 0.x versions are compatible. Validate every install_version against its requirement. Future-major and unknown-version results must not silently trigger a downgrade.

## July update integration audit

| Boundary | Current repository evidence | Consequence |
| --- | --- | --- |
| July version | `Cargo.toml:3`; `src/cli/mod.rs:4600` uses CARGO_PKG_VERSION | Reuse canonical package version. |
| Install methods | `README.md:65` documents cargo install; `scripts/install.sh:8` uses JULY_PREFIX, default ~/.local | Support/detect installation ownership explicitly; do not assume every binary is standalone. |
| Release assets | `scripts/release.sh:14` targets macOS arm64/x86_64; line 33 emits july-$version-$target.tar.gz; line 36 writes SHA256SUMS | Plan filenames are examples. Match actual packaging or change producer and consumer together. No Linux artifact promise until produced and tested. |
| Publishing | `.github/workflows/ci.yml` runs quality/build checks; no release publishing workflow found | Local packaging is not proof of a published stable release. Verify expected repository and live assets in the provider slice. |
| CLI | `src/cli/mod.rs:142` dispatches current commands | Add parser/help/dispatch and tests for update; never report success for an unimplemented stage. |
| Storage migration | `src/storage/sqlite.rs:4564` applies each migration in its own transaction; latest embedded version is 20 at line 118 | Reuse migration machinery and DatabaseTooNew protection. |
| Legacy schema | `src/storage/sqlite.rs:176` rejects pre-Phase9 Work schema and requests a fresh database | This conflicts with a blanket preservation claim; Part 9 must define supported migration boundary and safe failure. Never delete/recreate the user's DB as an update fallback. |
| State roots | Adapter store honors JULY_HOME; `src/cli/mod.rs:4610` resolves DB using JULY_WORKSPACE_DB or HOME/.july/workspace.db | JULY_HOME alone does not isolate workspace data. Test with explicit isolated DB. |

Self-update must hand control to the new binary before migration/reconciliation. Replacing a file does not replace the old process's compiled specs. Lock ownership across handoff, partial failure reporting, and migration failure after replacement belong in the updater slices.

## Sequential delivery boundaries

Beads is the authoritative status/dependency tracker. Each part receives tests/review appropriate to its scope, a commit, and a stop for Tony's next instruction.

| Part / Bead suffix | Plan workstream | Bounded deliverable |
| --- | --- | --- |
| 1 / qqg.1 | A | This source audit and dependency breakdown. |
| 2 / qqg.2 | B | version_req/install_version, existing pins preserved, SemVer validation, updated callers and stale pin assertion. |
| 3 / qqg.3 | C | Canonical executable resolution and bounded version detection, deterministic fake-executable tests. |
| 4 / qqg.4 | D | Pure compatibility states/action planning, prerelease and malformed cases, no mutation. |
| 5 / qqg.5 | E | Setup reuse/install/upgrade/report, post-install checks, persisted identity compatibility and user-config preservation. |
| 6 / qqg.6 | F/G | CLI plus stable release/asset planning; fixture-based provider tests, truthful incomplete-stage behavior. |
| 7 / qqg.7 | H | Verified HTTPS download; checksum failure preserves installation. |
| 8 / qqg.8 | I + lock | Ownership-aware safe replacement and new-binary handoff; concurrent-update/failure tests. |
| 9 / qqg.9 | J/K | Explicit migrations and reconciliation under new specs, including already-latest case. |
| 10 / qqg.10 | L + final acceptance | Reporting, partial failures and end-to-end acceptance from plan sections 34–39. |

Primary existing edit boundaries for Parts 2–5: `src/adapter/catalog.rs`, `src/adapter/store.rs`, `src/adapter/mod.rs`, `src/cli/setup.rs`, `Cargo.toml`/`Cargo.lock`. Existing-agent configuration integration additionally requires `src/cli/mod.rs`, `src/runtime/direct_message.rs`, `src/transport/acp.rs` and the relevant storage API after a bounded ownership design. Preserve custom ACP config contracts and existing tests in `tests/cli_agent.rs` and `tests/acp_transport.rs`.

## Audit verification

- `cargo test --lib cli::setup::`: **7 passed, 0 failed**.
- `cargo test --lib adapter::` at the unchanged baseline: **21 passed, 1 failed**. The failure is `npm_install_targets_the_july_prefix_with_a_pinned_version`, `src/adapter/store.rs:378`: expected Codex 1.6.2, actual catalog pin 1.10.0. This pre-existing assertion is assigned to Part 2; no runtime patch is included in this audit.
- Source tracing used Semble, CodeGraph and exhaustive literal caller searches, followed by direct source context. No installed adapters, user configuration or workspace database were mutated by the audit.
- Documentation-only delivery does not establish full-suite success or any shipped updater behavior.

Review: independent source review confirmed installer/updater boundaries; clarified shared config storage versus shape and per-migration transaction scope. Earlier migrations remain committed if a later migration fails.
