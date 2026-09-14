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
//! Every link creation and every link removal goes through `safelink`
//! (Constraint 5). The filesystem work this module does itself is *reading* —
//! enumerating a declared directory's files and comparing bytes to decide
//! whether anything needs doing — plus two narrow deletions of this tool's own
//! prior output: unlinking a copy whose source is gone ([`prune_one`]) and
//! sweeping an expired backup directory (`crate::retention`). Both use the
//! narrowest primitive that can do the job, both after `safelink::classify`
//! has confirmed what they are about to touch, and neither ever removes a
//! link.
//!
//! **Prune's scope is the single most important safety property here.** It
//! reconciles a declared `copy` directory's own destination subtree against
//! that subtree's current source contents, and it never enumerates — let alone
//! touches — anything at a target root outside those subtrees. See
//! [`prune_scope`] for why that is structural rather than a convention.
//!
//! *One deliberate limitation.* Prune removes stale **files**, never
//! directories. A directory that emptied because every file under it left the
//! source is left behind as an empty directory. That matches the requirement
//! this implements ("a file deleted from the source disappears from the
//! destination"), and the cost of the alternative is poor: adding directory
//! removal to the most dangerous code path in the project, to win something
//! purely cosmetic.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use safelink::capability::SymlinkProbe;
use safelink::classify::{LinkClass, classify};
use safelink::materialize::{MaterializeOptions, materialize};
use safelink::op::{Entry, LinkOp};

use crate::manifest::Manifest;
use crate::retention::{KEEP_BACKUPS, plan_retention, remove_backup_dir};

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

/// What the planner decided about one object found inside a declared
/// directory's destination subtree but no longer present in its source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PruneDecision {
    /// A plain file this tool materialized from a source file that is gone.
    /// Back it up, then unlink it.
    Remove,
    /// A link was found where a prune candidate would otherwise be. It is
    /// neither descended into nor removed — see [`PlannedPrune`].
    LeftInPlace(String),
}

/// One object inside a declared `copy`-directory's destination subtree that
/// the current source no longer accounts for.
///
/// Deliberately *not* a [`PlannedEntry`] with a clever `entry: None`: that
/// field already means "refused before an `Entry` could be built", and a prune
/// candidate is the opposite shape — it has a destination and, by definition,
/// no source at all. Conflating the two would make both harder to read at
/// exactly the place where reading carefully matters most.
#[derive(Debug, Clone)]
pub struct PlannedPrune {
    /// The `[targets.*]` name this belongs to.
    pub target: Option<String>,
    /// What the user sees: the path relative to the source root, the same
    /// convention [`PlannedEntry::label`] uses for a directory-copy expansion
    /// (`"modes/old-file.md"` for the declared entry `"modes"`).
    pub label: String,
    /// The absolute destination path. Always inside the declared directory's
    /// own destination subtree — see [`prune_scope`].
    pub dest: PathBuf,
    /// Where this file's content is preserved before it is unlinked. `None`
    /// only for a candidate that is not going to be removed.
    pub backup: Option<PathBuf>,
    pub decision: PruneDecision,
}

/// The retention sweep's decision for one target root's backup directory.
#[derive(Debug, Clone)]
pub struct PlannedRetention {
    pub target: Option<String>,
    /// Backup directories to remove, oldest first.
    pub removals: Vec<PathBuf>,
}

/// Something the planner could not determine, and declined to guess about.
///
/// Every such case in this module chooses the safe direction — do nothing —
/// and every one of them then says so here. A pass that quietly did nothing is
/// indistinguishable from one that had nothing to do, and Constraint 3's
/// honesty requirement covers what a run *could not* decide just as much as
/// what it decided.
#[derive(Debug, Clone)]
pub struct PlanNote {
    pub target: Option<String>,
    /// Printed verbatim, in both modes, so it must read as a complete
    /// sentence and say what the consequence is.
    pub message: String,
}

/// Everything one `apply` invocation intends to do.
#[derive(Debug, Clone)]
pub struct Plan {
    /// Old backup directories to sweep. Executed **first**, before any entry
    /// or prune — see [`execute`].
    pub retention: Vec<PlannedRetention>,
    /// What the planner could not determine. Reported in both modes; changes
    /// no exit code, since nothing was attempted and nothing failed.
    pub notes: Vec<PlanNote>,
    pub entries: Vec<PlannedEntry>,
    /// Files to remove from a declared directory's destination subtree because
    /// the source no longer has them. Always a subset of those subtrees; see
    /// [`prune_scope`] for why that is structural and not a convention.
    pub prunes: Vec<PlannedPrune>,
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
        let mut lines = Vec::with_capacity(self.entries.len() + self.prunes.len());
        let mut refused = false;

        // Same order as `execute`: the sweep first, then the entries, then the
        // prunes. A dry run that reordered the report would be describing a
        // different run than the one it claims to preview.
        for sweep in &self.retention {
            for removal in &sweep.removals {
                lines.push(with_target(
                    &sweep.target,
                    &format!("would prune backup: {}", dir_name(removal)),
                ));
            }
        }
        for note in &self.notes {
            lines.push(with_target(&note.target, &note.message));
        }

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

        for prune in &self.prunes {
            lines.push(with_target(&prune.target, &describe_prune(prune, true)));
        }

        Report {
            lines,
            refused,
            failed: false,
        }
    }
}

/// The body of one prune's report line, in either mode.
///
/// `LeftInPlace` reads identically in both: it is a statement of present fact
/// plus a refusal to act, and there is no hypothetical version of "this is a
/// link and it is staying".
fn describe_prune(prune: &PlannedPrune, hypothetical: bool) -> String {
    match &prune.decision {
        PruneDecision::Remove => {
            let verb = if hypothetical {
                "would prune"
            } else {
                "pruned"
            };
            let note = match &prune.backup {
                Some(path) => {
                    let backed = if hypothetical {
                        "would back up to"
                    } else {
                        "backed up to"
                    };
                    format!(" ({backed} {})", path.display())
                }
                None => String::new(),
            };
            format!("{verb}: {}{note}", prune.label)
        }
        PruneDecision::LeftInPlace(reason) => {
            format!("left in place: {}: {reason}", prune.label)
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
    let mut prunes = Vec::new();
    let mut notes = Vec::new();

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
            retention: Vec::new(),
            notes,
            entries,
            prunes,
            run_timestamp,
        };
    }

    for target_name in &target_names {
        let target = &manifest.targets[*target_name];
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
            &mut prunes,
            &mut notes,
        );
    }

    refuse_conflicting_destinations(&mut entries);
    // After the refusals, so a prune is never planned against a destination
    // some entry has just been told not to touch either.
    reconcile_prunes(&entries, &mut prunes);

    // Last, because whether this run will create a backup directory of its own
    // depends on the two passes above having settled which entries and prunes
    // actually survive.
    //
    // One sweep per *backup directory*, not per target: two `[targets.*]` may
    // point at the same root, and they then share one `.agrafter-backups/`.
    // Sweeping it once per target would list every expired directory twice —
    // printed twice under `--dry-run`, and on a real run removed once and then
    // failed on the second attempt, reporting an unexpected error over a
    // directory that is already correctly gone. It would also split the "is
    // this run going to write a backup here" question across two targets that
    // share the answer.
    let mut swept: HashSet<PathBuf> = HashSet::new();
    let mut retention = Vec::new();
    for name in &target_names {
        if !swept.insert(manifest.targets[*name].dest.join(BACKUP_DIR)) {
            continue;
        }
        plan_target_retention(
            name,
            &manifest.targets[*name].dest,
            &run_timestamp,
            &entries,
            &prunes,
            &mut retention,
            &mut notes,
        );
    }

    Plan {
        retention,
        notes,
        entries,
        prunes,
        run_timestamp,
    }
}

/// The retention sweep for one backup directory.
///
/// This run's own backup directory is counted toward the limit when — and only
/// when — something in this run is actually going to write a backup into it.
/// Counting it unconditionally would retire a real backup a run later than it
/// should on every no-op run.
///
/// "Going to write a backup *into it*" is asked of the planned backup paths
/// themselves rather than of which target an entry belongs to. Two targets may
/// share a root and therefore share this directory, so an entry belonging to
/// the other one still fills it — the path is the thing that decides, and the
/// target name is only a label for the report.
fn plan_target_retention(
    target_name: &str,
    target_dest: &Path,
    run_timestamp: &str,
    entries: &[PlannedEntry],
    prunes: &[PlannedPrune],
    out: &mut Vec<PlannedRetention>,
    notes: &mut Vec<PlanNote>,
) {
    let backups_dir = target_dest.join(BACKUP_DIR);
    let lands_here = |backup: &Option<PathBuf>| {
        backup
            .as_ref()
            .is_some_and(|path| path.starts_with(&backups_dir))
    };
    let will_back_up = entries
        .iter()
        .any(|e| e.decision == Decision::Write && lands_here(&e.backup))
        || prunes
            .iter()
            .any(|p| p.decision == PruneDecision::Remove && lands_here(&p.backup));

    let prospective = will_back_up.then_some(run_timestamp);

    match plan_retention(&backups_dir, KEEP_BACKUPS, prospective) {
        Ok(removals) => out.push(PlannedRetention {
            target: Some(target_name.to_string()),
            removals,
        }),
        Err(e) => notes.push(PlanNote {
            target: Some(target_name.to_string()),
            message: format!(
                "backup retention skipped: could not read {}: {e}. Old backups \
                 are accumulating there; nothing else about this run is affected.",
                backups_dir.display()
            ),
        }),
    }
}

/// Drop any prune candidate that is not exclusively this prune's to remove.
///
/// Two cases, both belt rather than braces — neither should be reachable, and
/// both are cheap to make unreachable *twice*:
///
///   - A destination some planned entry also claims. A candidate is by
///     construction a path the source no longer holds, and every entry's
///     destination comes from a path the source does hold, so the sets are
///     disjoint. Enforced anyway, because "prune deletes what a write is about
///     to create" is not a bug anyone should have to reason their way out of.
///   - A duplicate. Two `[targets.*]` pointing at the same directory plan the
///     same candidate twice; acting on it twice would make the second attempt
///     an unexplained failure over a file that is already correctly gone.
fn reconcile_prunes(entries: &[PlannedEntry], prunes: &mut Vec<PlannedPrune>) {
    let claimed: HashSet<&PathBuf> = entries
        .iter()
        .filter_map(|planned| planned.entry.as_ref().map(|entry| &entry.dest))
        .collect();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    prunes.retain(|prune| !claimed.contains(&prune.dest) && seen.insert(prune.dest.clone()));
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
fn plan_target(
    manifest: &Manifest,
    ctx: &TargetContext<'_>,
    out: &mut Vec<PlannedEntry>,
    prunes: &mut Vec<PlannedPrune>,
    notes: &mut Vec<PlanNote>,
) {
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
        plan_copy_entry(ctx, name, out, prunes, notes);
    }
}

/// Walk from `ctx.dest` down through every component of `rel` **except the
/// last**, classifying each level via `safelink::classify` before descending
/// — the write-side counterpart of [`prune_scope`]'s component-by-component
/// walk, generalized to a destination that may not exist yet.
///
/// `plan_copy_entry` and `plan_link_entry` used to build a destination with a
/// single, un-classified `Path::join` chain (`ctx.dest.join(name)`, or
/// `ctx.dest.join(name).join(rel)` for a directory-copy expansion), the exact
/// shape [`prune_scope`]'s doc comment explains is unsafe for anything with
/// more than one path component: the kernel resolves *through* an
/// intermediate link before `classify` ever sees the final path. A symlinked
/// `dest/agents` then makes a declared `copy = ["agents/modes"]` entry write —
/// and get backed up — into whatever `dest/agents` actually points at, which
/// can be anywhere. This function is what both planners now call before they
/// build an `Entry`, so no destination is ever resolved without it.
///
/// **Why this is a separate function rather than a shared helper with
/// `prune_scope`, despite the identical-sounding walk:** the two have
/// different termination rules, and folding them into one function would mean
/// threading a flag through the one place correctness matters most.
/// `prune_scope` only ever deals with a destination that must already fully
/// exist (there is nothing to enumerate otherwise) and refuses on the
/// *last* component being a link too, since pruning walks *into* that
/// component. This function's last component is the thing being written,
/// whose own link-or-not status `decide` already handles correctly (a
/// wrong-kind link there is fine to replace) — so it deliberately stops
/// classifying one level early, and treats "nothing here yet" as success
/// rather than `prune_scope`'s "nothing to reconcile". A shared function
/// would need a parameter meaning "but not the last one, and NotFound means
/// something different here", which is worse for the next reader than two
/// short functions that each say exactly what they check.
///
/// One check `prune_scope` has that this does not: it never special-cases a
/// component named [`BACKUP_DIR`]. Writing *through* a symlinked
/// `.agrafter-backups` is exactly the same scope escape as any other
/// symlinked intermediate and is refused here like any other link; writing a
/// declared entry's *content* into a real, non-link `.agrafter-backups`
/// directory is a separate, pre-existing question this task was not asked to
/// change (see `the_backup_directory_is_never_itself_a_prune_candidate`,
/// which already covers what happens to prune in that case and does not
/// assert anything about the write side).
///
/// Returns the fully joined destination once every intermediate component is
/// confirmed to be a real, non-link directory, or is confirmed not to exist
/// yet — nothing can exist beneath a path component that is not there, so the
/// remainder of `rel` is a clean write into directories `materialize` will
/// create. Returns `Err` with a user-facing reason when an intermediate
/// component is a link, is not a plain path component (`..`, an absolute
/// root, a Windows prefix), or cannot be inspected at all.
///
/// The last case also covers an intermediate component that is a real
/// *file*, not a directory (`classify` succeeds on it — it is not a link —
/// but the next level's `classify` fails to inspect anything beneath it).
/// That is a plan-time refusal now, where before this fix it surfaced as a
/// runtime `failed:` line out of `materialize`'s own missing/blocked-parent
/// handling. Refusing it here is the more honest answer — the write cannot
/// possibly succeed, and the planner already refuses equally unwritable
/// destinations elsewhere (`decide_over_real_data`) — but it is a real,
/// user-visible change for that one non-symlink shape: what used to fail
/// during `execute` (exit 1) now stops at `plan` (exit 4).
fn resolve_scoped_dest(
    ctx: &TargetContext<'_>,
    declared: &str,
    rel: &Path,
) -> Result<PathBuf, String> {
    let mut names = Vec::new();
    for component in rel.components() {
        match component {
            Component::Normal(name) => names.push(name),
            // Names the same directory; neither descends nor needs checking.
            Component::CurDir => continue,
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "the declared entry {declared:?} contains a path \
                     component that is not a plain name, so where it lands \
                     cannot be established without resolving the filesystem; \
                     nothing was written. Spell it as a plain relative path."
                ));
            }
        }
    }

    // Split off the last component: its own class is `decide`'s question, not
    // this walk's. `split_last` returning `None` is unreachable in practice —
    // every caller passes at least one real component, and
    // `manifest::validate_entry` already refuses an entry that normalizes to
    // none — but treated as "nothing to check" rather than assumed away, so a
    // future caller that got this wrong fails safe instead of panicking.
    let Some((_last, intermediate)) = names.split_last() else {
        return Ok(ctx.dest.join(rel));
    };

    let mut current = ctx.dest.to_path_buf();
    for name in intermediate {
        current.push(name);

        // classify() before any metadata question, exactly as `prune_scope`
        // does and for the same reason: `is_dir()` and `is_symlink()` can
        // both be true at once, so `is_dir()` alone never answers "is it safe
        // to write through here" (Spike S1).
        match classify(&current) {
            Ok(LinkClass::NotALink) => {}
            Ok(LinkClass::DirLink | LinkClass::FileLink) => {
                return Err(format!(
                    "{} is a link, so writing the declared entry {declared:?} \
                     would mean resolving through it and possibly outside {}; \
                     nothing was written.",
                    current.display(),
                    ctx.dest.display()
                ));
            }
            // Nothing materialized at this level yet, so nothing beneath it
            // can exist either: the rest of `rel` is a clean write into
            // directories `materialize` will create.
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(ctx.dest.join(rel)),
            Err(e) => {
                return Err(format!(
                    "could not inspect {} while resolving the declared entry \
                     {declared:?}: {e}; nothing was written.",
                    current.display()
                ));
            }
        }
    }

    Ok(ctx.dest.join(rel))
}

/// A `link` entry maps 1:1 and is never expanded: a directory source becomes
/// one `DirectoryLink`, a file source one `FileSymlink` (which the engine may
/// downgrade to a copy at execution time).
///
/// **A `link` entry is never pruned, and this is a prohibition rather than an
/// omission.** Note that this function takes no `prunes` argument at all: a
/// `link` entry has no way to contribute a prune candidate, because the only
/// code that produces one is reached exclusively from `plan_copy_entry`'s
/// directory branch.
///
/// The reason is that a `DirectoryLink`'s destination *is* its source. Reading
/// through it sees the source's live contents; there is no independently
/// materialized copy to reconcile against anything, so there is nothing that
/// could meaningfully be stale. An implementer who "helpfully" walked a link
/// entry's destination looking for files "not declared anywhere" and removed
/// them would be deleting *through the link, into the source of truth* — the
/// exact failure Global Constraint 3 (inherited from `safelink`: never delete
/// through a link) exists to prevent. Do not add pruning here.
fn plan_link_entry(ctx: &TargetContext<'_>, name: &str) -> PlannedEntry {
    let source = ctx.source_root.join(name);
    if !source.exists() {
        return refusal(ctx.name, name, missing_source_reason(&source));
    }
    let dest = match resolve_scoped_dest(ctx, name, Path::new(name)) {
        Ok(dest) => dest,
        Err(reason) => return refusal(ctx.name, name, reason),
    };
    let op = if source.is_dir() {
        LinkOp::DirectoryLink
    } else {
        LinkOp::FileSymlink
    };
    let entry = Entry { source, dest, op };
    resolve(ctx, name, name, entry)
}

/// A `copy` entry naming a file is one `FileCopy`; a `copy` entry naming a
/// directory expands to one `FileCopy` per file found beneath it, each
/// mirroring its path relative to the declared directory.
///
/// A declared directory holding no files expands to no entries, and therefore
/// reports nothing: there is no work to report, and inventing a fifth report
/// verb for "nothing was in there" is not this task's to define.
/// A `copy` entry naming a **file** contributes no prune candidates: a single
/// file either exists or it does not, and there is no subtree to reconcile.
/// Only the directory branch below reaches [`plan_prunes`].
fn plan_copy_entry(
    ctx: &TargetContext<'_>,
    name: &str,
    out: &mut Vec<PlannedEntry>,
    prunes: &mut Vec<PlannedPrune>,
    notes: &mut Vec<PlanNote>,
) {
    let source = ctx.source_root.join(name);
    if !source.exists() {
        out.push(refusal(ctx.name, name, missing_source_reason(&source)));
        return;
    }

    if !source.is_dir() {
        let dest = match resolve_scoped_dest(ctx, name, Path::new(name)) {
            Ok(dest) => dest,
            Err(reason) => {
                out.push(refusal(ctx.name, name, reason));
                return;
            }
        };
        let entry = Entry {
            source,
            dest,
            op: LinkOp::FileCopy,
        };
        out.push(resolve(ctx, name, name, entry));
        return;
    }

    let mut relative = Vec::new();
    if let Err(e) = collect_files(&source, Path::new(""), &mut relative) {
        // A *partial* source listing is the one input that could turn prune
        // into a wipe, so this arm returns before `plan_prunes` is reached
        // rather than pruning against whatever was read before the error.
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

    for rel in &relative {
        let full_rel = Path::new(name).join(rel);
        let label = full_rel.to_string_lossy().into_owned();
        // `declared` is `name` (the directory the manifest actually declares),
        // not `label` (this one expanded file) — the same distinction
        // `PlannedEntry::declared`'s own doc comment draws, and the reason a
        // refusal reason must say "edit *this*", which is what the user can
        // actually act on.
        let dest = match resolve_scoped_dest(ctx, name, &full_rel) {
            Ok(dest) => dest,
            Err(reason) => {
                out.push(PlannedEntry {
                    target: Some(ctx.name.to_string()),
                    label: label.clone(),
                    declared: name.to_string(),
                    entry: None,
                    backup: None,
                    decision: Decision::Refused(reason),
                });
                continue;
            }
        };
        let entry = Entry {
            source: source.join(rel),
            dest,
            op: LinkOp::FileCopy,
        };
        out.push(resolve(ctx, name, &label, entry));
    }

    plan_prunes(ctx, name, &source, prunes, notes);
}

// ---------------------------------------------------------------------------
// Prune. The scope boundary, and then the walk inside it.
// ---------------------------------------------------------------------------

/// The **only** directory prune is ever allowed to enumerate for one declared
/// entry, or `None` when there is none.
///
/// This function is where the single most important safety property of this
/// whole tool lives, so it is worth stating what that property is and why the
/// structure — not a convention — guarantees it.
///
/// The plan's own wording for prune ("destination entries absent from the
/// manifest and from the source are removed"), read literally against a real
/// target root like an agent's config directory, would authorize enumerating
/// that entire directory and deleting everything the manifest does not
/// mention: credentials, settings, session transcripts — the real data this
/// tool's whole declare-don't-discover design exists to leave alone. That
/// reading is wrong. The correct scope is far narrower: prune reconciles a
/// declared `copy` directory's own destination subtree against that same
/// subtree's current source contents, and nothing else.
///
/// What makes that structural rather than aspirational:
///
///   1. This is the only function that turns a target root into a walkable
///      path, and it builds that path **one component at a time from
///      `ctx.dest`**, checking every level on the way down (see below).
///   2. It is called from exactly one place: [`plan_prunes`], which is called
///      from exactly one place: the directory branch of [`plan_copy_entry`].
///      `link` entries and file `copy` entries therefore have no route here at
///      all (Rulings A and B).
///   3. The walk below never receives `ctx.dest`. It is handed the scope root
///      and a relative prefix, so there is no expression anywhere in the prune
///      path that could name a sibling of the declared directory.
///
/// **Why the descent is component-by-component, and not one `join` plus one
/// `classify`.** That is what this function used to do, and it was wrong for
/// any declared entry naming more than one segment. `ctx.dest.join("agents/modes")`
/// is resolved by the kernel *through* `dest/agents` before `classify` ever
/// sees the end of the path, so if `dest/agents` is a link, `classify` happily
/// reports `NotALink` — about an object that may live nowhere near the target
/// root. Prune would then enumerate and delete inside a completely unrelated
/// directory. Classifying the joined result answers a question about the
/// destination of the path, not about the path; only walking it level by level
/// answers "does getting here pass through anything I must not cross".
///
/// So every level from `ctx.dest` down is classified in turn, and any of the
/// following yields no scope, and therefore no prune candidates at all:
///
///   - **A link at any level, intermediate or final.** Including the stale
///     `DirLink` left at `dest/modes` by an earlier `link = ["modes"]`
///     declaration that has since become `copy = ["modes"]` — Ruling A's
///     failure mode arriving by a different door. Reported, not silent.
///   - **Ruling C.** A component named as the backup directory. Backups live
///     at `target.dest/.agrafter-backups`, not nested inside a declared
///     subdirectory, so a collision needs a manifest pathological enough to
///     declare that name itself — which nothing stops it from doing.
///     Hard-coded rather than argued to be impossible.
///   - **Any component that is not a plain name** — `..`, an absolute root, a
///     Windows prefix. `manifest::validate_entry` checks only an entry's *net*
///     depth, so `"a/b/../../c"` is accepted today; refusing the component
///     outright here means no `..` ever takes part in building a path this code
///     is about to delete inside, whatever the manifest layer does or does not
///     catch.
///   - **No plain components at all.** A belt against ever returning
///     `ctx.dest` itself, which is the one path this whole function exists to
///     keep prune away from.
///   - **A level that cannot be inspected**, or a final path that is not a real
///     directory.
fn prune_scope(ctx: &TargetContext<'_>, declared_dir: &str) -> Scope {
    let mut current = ctx.dest.to_path_buf();
    let mut depth = 0usize;

    for component in Path::new(declared_dir).components() {
        let name = match component {
            Component::Normal(name) => name,
            // `Components` keeps a leading `.`; it names the same directory,
            // so it neither descends nor needs checking.
            Component::CurDir => continue,
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Scope::Blocked(format!(
                    "the declared entry {declared_dir:?} contains a path \
                     component that is not a plain name, so where it lands \
                     cannot be established without resolving the filesystem; \
                     nothing was removed. Spell it as a plain relative path."
                ));
            }
        };
        if name == BACKUP_DIR {
            return Scope::Nothing;
        }
        current.push(name);

        // classify() before any metadata question, at *every* level: `is_dir()`
        // and `is_symlink()` can both be true at once, so `is_dir()` alone is
        // not an answer to "is it safe to descend here" (Spike S1).
        match classify(&current) {
            Ok(LinkClass::NotALink) => {}
            Ok(LinkClass::DirLink | LinkClass::FileLink) => {
                return Scope::Blocked(format!(
                    "{} is a link, so reaching the declared entry \
                     {declared_dir:?} would mean resolving through it and \
                     possibly outside {}; nothing was removed.",
                    current.display(),
                    ctx.dest.display()
                ));
            }
            // Nothing materialized here yet. Not a problem, and not worth a
            // line: there is simply no destination subtree to reconcile.
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Scope::Nothing,
            Err(e) => {
                return Scope::Blocked(format!(
                    "could not inspect {} while locating the declared entry \
                     {declared_dir:?}: {e}; nothing was removed.",
                    current.display()
                ));
            }
        }
        depth += 1;
    }

    if depth == 0 {
        // `""`, `"."` — `manifest::validate_entry` already refuses these, and
        // this is the backstop, because the failure it prevents is prune
        // enumerating the target root itself.
        return Scope::Nothing;
    }
    if !std::fs::symlink_metadata(&current).is_ok_and(|m| m.is_dir()) {
        return Scope::Nothing;
    }
    Scope::Walk(current)
}

/// What [`prune_scope`] concluded about one declared entry.
enum Scope {
    /// Safe to enumerate: a real directory reached without crossing a link.
    Walk(PathBuf),
    /// Nothing to reconcile, and nothing worth a report line.
    Nothing,
    /// Refused. The string is the user-facing explanation, printed verbatim.
    Blocked(String),
}

/// What the destination-subtree walk found at one relative path.
enum Found {
    /// A plain file. A prune candidate if the source no longer has it.
    File,
    /// A link. Never descended into, never removed (Ruling D).
    Link,
}

/// Compare one declared directory's destination subtree against its current
/// source subtree, and plan the difference.
///
/// **Reached only from [`plan_copy_entry`]'s directory branch, after
/// `collect_files` on the source has already succeeded.** That ordering is the
/// safety argument, and the asymmetry behind it is worth being explicit about:
///
///   - Under-reading the **destination** finds *fewer* candidates than exist.
///     A file that should have been pruned survives another day. Harmless.
///   - Under-reading the **source** makes correctly materialized files look
///     undeclared, and prune would delete them. Catastrophic — and reachable
///     without any adversary at all: the removable or network volume the
///     missing-source message already anticipates, unmounted mid-run.
///
/// So only the source side gets a hard stop. An absent, unreadable, or
/// non-directory source yields *zero* candidates and never a partial set. The
/// legitimate twin is deliberately not caught by that: a source directory that
/// is **present but empty** is a real state a user can declare, and everything
/// the destination still holds under it genuinely is stale.
fn plan_prunes(
    ctx: &TargetContext<'_>,
    declared_dir: &str,
    source_dir: &Path,
    out: &mut Vec<PlannedPrune>,
    notes: &mut Vec<PlanNote>,
) {
    // Re-stated locally rather than inherited from the caller, so this
    // function is safe to read (and to call) on its own terms.
    if !source_dir.is_dir() {
        return;
    }
    let scope = match prune_scope(ctx, declared_dir) {
        Scope::Walk(scope) => scope,
        Scope::Nothing => return,
        Scope::Blocked(reason) => {
            notes.push(PlanNote {
                target: Some(ctx.name.to_string()),
                message: format!("prune skipped for {declared_dir:?}: {reason}"),
            });
            return;
        }
    };

    let mut found = Vec::new();
    if let Err(e) = collect_dest_subtree(&scope, Path::new(""), 0, &mut found) {
        // A partial destination listing only under-enumerates, so acting on it
        // would be safe — but a run whose report depends on how far a failing
        // walk happened to get is not one anybody can diff. Nothing, instead —
        // and said out loud, because a prune pass that silently did nothing
        // reads exactly like one that had nothing to do.
        notes.push(PlanNote {
            target: Some(ctx.name.to_string()),
            message: format!(
                "prune skipped for {declared_dir:?}: could not read {}: {e}. \
                 Files the source no longer has may still be there; nothing \
                 was removed.",
                scope.display()
            ),
        });
        return;
    }

    for (rel, kind) in found {
        // The source's *current* contents decide. A single `symlink_metadata`
        // rather than a lookup in the file list `plan_copy_entry` built,
        // because that list holds only files: a directory, or a link, that the
        // source still has at this relative path is not stale either, and
        // asking the filesystem directly is the answer that cannot drift from
        // how the list was assembled.
        //
        // The error *kind* is load-bearing, and reading it as a bare `is_ok()`
        // was a bug: that turns "I could not find out" into "it is gone", and
        // from there into a deletion. Only two kinds actually mean absent.
        // Everything else is undecidable, and an undecidable source is never a
        // reason to remove something — the same asymmetry the scope root is
        // already governed by, applied per path.
        match std::fs::symlink_metadata(source_dir.join(&rel)) {
            Ok(_) => continue,
            Err(e) if is_genuinely_absent(&e) => {}
            Err(e) => {
                notes.push(PlanNote {
                    target: Some(ctx.name.to_string()),
                    message: format!(
                        "prune skipped for {}: could not tell whether the \
                         source still has it: {e}. It was left in place.",
                        Path::new(declared_dir).join(&rel).display()
                    ),
                });
                continue;
            }
        }

        let dest = scope.join(&rel);
        // Belt over the braces above. `rel` is built only from `file_name()`
        // components of entries read out of `scope`, so it cannot contain `..`
        // and this cannot fire — which is exactly why it is cheap to check.
        if !dest.starts_with(&scope) {
            continue;
        }

        let label = Path::new(declared_dir)
            .join(&rel)
            .to_string_lossy()
            .into_owned();
        let decision = match kind {
            Found::Link => PruneDecision::LeftInPlace(
                "a link, not a file this tool materialized; refusing to remove \
                 it or to delete anything through it"
                    .to_string(),
            ),
            Found::File => PruneDecision::Remove,
        };
        let backup = match decision {
            PruneDecision::Remove => backup_path(&dest, ctx.dest, ctx.run_timestamp),
            PruneDecision::LeftInPlace(_) => None,
        };
        out.push(PlannedPrune {
            target: Some(ctx.name.to_string()),
            label,
            dest,
            backup,
            decision,
        });
    }
}

/// Every plain file and every link beneath `dir`, as paths relative to it, in
/// sorted order.
///
/// Distinct from [`collect_files`], which reads the *source* and cares only
/// about what to copy. This one reads a **destination** and is about to decide
/// what to delete, so it reports links separately instead of folding them in
/// with files, and it never descends into one.
///
/// Every object is put to `classify` before anything is decided about it
/// (Ruling D). A bare `is_dir()` check to choose "recurse here" is wrong:
/// `is_dir()` and `is_symlink()` can both be true simultaneously on a junction,
/// so the metadata question is only ever asked *after* `classify` has said the
/// object is not a link.
fn collect_dest_subtree(
    dir: &Path,
    prefix: &Path,
    depth: usize,
    out: &mut Vec<(PathBuf, Found)>,
) -> io::Result<()> {
    const MAX_DEPTH: usize = 64;
    if depth > MAX_DEPTH {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "refusing to descend past {MAX_DEPTH} levels under {}",
                dir.display()
            ),
        ));
    }

    let mut names: Vec<PathBuf> = std::fs::read_dir(dir)?
        .map(|e| e.map(|e| e.file_name().into()))
        .collect::<io::Result<_>>()?;
    names.sort();

    for name in names {
        // Ruling C, again and unconditionally: at every level, not just the
        // top one, and regardless of whether it should be reachable here.
        if name.as_os_str() == BACKUP_DIR {
            continue;
        }
        let path = dir.join(&name);
        let rel = prefix.join(&name);
        match classify(&path)? {
            LinkClass::DirLink | LinkClass::FileLink => out.push((rel, Found::Link)),
            LinkClass::NotALink => {
                let meta = std::fs::symlink_metadata(&path)?;
                if meta.is_dir() {
                    collect_dest_subtree(&path, &rel, depth + 1, out)?;
                } else if meta.is_file() {
                    out.push((rel, Found::File));
                }
                // Anything else (a socket, a device node) is not something
                // this tool put here and not something it removes.
            }
        }
    }
    Ok(())
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

/// Whether a failure to stat a source path means it is genuinely not there.
///
/// Two kinds do, and only two:
///
///   - `NotFound` — the obvious one.
///   - `NotADirectory` — a path component that used to be a directory is now a
///     plain file, so everything that was under it is gone just as surely.
///     Treating this as undecidable would leave a whole stale subtree behind
///     forever on a perfectly ordinary source edit.
///
/// Every other kind — permission denied, an I/O error, a link that cannot be
/// traversed — means the question was not answered, and an unanswered question
/// is never grounds for a deletion.
fn is_genuinely_absent(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
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
    let mut lines = Vec::with_capacity(plan.entries.len() + plan.prunes.len());
    let mut refused = false;
    let mut failed = false;

    // Ruling F: the retention sweep goes **first**, before any entry or prune
    // executes.
    //
    // The order is the whole reason it is safe. Backup directories are created
    // lazily, by the first `copy_atomic` that needs one — nothing pre-creates
    // `.agrafter-backups/<run_timestamp>/` at the top of a run — so sweeping
    // before anything writes means the sweep cannot possibly delete a
    // directory this run is in the middle of filling. Sweeping afterwards
    // would have to special-case this run's own directory to avoid exactly
    // that, and a special case is a thing that can be got wrong; an ordering
    // is not. `plan_target_retention` still counts this run's prospective
    // directory toward the limit, so "keep 10" means 10 including it.
    for sweep in &plan.retention {
        for removal in &sweep.removals {
            let body = match remove_backup_dir(removal) {
                Ok(()) => format!("pruned backup: {}", dir_name(removal)),
                Err(e) => {
                    failed = true;
                    format!(
                        "failed: could not remove the backup {}: {e}",
                        removal.display()
                    )
                }
            };
            lines.push(with_target(&sweep.target, &body));
        }
    }
    for note in &plan.notes {
        lines.push(with_target(&note.target, &note.message));
    }

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

    // Prunes last: the writes above have already put the current source's
    // contents in place, so nothing here can remove something a write was
    // about to create even if the plan somehow contained both.
    for prune in &plan.prunes {
        let body = match prune_one(prune) {
            Ok(Some(reason)) => format!("left in place: {}: {reason}", prune.label),
            Ok(None) => describe_prune(prune, false),
            Err(e) => {
                failed = true;
                format!("failed: could not prune {}: {e}", prune.label)
            }
        };
        lines.push(with_target(&prune.target, &body));
    }

    Report {
        lines,
        refused,
        failed,
    }
}

/// Back up and unlink one pruned file, or explain why it was left alone.
///
/// `Ok(None)` means it was removed; `Ok(Some(reason))` means it was not, and
/// why. Neither is a failure: declining to delete something is the safe
/// outcome, and it does not change the run's exit code. Exit 4 is reserved for
/// *declared* work this tool refused to do — a stray link sitting inside a
/// materialized directory is not declared work, and making it fail the run
/// would turn one editor backup file into a permanently red CI gate.
///
/// The plan-time decision is re-checked here rather than trusted, the same
/// caution the write path already applies: `classify` is asked again
/// immediately before anything is removed, so an object that has become a link
/// (or vanished, or turned into a directory) since the plan was computed is
/// left alone instead of deleted on the strength of a stale snapshot.
///
/// The removal itself is `std::fs::remove_file` and nothing else. It is
/// narrow by construction — it cannot remove a directory, and it cannot
/// recurse — and it runs only after `classify` has said "not a link" and the
/// metadata has said "a plain file". Removing a *link* is never this code's
/// job; that stays exclusively `safelink::remove::remove_link`'s
/// (Constraint 5), and this path never calls it, because it never removes a
/// link at all.
fn prune_one(prune: &PlannedPrune) -> io::Result<Option<String>> {
    if let PruneDecision::LeftInPlace(reason) = &prune.decision {
        return Ok(Some(reason.clone()));
    }

    match classify(&prune.dest) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(Some("it is no longer there".to_string()));
        }
        Err(e) => return Err(e),
        Ok(LinkClass::DirLink | LinkClass::FileLink) => {
            return Ok(Some(
                "it became a link after this run was planned; refusing to \
                 remove it or to delete anything through it"
                    .to_string(),
            ));
        }
        Ok(LinkClass::NotALink) => {}
    }

    if !std::fs::symlink_metadata(&prune.dest)?.is_file() {
        return Ok(Some(
            "it is no longer a plain file; refusing to remove it".to_string(),
        ));
    }

    // Constraint 4: nothing is deleted without its content being preserved
    // first. A `Remove` always carries a backup path — `backup_path` produced
    // one for a plain file inside the target root — so its absence means the
    // plan and the disk disagree, and the safe reading of a disagreement is
    // "do not delete".
    let Some(backup) = &prune.backup else {
        return Ok(Some(
            "no backup location could be computed for it, and nothing is \
             removed without one"
                .to_string(),
        ));
    };
    safelink::copy::copy_atomic(&prune.dest, backup).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!(
                "could not back up {} to {}: {e} -- refusing to remove it \
                 without a backup",
                prune.dest.display(),
                backup.display()
            ),
        )
    })?;

    std::fs::remove_file(&prune.dest)?;
    Ok(None)
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
    with_target(&planned.target, body)
}

fn with_target(target: &Option<String>, body: &str) -> String {
    match target {
        Some(target) => format!("{target}: {body}"),
        None => body.to_string(),
    }
}

/// A backup directory's own name, for reporting. The full path is long,
/// repetitive across a run's lines, and adds nothing the target prefix and the
/// timestamp do not already say.
fn dir_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
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
