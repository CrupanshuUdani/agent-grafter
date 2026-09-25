//! Argument parsing, subcommand dispatch, and exit codes.
//!
//! `apply` really materializes (Task S3a) and reconciles (Task S3b): it
//! computes a plan via `crate::apply::plan` — writes, prunes and the backup
//! retention sweep, all decided together — and, unless `--dry-run` was passed,
//! executes it. Prune and retention need no wiring of their own here: they
//! live in the same `Plan`, so both modes get them by construction rather than
//! by a second code path remembering to ask. `status` does read-only drift
//! checking against `link` entries only. `doctor` is also read-only, but wider:
//! it checks every declared `copy` and `link` entry (by reusing
//! `crate::apply::plan`, the same decision `apply` itself would reach),
//! link-target drift `is_same()` cannot see, and each target's backup-root
//! health — see `crate::doctor` for the full breakdown. `unlink` really
//! removes (Task S8), using the same plan/execute split `apply` does and the
//! same exit-code mapping: it removes links via `safelink` and plain files
//! that still match their declared source, and refuses everything else — see
//! `crate::unlink` for what it will and will not touch, and for why it never
//! restores a backup. There is no `explain` subcommand: `apply --dry-run`
//! already prints the full plan, and clap's own `help` subcommand (see below)
//! covers the rest of what a stub named `explain` would have offered.

use std::io::{ErrorKind, IsTerminal};
use std::path::{Path, PathBuf};

use clap::{CommandFactory, Parser, Subcommand};

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
#[command(name = "agent-grafter", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// The `clap::Command` this CLI parses with, exposed for tools that need the
/// definition without going through `run()` — namely the man-page generator
/// (`src/bin/gen-manpage.rs`), so the generated page can never drift from
/// the real CLI the way a hand-written roff page could.
pub fn build_cli() -> clap::Command {
    Cli::command()
}

#[derive(Subcommand)]
enum Command {
    /// Materialize the manifest's declared links/copies onto disk.
    Apply(ApplyArgs),
    /// Report whether declared entries are already correctly in place.
    Status(StatusArgs),
    /// Diagnose the current install: read-only checks, never a write.
    Doctor(DoctorArgs),
    /// Remove previously materialized entries: links, and copies that still
    /// match their declared source. Refuses anything else.
    Unlink(UnlinkArgs),
    /// Generate a starter manifest: prompts interactively, or `--print`s a
    /// commented template to stdout without touching anything.
    Init(InitArgs),
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
struct DoctorArgs {
    #[arg(long, default_value = DEFAULT_MANIFEST)]
    manifest: PathBuf,
}

#[derive(clap::Args)]
struct UnlinkArgs {
    /// Report what would be removed without touching the filesystem.
    #[arg(long)]
    dry_run: bool,
    #[arg(long, default_value = DEFAULT_MANIFEST)]
    manifest: PathBuf,
}

#[derive(clap::Args)]
struct InitArgs {
    /// Overwrite an existing manifest instead of refusing.
    #[arg(long)]
    force: bool,
    /// Print a commented template to stdout instead of prompting; writes
    /// nothing, and works without a terminal.
    #[arg(long)]
    print: bool,
    #[arg(long, default_value = DEFAULT_MANIFEST)]
    manifest: PathBuf,
}

/// Parse arguments from the process environment and dispatch, returning
/// the exit code the process should terminate with.
pub fn run() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            // clap's own formatted usage/help/version text. `--help`,
            // `--version`, and — since `disable_help_subcommand` was removed
            // — the `help` subcommand (and `help <SUBCOMMAND>`) all reach
            // this same branch: clap reports each as an error internally so
            // it can print-and-exit early, not because any of them are a
            // usage mistake. Only a genuine parse failure should map to our
            // code 2.
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
        Command::Doctor(args) => doctor(&args),
        Command::Unlink(args) => unlink(&args),
        Command::Init(args) => init(&args),
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

    let manifest_dir = manifest_dir(&args.manifest);
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

/// `unlink` (`--dry-run` or not): compute the full removal plan, then either
/// print it or carry it out.
///
/// Wired exactly like `apply` above, and for the same reason (Constraint 3):
/// both modes run the identical decision logic in `crate::unlink::plan`, so
/// `--dry-run` reports the same exit code a real run would and a CI gate on
/// exit 4 behaves the same either way.
///
/// The exit-code mapping is `apply`'s, minus code 3: `unlink` is not a drift
/// checker, so it never reports a finding — 2 for an unreadable or
/// unparseable manifest, 4 for anything it refused to remove, 1 for a genuine
/// I/O failure while removing, 0 otherwise.
///
/// No `SymlinkProbe` is passed. `crate::unlink::plan` takes none: a probe
/// answers whether a symlink can be *created*, which is not a question this
/// command asks — see that function's own doc comment.
fn unlink(args: &UnlinkArgs) -> ExitCode {
    let manifest = match manifest::read(&args.manifest) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::InvalidUsage;
        }
    };

    let plan = crate::unlink::plan(&manifest, &manifest_dir(&args.manifest));
    let report = if args.dry_run {
        plan.describe()
    } else {
        crate::unlink::execute(&plan)
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

/// Wire clap's args to the init flow, supply the terminal check and prompter,
/// and map the outcome to an exit code.
fn init(args: &InitArgs) -> ExitCode {
    let is_tty = std::io::stdin().is_terminal();
    let init_args = crate::init::InitArgs {
        manifest_path: args.manifest.clone(),
        force: args.force,
        print: args.print,
    };
    let mut prompter = crate::init::CliclackPrompter;

    match crate::init::run(&init_args, is_tty, &mut prompter) {
        crate::init::InitOutcome::Printed(contents) => {
            print!("{contents}");
            ExitCode::Success
        }
        crate::init::InitOutcome::Wrote { path, .. } => {
            println!("wrote {}", path.display());
            ExitCode::Success
        }
        crate::init::InitOutcome::NonInteractive => {
            eprintln!(
                "init needs an interactive terminal; run \
                 `agent-grafter init --print > {DEFAULT_MANIFEST}` instead"
            );
            ExitCode::InvalidUsage
        }
        crate::init::InitOutcome::AlreadyExists(path) => {
            eprintln!(
                "{} already exists; re-run with --force to overwrite it",
                path.display()
            );
            ExitCode::Refused
        }
        crate::init::InitOutcome::InvalidGenerated(e) => {
            eprintln!("generated manifest is invalid: {e}");
            ExitCode::InvalidUsage
        }
        crate::init::InitOutcome::Cancelled => {
            eprintln!("init cancelled; nothing written");
            ExitCode::UnexpectedError
        }
        crate::init::InitOutcome::Io(e) => {
            eprintln!("init failed: {e}");
            ExitCode::UnexpectedError
        }
    }
}

/// The manifest's own directory: `parent()` for a real path, `.` for a bare
/// file name (whose `parent()` is empty) — spelled explicitly rather than
/// relied on as an accident of `Path::join("")`. `apply` and `doctor` both
/// resolve the manifest's declared `source` relative to this, not to
/// wherever the process happens to have been started.
fn manifest_dir(manifest_path: &Path) -> PathBuf {
    match manifest_path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// `doctor`: read-only diagnostics over the current install. See
/// `crate::doctor` for what each check inspects and why; this function only
/// wires the manifest read, the resolved manifest directory, and the exit
/// code together.
///
/// Never refuses (it never writes), so it never exits 4 — the exit codes
/// this maps to are exactly `status`'s: 2 for an unreadable/unparseable
/// manifest, 1 for a genuine I/O failure while inspecting the filesystem,
/// 3 for drift, 0 otherwise.
fn doctor(args: &DoctorArgs) -> ExitCode {
    let manifest = match manifest::read(&args.manifest) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::InvalidUsage;
        }
    };

    let dir = manifest_dir(&args.manifest);
    let probe = safelink::capability::RealProbe;
    let report = crate::doctor::diagnose(&manifest, &args.manifest, &dir, &probe);

    for line in &report.lines {
        println!("{line}");
    }

    if report.failed {
        ExitCode::UnexpectedError
    } else if report.drifted {
        ExitCode::DriftFound
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
