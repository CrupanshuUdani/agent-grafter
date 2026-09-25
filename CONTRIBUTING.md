# Contributing to agent-grafter

## Running the suite

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```

MSRV is 1.88.0 (let-chains). Check with `cargo +1.88.0 test --workspace --locked` if that toolchain is installed. CI's `msrv` job is the backstop.

## Windows pre-flight

```bash
rustup target add x86_64-pc-windows-gnu
cargo check --target x86_64-pc-windows-gnu --workspace --all-targets
```

`#[cfg(windows)]` branches never compile on Linux, so this catches typos and missing imports in seconds instead of a CI round-trip.

This proves compilation only, never behaviour. A dangling-junction bug once compiled cleanly and still deleted nothing.

## Windows claims need Windows evidence

A Windows-only behavioural claim needs evidence from a real Windows run (paste output), not a green Linux run. Note CI's Windows runner is elevated, so it cannot verify the non-admin path. State whether you tested as a standard user and with Developer Mode on or off.

## Safety rules

No `remove_dir_all` in either crate's `src/`. A test enforces this. A new removal path needs a test that fails against the unsafe version first.

## Pull requests

Branch off `main`, one concern per PR, all required CI checks green before review.
