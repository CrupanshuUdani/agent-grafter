//! Generates the `agent-grafter(1)` man page from the same `clap::Command`
//! the CLI itself parses with, so the page cannot drift from the real CLI
//! the way a hand-written roff page could.
//!
//! Deliberately a `cargo` example (not a `build.rs`, not a `[[bin]]`): a
//! `build.rs` that writes files runs on every consumer build, including
//! `cargo install`, and complicates `cargo publish`'s sandbox and
//! reproducible builds. A `[[bin]]` would avoid that but still land a
//! second, generically-named `gen-manpage` binary on a consumer's PATH via
//! `cargo install`. An `examples/` target backed by a dev-dependency
//! (`clap_mangen`, in `Cargo.toml`) gets neither problem: `cargo install`
//! ignores both examples and dev-dependencies, while `cargo build
//! --all-targets`, `cargo test`, and `cargo clippy --all-targets` all still
//! compile it, so a broken generator cannot go unnoticed in CI. The man
//! page itself is only needed at packaging time, so it is generated on
//! demand by running this example — it is not committed as a source file,
//! and it is not wired into any release workflow here (that is a later,
//! separate task's scope).
//!
//! Usage: `cargo run --example gen-manpage -- <output-dir>` (defaults to
//! the current directory when no argument is given). Writes
//! `agent-grafter.1`.

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() -> std::io::Result<()> {
    let out_dir = env::args().nth(1).map(PathBuf::from).unwrap_or_default();

    let cmd = agent_grafter::cli::build_cli();
    let man = clap_mangen::Man::new(cmd);
    let mut buffer: Vec<u8> = Vec::new();
    man.render(&mut buffer)?;

    if !out_dir.as_os_str().is_empty() {
        fs::create_dir_all(&out_dir)?;
    }
    let out_path = out_dir.join("agent-grafter.1");
    fs::write(&out_path, buffer)?;
    eprintln!("wrote {}", out_path.display());
    Ok(())
}
