//! Argument parsing, subcommand dispatch, and exit codes.
//!
//! `apply --dry-run` and `status` have real (read-only) behavior against the
//! hardened `manifest` format (Task S2): both iterate over every declared
//! `[targets.*]` entry. `doctor`, `unlink`, and `explain` remain stub
//! subcommands that parse their own args and exit 0. Later tasks fill those
//! in (S3: real `apply`/`unlink`; S4: `doctor`).

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

use crate::manifest::{self, Manifest};

/// The exit codes this CLI promises to scripts and hooks. See the task
/// brief's table — these values are load-bearing, not incidental.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCode {
    /// Success, or `status` found everything in sync.
    Success = 0,
    /// Unexpected error.
    UnexpectedError = 1,
    /// Invalid usage or unparseable manifest.
    InvalidUsage = 2,
    /// `status`/`doctor` found drift (not an error — a finding).
    DriftFound = 3,
    /// Refused for a safety reason (occupied destination, missing source).
    Refused = 4,
}

impl ExitCode {
    pub fn code(self) -> u8 {
        self as u8
    }
}

impl From<ExitCode> for std::process::ExitCode {
    fn from(value: ExitCode) -> Self {
        std::process::ExitCode::from(value.code())
    }
}

const DEFAULT_MANIFEST: &str = "agent-grafter.toml";

#[derive(Parser)]
#[command(name = "agent-grafter", version, about, disable_help_subcommand = true)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Materialize the manifest's declared links/copies onto disk.
    Apply(ApplyArgs),
    /// Report whether declared entries are already correctly in place.
    Status(StatusArgs),
    /// Diagnose the current install. Stub in S1 (real behavior: S4).
    Doctor(DoctorArgs),
    /// Remove previously materialized entries. Stub in S1 (real behavior: S3).
    Unlink(UnlinkArgs),
    /// Explain what a command would do without running it. Stub in S1.
    Explain(ExplainArgs),
}

#[derive(clap::Args)]
struct ApplyArgs {
    /// Report what would change without touching the filesystem.
    #[arg(long)]
    dry_run: bool,
    #[arg(long, default_value = DEFAULT_MANIFEST)]
    manifest: PathBuf,
}

#[derive(clap::Args)]
struct StatusArgs {
    #[arg(long, default_value = DEFAULT_MANIFEST)]
    manifest: PathBuf,
}

#[derive(clap::Args)]
struct DoctorArgs {}

#[derive(clap::Args)]
struct UnlinkArgs {}

#[derive(clap::Args)]
struct ExplainArgs {}

/// Parse arguments from the process environment and dispatch, returning
/// the exit code the process should terminate with.
pub fn run() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            // clap's own formatted usage/help/version text. `--help` and
            // `--version` are not usage errors (clap reports them as one
            // internally so it can print-and-exit early) — only a genuine
            // parse failure should map to our code 2.
            let _ = err.print();
            return if err.exit_code() == 0 {
                ExitCode::Success
            } else {
                ExitCode::InvalidUsage
            };
        }
    };

    match cli.command {
        Command::Apply(args) => apply(&args),
        Command::Status(args) => status(&args),
        Command::Doctor(_) => ExitCode::Success,
        Command::Unlink(_) => ExitCode::Success,
        Command::Explain(_) => ExitCode::Success,
    }
}

/// `apply` (`--dry-run` or not): read the manifest and report what its
/// declared entries are. S1 does not implement real materialization (that
/// is S3's job — idempotent apply, backup, prune, all of which Constraint 4
/// requires before any destination may be touched), so there is nothing to
/// mutate yet. `--dry-run` prints the same listing phrased as hypothetical
/// ("would link: …") and is unconditionally honest: nothing on disk is ever
/// touched. Plain `apply` prints the listing too, but says so on stderr —
/// it must never look like it completed a materialization it didn't do.
fn apply(args: &ApplyArgs) -> ExitCode {
    let manifest = match manifest::read(&args.manifest) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::InvalidUsage;
        }
    };

    report(&manifest, args.dry_run);
    if !args.dry_run {
        eprintln!(
            "note: apply does not materialize anything yet in this version \
             (real apply lands in a later task); this was a listing only"
        );
    }
    ExitCode::Success
}

/// Read-only listing of every declared entry, for every declared target.
/// Target names are sorted for deterministic output; this is still just a
/// listing (real per-target materialization is Task S3's job), so each line
/// is prefixed with the target name it would apply to.
///
/// A manifest with no `[targets.*]` at all still has declared entries worth
/// reporting honestly (Constraint 3) — it falls back to an unprefixed
/// listing rather than silently printing nothing.
fn report(manifest: &Manifest, dry_run: bool) {
    let prefix = if dry_run { "would " } else { "" };
    let targets = sorted_target_names(manifest);

    if targets.is_empty() {
        for name in &manifest.link {
            println!("{prefix}link: {name}");
        }
        for name in &manifest.copy {
            println!("{prefix}copy: {name}");
        }
        return;
    }

    for target_name in targets {
        for name in &manifest.link {
            println!("{target_name}: {prefix}link: {name}");
        }
        for name in &manifest.copy {
            println!("{target_name}: {prefix}copy: {name}");
        }
    }
}

/// `status`: read the manifest, and for each declared target, resolve each
/// declared `link` entry's destination as `target.dest.join(entry_name)` and
/// use `safelink::classify` on it to decide whether it is already correctly
/// materialized. Either a nonexistent destination or one that classifies as
/// `NotALink` counts as drift. Exit 3 if any entry drifted (in any target),
/// 0 if all are in sync.
///
/// A manifest that declares `link` entries but no `[targets.*]` at all has
/// nothing to check them against; that is drift, not a silent "in sync" —
/// there is no destination anywhere that could be correctly materialized.
fn status(args: &StatusArgs) -> ExitCode {
    let manifest = match manifest::read(&args.manifest) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::InvalidUsage;
        }
    };

    let mut drifted = false;
    let targets = sorted_target_names(&manifest);

    if targets.is_empty() {
        for name in &manifest.link {
            println!("drift: {name} (no targets declared)");
            drifted = true;
        }
    } else {
        for target_name in targets {
            let target = &manifest.targets[target_name];
            for name in &manifest.link {
                let dest = target.dest.join(name);
                match entry_in_sync(&dest) {
                    Ok(true) => println!("{target_name}: in sync: {name}"),
                    Ok(false) => {
                        println!("{target_name}: drift: {name}");
                        drifted = true;
                    }
                    Err(()) => return ExitCode::UnexpectedError,
                }
            }
        }
    }

    if drifted {
        ExitCode::DriftFound
    } else {
        ExitCode::Success
    }
}

/// Target names in a deterministic (sorted) order, since `Manifest::targets`
/// is a `HashMap` and iteration order over it is otherwise unspecified.
fn sorted_target_names(manifest: &Manifest) -> Vec<&String> {
    let mut names: Vec<&String> = manifest.targets.keys().collect();
    names.sort();
    names
}

/// Returns `Ok(true)` if in sync, `Ok(false)` if drifted, `Err(())` for a
/// genuine (non-"missing") I/O failure while classifying.
fn entry_in_sync(dest: &Path) -> Result<bool, ()> {
    match safelink::classify::classify(dest) {
        Ok(safelink::classify::LinkClass::NotALink) => Ok(false),
        Ok(_) => Ok(true),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(false),
        Err(_) => Err(()),
    }
}

#[cfg(test)]
mod tests {
    use super::ExitCode;

    #[test]
    fn exit_codes_match_the_documented_table() {
        assert_eq!(ExitCode::Success.code(), 0);
        assert_eq!(ExitCode::UnexpectedError.code(), 1);
        assert_eq!(ExitCode::InvalidUsage.code(), 2);
        assert_eq!(ExitCode::DriftFound.code(), 3);
        assert_eq!(ExitCode::Refused.code(), 4);
    }
}
