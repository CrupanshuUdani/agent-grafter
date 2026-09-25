## What and why

<!-- Briefly describe what this PR does and why it is needed. -->

## How it was verified

- [ ] `cargo fmt --all -- --check` passes locally
- [ ] `cargo clippy --all-targets -- -D warnings` passes locally
- [ ] `cargo test --all-targets` passes locally
- [ ] Windows pre-flight `cargo check --target x86_64-pc-windows-gnu --workspace --all-targets` passes (if `cfg(windows)` code changed)
- [ ] Windows behaviour change verified on a real Windows machine — paste output

## Docs

- [ ] CHANGELOG.md `[Unreleased]` updated if user-visible
