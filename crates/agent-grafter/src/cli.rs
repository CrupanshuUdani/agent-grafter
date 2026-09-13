//! Argument parsing, subcommand dispatch, and exit codes.
//!
//! `apply` really materializes (Task S3a) and reconciles (Task S3b): it
//! computes a plan via `crate::apply::plan` — writes, prunes and the backup
//! retention sweep, all decided together — and, unless `--dry-run` was passed,
//! executes it. Prune and retention need no wiring of their own here: they
//! live in the same `Plan`, so both modes get them by construction rather than
//! by a second code path remembering to ask. `status` does read-only drift
//! checking. `doctor`, `unlink`, and `explain` remain stub subcommands that
//! parse their own args and exit 0; Task S4 fills those in.

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

/// `apply` (`--dry-run` or not): compute the full plan, then either print it
/// or carry it out.
///
/// Both modes run the identical decision logic in `crate::apply::plan`
/// (Constraint 3): `--dry-run` describes the plan and stops, a real run
/// additionally executes it — backing up anything it is about to overwrite.
/// Because the exit code is derived from the plan and not from what happened
/// during execution, `--dry-run` reports the same code a real run would, so a
/// CI gate on exit 4 behaves the same either way.
fn apply(args: &ApplyArgs) -> ExitCode {
    let manifest = match manifest::read(&args.manifest) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::InvalidUsage;
        }
    };

    // The manifest's `source` is documented as relative to the manifest file,
    // not to wherever the process happens to have been started. `parent()` is
    // empty for a bare file name like the default `agent-grafter.toml`, which
    // means the current directory — spelled explicitly rather than relied on
    // as an accident of `Path::join("")`.
    let manifest_dir = match args.manifest.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };

    let probe = safelink::capability::RealProbe;
    let plan = crate::apply::plan(&manifest, &manifest_dir, &probe);
    let report = if args.dry_run {
        plan.describe()
    } else {
        crate::apply::execute(&plan, &probe)
    };

    for line in &report.lines {
        println!("{line}");
    }

    if report.failed {
        ExitCode::UnexpectedError
    } else if report.refused {
        ExitCode::Refused
    } else {
        ExitCode::Success
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
