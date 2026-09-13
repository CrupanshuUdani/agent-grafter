//! `apply`: decide what the manifest implies, then act on the decision.
//!
//! The module is split in two halves on purpose, and the split is the point:
//!
//!   - [`plan`] is the decision half. It reads the manifest and the current
//!     filesystem state and returns a [`Plan`] — one [`PlannedEntry`] per unit
//!     of work, each already carrying the resolved [`Entry`] and a
//!     [`Decision`]. It never writes anything.
//!   - [`execute`] is the acting half. It takes a [`Plan`] and does exactly
//!     what the plan says, backing up anything it is about to overwrite.
//!
//! `--dry-run` and a real run compute the *same* plan from the same code;
//! dry-run then calls [`Plan::describe`] and stops, while a real run calls
//! [`execute`]. Constraint 3's "dry-run performs the full decision logic" is
//! therefore structural rather than a discipline the next editor has to
//! remember: there is no second, simplified decision path to drift away from.
//!
//! Every link, copy and removal goes through `safelink` (Constraint 5). The
//! only filesystem work this module does itself is *reading* — enumerating a
//! declared directory's files and comparing bytes to decide whether anything
//! needs doing at all.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use safelink::capability::SymlinkProbe;
use safelink::classify::{LinkClass, classify};
use safelink::materialize::{MaterializeOptions, materialize};
use safelink::op::{Entry, LinkOp};

use crate::manifest::Manifest;

/// The directory, under each target root, that replaced files are backed up
/// into. One timestamped subdirectory per `apply` invocation.
pub const BACKUP_DIR: &str = ".agrafter-backups";

/// What the planner decided about one resolved entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// The destination already satisfies the entry. Nothing to do.
    Same,
    /// The destination must be materialized.
    Write,
    /// Refused for a safety reason. The string is the user-facing explanation
    /// and is printed verbatim, so it must say what to do about it.
    Refused(String),
}

/// One planned unit of work, resolved far enough to both print and execute.
#[derive(Debug, Clone)]
pub struct PlannedEntry {
    /// The `[targets.*]` name this belongs to, or `None` when the manifest
    /// declares no targets at all.
    pub target: Option<String>,
    /// What the user sees. The declared manifest entry name, or — for a
    /// directory-copy expansion — the individual file's path relative to the
    /// source root.
    pub label: String,
    /// The manifest entry this came from, which for a directory-copy expansion
    /// is the declared *directory*, not the individual file. Distinct from
    /// `label` so a destination conflict can name the entries a user would
    /// have to edit: two entries can share a `label` while being declared
    /// separately (`copy = ["modes", "modes/one.md"]`).
    pub declared: String,
    /// The resolved unit of work, or `None` for a refusal reached before any
    /// `Entry` could be built (a missing source, a missing target root).
    pub entry: Option<Entry>,
    /// Where the destination's current content will be preserved if this
    /// entry is written. `Some` only when the planner saw a real file at the
    /// destination that a write would replace — a link is never backed up
    /// (removing one destroys nothing) and neither is an absent destination.
    pub backup: Option<PathBuf>,
    pub decision: Decision,
}

/// Everything one `apply` invocation intends to do.
#[derive(Debug, Clone)]
pub struct Plan {
    pub entries: Vec<PlannedEntry>,
    /// One UTC timestamp for the whole invocation, naming this run's backup
    /// directory. Format `YYYYMMDDTHHMMSSZ`: lexically sortable, so listing
    /// the backup directories in name order lists them in time order.
    ///
    /// Resolution is one second, so it is **not** unique across invocations:
    /// two `apply` runs inside the same second share a backup directory, and
    /// the later one's `copy_atomic` overwrites the earlier one's backup of
    /// the same file. The consequence is bounded — reduced backup *depth*,
    /// one fewer recoverable version than the directory count suggests, which
    /// matters to the retention sweep that counts these directories.
    ///
    /// The far worse *intra*-run version of this — two entries in one plan
    /// sharing a destination, where the second would back up what the first
    /// had just written and destroy the only copy of the original — is
    /// structurally impossible: `refuse_conflicting_destinations` refuses
    /// every entry of such a group at plan time, so `execute` never receives
    /// two writes for one path.
    pub run_timestamp: String,
}

/// What a run (real or dry) is reporting, and whether it ended badly.
#[derive(Debug, Clone)]
pub struct Report {
    /// One line per planned entry, in plan order.
    pub lines: Vec<String>,
    /// Any entry was refused. Maps to exit code 4.
    pub refused: bool,
    /// Any entry failed unexpectedly while being executed. Maps to exit 1.
    pub failed: bool,
}

impl Report {
    /// The report as one printable block, for assertions and diagnostics.
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }
}

impl Plan {
    /// Describe the plan without acting on it: what `--dry-run` prints.
    ///
    /// Only the mutating verbs are made hypothetical (`would write`, `would
    /// refuse`). `same` is a statement of present fact and reads identically
    /// in both modes.
    pub fn describe(&self) -> Report {
        let mut lines = Vec::with_capacity(self.entries.len());
        let mut refused = false;
        for planned in &self.entries {
            let body = match &planned.decision {
                Decision::Same => format!("same: {}", planned.label),
                Decision::Write => {
                    format!(
                        "would write: {}{}",
                        planned.label,
                        backup_note(planned, true)
                    )
                }
                Decision::Refused(reason) => {
                    refused = true;
                    format!("would refuse: {}: {reason}", planned.label)
                }
            };
            lines.push(prefixed(planned, &body));
        }
        Report {
            lines,
            refused,
            failed: false,
        }
    }
}

/// Compute the plan for `manifest`, whose `source` is resolved relative to
/// `manifest_dir` (the directory holding the manifest file — *not* the
/// process's working directory; see the module's CWD-independence test).
///
/// Reads the filesystem; writes nothing. The probe is injected rather than
/// hardcoded because whether a declared `FileSymlink` can be honored as a
/// link, or must be downgraded to a copy, changes what the correct decision
/// is — and no test can exercise that on a host whose filesystem happens to
/// support symlinks.
pub fn plan(manifest: &Manifest, manifest_dir: &Path, probe: &dyn SymlinkProbe) -> Plan {
    let run_timestamp = utc_timestamp(SystemTime::now());
    let source_root = absolutize(&manifest_dir.join(&manifest.source));
    let mut entries = Vec::new();

    let mut target_names: Vec<&String> = manifest.targets.keys().collect();
    target_names.sort();

    if target_names.is_empty() {
        // Declared entries with nowhere to go. Reporting nothing would be the
        // dishonest option (Constraint 3); this is a refusal with a reason.
        for label in manifest.link.iter().chain(manifest.copy.iter()) {
            entries.push(PlannedEntry {
                target: None,
                label: label.clone(),
                declared: label.clone(),
                entry: None,
                backup: None,
                decision: Decision::Refused(
                    "the manifest declares no [targets.*], so there is no \
                     destination to materialize into"
                        .to_string(),
                ),
            });
        }
        return Plan {
            entries,
            run_timestamp,
        };
    }

    for target_name in target_names {
        let target = &manifest.targets[target_name];
        plan_target(
            manifest,
            &TargetContext {
                source_root: &source_root,
                name: target_name,
                dest: &target.dest,
                run_timestamp: &run_timestamp,
                probe,
            },
            &mut entries,
        );
    }

    refuse_conflicting_destinations(&mut entries);

    Plan {
        entries,
        run_timestamp,
    }
}

/// Refuse every entry whose destination some other entry also claims.
///
/// Nothing in the manifest format stops two declared entries from resolving to
/// the same `dest` — `copy = ["modes", "modes/one.md"]` does it, and so does a
/// `copy` and a `link` naming the same path. Letting both run is not merely
/// redundant, it is destructive, and it lies about it:
///
///   1. the first entry backs up the user's file and overwrites it;
///   2. the second entry backs up *what the first just wrote*, overwriting the
///      backup that held the original;
///   3. both print `wrote: … (backed up to …)`, while the only copy of the
///      user's original content no longer exists anywhere.
///
/// No execution order fixes this — whichever entry runs second destroys the
/// other's backup — so it is refused before anything is written, and the whole
/// conflicting group is refused rather than an arbitrary winner picked.
///
/// Run over the finished plan rather than inside `resolve`, because a conflict
/// is a property of the plan as a whole and not of any single entry. Grouping
/// is global rather than per-target on purpose: two `[targets.*]` may point at
/// the same directory, which collides exactly the same way.
///
/// Entries already refused for another reason have no resolved `Entry` or no
/// write pending, and keep their original, more specific explanation.
fn refuse_conflicting_destinations(entries: &mut [PlannedEntry]) {
    let mut claims: HashMap<PathBuf, Vec<usize>> = HashMap::new();
    for (index, planned) in entries.iter().enumerate() {
        if let Some(entry) = &planned.entry {
            claims.entry(entry.dest.clone()).or_default().push(index);
        }
    }

    for (dest, indices) in claims {
        if indices.len() < 2 {
            continue;
        }

        let mut claimants: Vec<String> = indices
            .iter()
            .map(|&index| {
                let planned = &entries[index];
                match &planned.target {
                    Some(target) => format!("{:?} (target {target})", planned.declared),
                    None => format!("{:?}", planned.declared),
                }
            })
            .collect();
        claimants.sort();
        claimants.dedup();

        let reason = format!(
            "the destination {} is claimed by more than one declared entry: \
             {}. Refusing every entry that claims it: whichever ran second \
             would back up what the first had just written, destroying the \
             only copy of the original. Remove the overlap from the manifest.",
            dest.display(),
            claimants.join(", ")
        );

        for index in indices {
            entries[index].decision = Decision::Refused(reason.clone());
            // Nothing will be written, so nothing is to be preserved.
            entries[index].backup = None;
        }
    }
}

/// The fixed context every entry belonging to one target is planned against.
///
/// Bundled rather than threaded through as six positional parameters: every
/// planning function below needs the same set, and a positional list of three
/// same-typed paths is exactly the shape that invites a silent argument swap.
struct TargetContext<'a> {
    /// Absolute directory the manifest's declared entries are read from.
    source_root: &'a Path,
    /// The `[targets.*]` key. Reporting only.
    name: &'a str,
    /// The target root. Must already exist — see `plan_target`.
    dest: &'a Path,
    /// This invocation's backup subdirectory name.
    run_timestamp: &'a str,
    probe: &'a dyn SymlinkProbe,
}

/// Plan every declared entry for one target: `link` entries first, then
/// `copy`, matching the order the rest of the CLI reports them in.
fn plan_target(manifest: &Manifest, ctx: &TargetContext<'_>, out: &mut Vec<PlannedEntry>) {
    // Ruling 8. `MaterializeOptions` defaults to creating missing parents, and
    // its own doc comment warns that a typo in `dest` then silently builds a
    // whole tree in the wrong place. The target *root* is the one level that
    // must already exist; everything beneath it is this tool's to create.
    if !ctx.dest.is_dir() {
        let reason = format!(
            "the target root {} does not exist (or is not a directory); \
             refusing to create it, because a typo here would silently build \
             a tree in the wrong place. Create it yourself and re-run.",
            ctx.dest.display()
        );
        for label in manifest.link.iter().chain(manifest.copy.iter()) {
            out.push(refusal(ctx.name, label, reason.clone()));
        }
        return;
    }

    for name in &manifest.link {
        out.push(plan_link_entry(ctx, name));
    }
    for name in &manifest.copy {
        plan_copy_entry(ctx, name, out);
    }
}

/// A `link` entry maps 1:1 and is never expanded: a directory source becomes
/// one `DirectoryLink`, a file source one `FileSymlink` (which the engine may
/// downgrade to a copy at execution time).
fn plan_link_entry(ctx: &TargetContext<'_>, name: &str) -> PlannedEntry {
    let source = ctx.source_root.join(name);
    if !source.exists() {
        return refusal(ctx.name, name, missing_source_reason(&source));
    }
    let op = if source.is_dir() {
        LinkOp::DirectoryLink
    } else {
        LinkOp::FileSymlink
    };
    let entry = Entry {
        source,
        dest: ctx.dest.join(name),
        op,
    };
    resolve(ctx, name, name, entry)
}

/// A `copy` entry naming a file is one `FileCopy`; a `copy` entry naming a
/// directory expands to one `FileCopy` per file found beneath it, each
/// mirroring its path relative to the declared directory.
///
/// A declared directory holding no files expands to no entries, and therefore
/// reports nothing: there is no work to report, and inventing a fifth report
/// verb for "nothing was in there" is not this task's to define.
fn plan_copy_entry(ctx: &TargetContext<'_>, name: &str, out: &mut Vec<PlannedEntry>) {
    let source = ctx.source_root.join(name);
    if !source.exists() {
        out.push(refusal(ctx.name, name, missing_source_reason(&source)));
        return;
    }

    if !source.is_dir() {
        let entry = Entry {
            source,
            dest: ctx.dest.join(name),
            op: LinkOp::FileCopy,
        };
        out.push(resolve(ctx, name, name, entry));
        return;
    }

    let mut relative = Vec::new();
    if let Err(e) = collect_files(&source, Path::new(""), &mut relative) {
        out.push(refusal(
            ctx.name,
            name,
            format!(
                "could not enumerate the declared directory {}: {e}",
                source.display()
            ),
        ));
        return;
    }

    for rel in relative {
        let label = Path::new(name).join(&rel).to_string_lossy().into_owned();
        let entry = Entry {
            source: source.join(&rel),
            dest: ctx.dest.join(name).join(&rel),
            op: LinkOp::FileCopy,
        };
        out.push(resolve(ctx, name, &label, entry));
    }
}

/// `path` made absolute and lexically tidied, without resolving symlinks.
///
/// The source root has to be absolute before it reaches `safelink`, and this
/// is not cosmetic: a POSIX symlink stores its target verbatim and resolves it
/// relative to the **link's own directory**, not to the working directory of
/// whatever created it. A relative source — which is the default, since
/// `source` defaults to `"."` and `--manifest` defaults to a bare file name —
/// would therefore produce a directory link that silently points nowhere. The
/// link is created successfully, `classify` calls it a link, and only reading
/// through it fails.
///
/// `canonicalize` would also make it absolute, but it additionally resolves
/// every symlink along the way (so a deliberately symlinked source directory
/// would be replaced by its target) and returns Windows verbatim `\\?\` paths
/// that leak into user-facing messages. Neither is wanted here.
fn absolutize(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(path),
            // The working directory is gone or unreadable (it was deleted, or
            // the process cannot stat it). There is nothing to resolve
            // against, so the path is returned as-is and whatever the entry
            // does next is decided by the ordinary source/destination checks.
            // Not a graceful degradation: if a same-named tree happens to sit
            // under the eventual working directory, a `link` entry can still
            // produce a relative link target. Left unhandled because
            // `current_dir()` failing at all means the process is already in a
            // state nothing here can reason about.
            Err(_) => path.to_path_buf(),
        }
    };
    // Drop `.` components, which `join` leaves behind and which would show up
    // verbatim in link targets and error messages. Purely lexical: `..` is
    // left alone, because removing it without touching the filesystem is
    // wrong in the presence of symlinks.
    absolute
        .components()
        .filter(|c| !matches!(c, std::path::Component::CurDir))
        .collect()
}

/// Every real file beneath `dir`, as paths relative to it, in sorted order.
///
/// Sorted because `read_dir` order is unspecified, and an `apply` whose
/// output lines shuffle between runs is a report nobody can diff.
///
/// Recursion follows *real* directories only — a directory symlink inside the
/// source is a traversal leaf, both because descending one could loop forever
/// and because `safelink` treats every link as a boundary (Constraint 5).
fn collect_files(dir: &Path, prefix: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    let mut names: Vec<PathBuf> = std::fs::read_dir(dir)?
        .map(|e| e.map(|e| e.file_name().into()))
        .collect::<io::Result<_>>()?;
    names.sort();

    for name in names {
        let path = dir.join(&name);
        let rel = prefix.join(&name);
        let meta = std::fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            out.push(rel);
        } else if meta.is_dir() {
            collect_files(&path, &rel, out)?;
        } else {
            out.push(rel);
        }
    }
    Ok(())
}

/// Turn one resolved `Entry` into a `PlannedEntry` by deciding what the
/// current state of its destination means.
fn resolve(ctx: &TargetContext<'_>, declared: &str, label: &str, entry: Entry) -> PlannedEntry {
    let decision = decide(&entry, ctx.probe);
    let backup = if decision == Decision::Write {
        backup_path(&entry.dest, ctx.dest, ctx.run_timestamp)
    } else {
        None
    };
    PlannedEntry {
        target: Some(ctx.name.to_string()),
        label: label.to_string(),
        declared: declared.to_string(),
        entry: Some(entry),
        backup,
        decision,
    }
}

/// Decide what one entry's destination needs.
fn decide(entry: &Entry, probe: &dyn SymlinkProbe) -> Decision {
    if is_same(entry) {
        return Decision::Same;
    }

    match classify(&entry.dest) {
        // Nothing is there. A clean write.
        Err(e) if e.kind() == io::ErrorKind::NotFound => Decision::Write,
        Err(e) => Decision::Refused(format!(
            "could not inspect the destination {}: {e}",
            entry.dest.display()
        )),
        // A link of the wrong kind. Replacing it destroys nothing (the target
        // it points at is untouched), so `execute` unlinks it via
        // `safelink::remove::remove_link` and materializes over it.
        Ok(LinkClass::DirLink) | Ok(LinkClass::FileLink) => Decision::Write,
        Ok(LinkClass::NotALink) => decide_over_real_data(entry, probe),
    }
}

/// The destination holds real data — a file or a directory this tool did not
/// create. Constraint 4: overwriting it is opt-in, backed up, or refused.
fn decide_over_real_data(entry: &Entry, probe: &dyn SymlinkProbe) -> Decision {
    if entry.dest.is_dir() {
        let occupied = std::fs::read_dir(&entry.dest)
            .map(|mut d| d.next().is_some())
            .unwrap_or(true);
        return Decision::Refused(if occupied {
            format!(
                "a non-empty directory already exists at {}; refusing to \
                 replace or merge into it. Merge it by hand (or move it \
                 aside) and re-run.",
                entry.dest.display()
            )
        } else {
            format!(
                "an empty directory already exists at {}; refusing to replace \
                 it. Remove the directory and re-run.",
                entry.dest.display()
            )
        });
    }

    // A real file. Only the two copy-shaped operations can replace one
    // safely: `copy_atomic` renames the new content over it in a single step,
    // so the backup taken first is the complete record of what was there.
    match entry.op {
        LinkOp::FileCopy => Decision::Write,
        LinkOp::FileSymlink if !probe.can_create_file_symlink(&entry.dest) => Decision::Write,
        // A link cannot be created on top of an existing file, and deleting
        // real data to make room for one is not a default this tool takes.
        // Reached only when the file's content differs from the source (an
        // identical file is already `same`).
        LinkOp::FileSymlink | LinkOp::DirectoryLink => Decision::Refused(format!(
            "a real file already exists at {} and this entry is declared as a \
             link; refusing to delete real data to make room for one. Move \
             the file aside and re-run, or declare the entry under `copy`.",
            entry.dest.display()
        )),
    }
}

/// Whether the destination already satisfies the entry (Ruling 4).
///
/// **Known gap, deliberately left open:** this never checks where an existing
/// link *points*. `safelink::classify` reports only the *kind* of object at a
/// path, not its resolved target, so a link left over from before a manifest
/// edit — pointing at a source that is no longer declared — is reported here
/// as `same`. Closing that needs drift detection that can read a link's
/// target, which belongs to `doctor` (Task S4) and does not exist yet. Do not
/// silently "improve" this into a target comparison without that support.
///
/// The `FileCopy`/`FileSymlink` arm compares bytes as well as kind, and it has
/// to: `materialize` may downgrade a declared `FileSymlink` to a plain copy
/// when the destination's filesystem cannot hold a symlink, after which the
/// destination is an ordinary file that `classify` cannot tell apart from one
/// that was never materialized at all. Kind alone would rewrite it on every
/// single run.
fn is_same(entry: &Entry) -> bool {
    match entry.op {
        LinkOp::DirectoryLink => matches!(classify(&entry.dest), Ok(LinkClass::DirLink)),
        LinkOp::FileCopy | LinkOp::FileSymlink => {
            matches!(classify(&entry.dest), Ok(LinkClass::FileLink))
                || has_identical_bytes(&entry.source, &entry.dest)
        }
    }
}

/// Whether `dest` is an ordinary file holding exactly `source`'s bytes.
///
/// Deliberately not a timestamp or size comparison: size alone calls an
/// edited file of the same length unchanged, and modification times are not
/// preserved by `copy_atomic` (nor comparable across the volumes this tool
/// exists to span). Content is the only question that has a correct answer.
fn has_identical_bytes(source: &Path, dest: &Path) -> bool {
    let Ok(dest_meta) = std::fs::symlink_metadata(dest) else {
        return false;
    };
    if !dest_meta.is_file() {
        return false;
    }
    let Ok(source_meta) = std::fs::metadata(source) else {
        return false;
    };
    if !source_meta.is_file() || source_meta.len() != dest_meta.len() {
        return false;
    }
    match (std::fs::read(source), std::fs::read(dest)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Where `dest`'s current content should be preserved before it is replaced,
/// or `None` when there is nothing worth preserving (Ruling 7).
///
/// Only a **real file** is ever backed up. A link is not: removing one
/// destroys no data, since the thing it points at is untouched. A destination
/// that does not exist has nothing to preserve either.
fn backup_path(dest: &Path, target_dest: &Path, run_timestamp: &str) -> Option<PathBuf> {
    if !matches!(classify(dest), Ok(LinkClass::NotALink)) {
        return None;
    }
    if !std::fs::symlink_metadata(dest).map(|m| m.is_file()).ok()? {
        return None;
    }
    let relative = dest.strip_prefix(target_dest).ok()?;
    Some(
        target_dest
            .join(BACKUP_DIR)
            .join(run_timestamp)
            .join(relative),
    )
}

/// Carry out `plan`, backing up before anything destructive.
///
/// Acts only on what the plan already decided: nothing here re-derives
/// whether an entry should be written. An entry the planner marked `Refused`
/// is reported and skipped — a refusal stops that entry, never the run
/// (Ruling 10).
pub fn execute(plan: &Plan, probe: &dyn SymlinkProbe) -> Report {
    let mut lines = Vec::with_capacity(plan.entries.len());
    let mut refused = false;
    let mut failed = false;

    for planned in &plan.entries {
        match &planned.decision {
            Decision::Same => lines.push(prefixed(planned, &format!("same: {}", planned.label))),
            Decision::Refused(reason) => {
                refused = true;
                // Reported, then skipped. One entry's refusal must never
                // decide the fate of the others (Ruling 10); it only sets the
                // run's exit code.
                lines.push(prefixed(
                    planned,
                    &format!("refused: {}: {reason}", planned.label),
                ));
            }
            Decision::Write => {
                let body = match write_entry(planned, probe) {
                    Ok(()) => format!("wrote: {}{}", planned.label, backup_note(planned, false)),
                    Err(e) => {
                        failed = true;
                        format!("failed: {}: {e}", planned.label)
                    }
                };
                lines.push(prefixed(planned, &body));
            }
        }
    }

    Report {
        lines,
        refused,
        failed,
    }
}

/// Back up, clear the way, and materialize one entry — in that order.
///
/// The backup is taken first and unconditionally: if it fails, nothing is
/// overwritten. There is no path here that removes a directory, recursively
/// or otherwise; the only removal is `safelink::remove::remove_link`, which
/// refuses anything that is not a link.
fn write_entry(planned: &PlannedEntry, probe: &dyn SymlinkProbe) -> io::Result<()> {
    let entry = planned.entry.as_ref().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "internal error: a write was planned without a resolved entry",
        )
    })?;

    if let Some(backup) = &planned.backup {
        safelink::copy::copy_atomic(&entry.dest, backup).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!(
                    "could not back up {} to {}: {e} -- refusing to overwrite \
                     it without a backup",
                    entry.dest.display(),
                    backup.display()
                ),
            )
        })?;
    }

    // A link cannot be created on top of an existing object, so a link of the
    // wrong kind is unlinked first. Only a *link* can reach this: a real file
    // or directory at `dest` was either `same` or refused during planning, and
    // `remove_link` refuses anything that is not a link anyway — it never
    // traverses, so the thing the link points at is untouched.
    if matches!(
        classify(&entry.dest),
        Ok(LinkClass::DirLink | LinkClass::FileLink)
    ) {
        safelink::remove::remove_link(&entry.dest)?;
    }

    materialize(entry, probe, &MaterializeOptions::default()).map(|_| ())
}

/// A refusal that never got as far as a resolved `Entry`.
fn refusal(target_name: &str, label: &str, reason: String) -> PlannedEntry {
    PlannedEntry {
        target: Some(target_name.to_string()),
        label: label.to_string(),
        declared: label.to_string(),
        entry: None,
        backup: None,
        decision: Decision::Refused(reason),
    }
}

fn missing_source_reason(source: &Path) -> String {
    format!(
        "the declared source {} does not exist; nothing was written. If it \
         lives on a removable or network volume, mount it and re-run.",
        source.display()
    )
}

/// Prefix a report line with its target name, when there is one.
fn prefixed(planned: &PlannedEntry, body: &str) -> String {
    match &planned.target {
        Some(target) => format!("{target}: {body}"),
        None => body.to_string(),
    }
}

/// The parenthetical that names where a replaced file's content went.
fn backup_note(planned: &PlannedEntry, hypothetical: bool) -> String {
    match &planned.backup {
        Some(path) => {
            let verb = if hypothetical {
                "would back up to"
            } else {
                "backed up to"
            };
            format!(" ({verb} {})", path.display())
        }
        None => String::new(),
    }
}

/// `YYYYMMDDTHHMMSSZ` in UTC — lexically sortable, so backup directories sort
/// by name in the same order they were created.
///
/// Hand-rolled rather than pulled from a date crate: this is the only date
/// formatting in the whole binary, and the conversion below is exact for every
/// representable instant.
fn utc_timestamp(now: SystemTime) -> String {
    let secs = now
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (year, month, day) = civil_from_days((secs / 86_400) as i64);
    let sod = secs % 86_400;
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        sod / 3600,
        (sod % 3600) / 60,
        sod % 60
    )
}

/// Days since 1970-01-01 to a proleptic-Gregorian civil date.
///
/// Howard Hinnant's `civil_from_days`, the standard branch-free formulation
/// (it shifts the year to start in March so the leap day lands last).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(secs: u64) -> String {
        utc_timestamp(UNIX_EPOCH + Duration::from_secs(secs))
    }

    #[test]
    fn timestamps_are_utc_and_lexically_sortable() {
        assert_eq!(at(0), "19700101T000000Z");
        assert_eq!(at(1_700_000_000), "20231114T221320Z");
        // A leap day, which the shifted-year arithmetic is the reason for.
        assert_eq!(at(1_709_209_845), "20240229T123045Z");
        assert!(at(0) < at(1_700_000_000), "must sort in time order");
    }
}
