//! Library surface for `agent-grafter`, split out from the thin binary
//! (`main.rs`) so integration tests (`tests/manifest.rs`) can exercise
//! `manifest::parse` directly on raw TOML strings, without going through
//! the CLI or a file on disk.

pub mod apply;
pub mod cli;
pub mod manifest;
