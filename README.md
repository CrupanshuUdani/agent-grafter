# agent-grafter

CLI that materializes declared AI agent config/state file pairs across
Windows/macOS/Linux.

This file is intentionally minimal — it exists to state the project's
Requirements, not as a full project guide. See `crates/agent-grafter/` and
`crates/safelink/` for the crates themselves, including `safelink`'s own
README for the CI-coverage caveats around its Windows link-capability
detection.

## Requirements

Minimum Supported Rust Version (MSRV): **1.88.0**.

This is not the edition-2024 floor (1.85.0) — it's higher because this
codebase uses if-let chains (stabilized in 1.88.0), e.g. in
`crates/agent-grafter/src/manifest.rs` and `apply.rs`. The workspace's
`rust-version = "1.88"` (root `Cargo.toml`) enforces this at `cargo check`
time, and CI runs a dedicated job pinned to exactly this toolchain so the
claim stays true.
