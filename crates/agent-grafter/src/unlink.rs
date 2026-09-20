//! `unlink`: decide what of this tool's own output is safe to remove, then
//! remove exactly that.
//!
//! Split into [`plan`] and [`execute`] for the same reason [`crate::apply`]
//! is, and the split is load-bearing in the same way: `--dry-run` calls
//! [`Plan::describe`] and stops, a real run calls [`execute`], and both start
//! from one decision path. Constraint 3's "dry-run performs the full decision
//! logic" is therefore structural here too, not a discipline the next editor
//! has to remember.
//!
//! **This command never writes.** Its entire removal surface is two calls:
//! `safelink::remove::remove_link` for anything that classifies as a link
//! (Constraint 5 — link removal is never reimplemented here), and
//! `std::fs::remove_file` for a plain file whose bytes are identical to the
//! declared source. It creates no backup directory, never triggers the
//! retention sweep, and has no path that removes a directory, recursively or
//! otherwise. That one-sentence claim is what the refusals below buy, and it
//! is why they are refusals rather than "back it up first and delete it
//! anyway".
//!
//! **What makes a destination removable.** Two cases, and nothing else:
//!
//!   - **A link.** Removing a link destroys nothing: the thing it points at
//!     is untouched, by construction, because `remove_link` refuses to
//!     traverse. This holds regardless of where the link points, which is why
//!     no target comparison is made before removing one (`doctor`'s check 4
//!     is where a link's *target* is a finding; here it cannot change the
//!     answer, since there is no outcome in which real data is lost).
//!   - **A plain file byte-identical to the declared source.** Then the
//!     content still exists at the source and removing the destination copy
//!     loses nothing. This is the same criterion `apply::is_same` already
//!     uses to decide a `FileCopy` needs no rewrite, reused rather than
//!     restated — and it is the only thing standing between `unlink` and a
//!     user's edited file, since a materialized `copy` entry is *real files*
//!     with no link indirection to protect them.
//!
//! Everything else is refused, out loud, with exit code 4: a real directory
//! where a link was declared, a file whose bytes differ from the source, a
//! destination that cannot be inspected, a declared path that would have to
//! be reached through a link. "Refuse rather than guess" is the whole rule —
//! there is no `--force`, because a destructive opt-in is not a default this
//! task was asked to invent (Constraint 4).
//!
//! **Backups are left exactly where `apply` put them, and never restored.**
//! Not a preference — restoring is not implementable correctly. `apply`'s
//! backups are per-run *and sparse*: each `.agrafter-backups/<timestamp>/`
//! holds only the files that one run actually replaced. No generation is a
//! snapshot of "the state before this tool was installed", and no
//! combination of them reconstructs one, so there is no most-recent backup
//! to restore that would mean what a user asking for a restore expects.
//! Deleting them instead would be worse: they are the only surviving copy of
//! content `apply` overwrote. So they stay, and [`plan`] names the directory
//! in the report so they are not *silently* orphaned.
//!
//! *One deliberate limitation, mirroring prune's.* Removing a declared `copy`
//! directory removes its files and leaves the directories that held them,
//! empty. That is the same call `apply`'s prune already made, for the same
//! reason: directory removal on the most dangerous code path in the project,
//! to win something purely cosmetic, is a bad trade. It is also what keeps
//! the two-call removal surface above true.

use std::io;
use std::path::{Component, Path, PathBuf};

use safelink::classify::{LinkClass, classify};

use crate::apply::{BACKUP_DIR, Report, collect_files, has_identical_bytes, with_target};
use crate::manifest::Manifest;

/// What the planner decided about one declared destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Nothing is there. Already removed, or never materialized.
    Absent,
    /// A link. Removed via `safelink::remove::remove_link`, which unlinks the
    /// link object itself and never traverses it.
    RemoveLink,
    /// A plain file byte-identical to the declared source. Removed with
    /// `std::fs::remove_file`.
    RemoveCopy,
    /// Refused for a safety reason. Printed verbatim, so it must say what to
    /// do about it.
    Refused(String),
}

/// One destination `unlink` has reached a decision about.
#[derive(Debug, Clone)]
pub struct PlannedRemoval {
    /// The `[targets.*]` name this belongs to, or `None` when the manifest
    /// declares no targets at all.
    pub target: Option<String>,
    /// What the user sees: the declared entry name, or — for a directory
    /// `copy` expansion — the individual file's path relative to the source
    /// root, the same convention `apply` uses.
    pub label: String,
    /// The manifest entry this came from, which for a directory expansion is
    /// the declared *directory*, not the individual file.
    pub declared: String,
    /// The resolved destination. Always a `target.dest`-rooted join whose
    /// every intermediate component was classified as a real, non-link
    /// directory (or confirmed absent) — see [`resolve_removal_dest`].
    pub dest: PathBuf,
    /// The declared source this destination is compared against, when there
    /// is one. `None` when the source no longer exists, which is survivable
    /// for a link (removal destroys nothing either way) and fatal for a copy
    /// (there is nothing left to compare bytes against).
    pub source: Option<PathBuf>,
    pub decision: Decision,
}

/// Something stated for the record rather than decided. Currently exactly
/// one kind: where a target's backups were left.
#[derive(Debug, Clone)]
pub struct Note {
    pub target: Option<String>,
    /// Printed verbatim, in both modes.
    pub message: String,
}

/// Everything one `unlink` invocation intends to do.
#[derive(Debug, Clone)]
pub struct Plan {
    pub removals: Vec<PlannedRemoval>,
    /// Printed **after** the removals, unlike `apply::Plan::notes`, which are
    /// printed first. The difference is what they are: `apply`'s notes are
    /// things the planner could not determine, which belong before the work
    /// they qualify; these are a closing statement about what was
    /// deliberately left behind, which belongs after it. Both modes print
    /// them in the same position, which is the part that matters.
    pub notes: Vec<Note>,
}

impl Plan {
    /// Describe the plan without acting on it: what `--dry-run` prints.
    ///
    /// Only the mutating verbs are made hypothetical. `not present` is a
    /// statement of present fact and reads identically in both modes.
    pub fn describe(&self) -> Report {
        let mut lines = Vec::with_capacity(self.removals.len() + self.notes.len());
        let mut refused = false;

        for removal in &self.removals {
            let body = match &removal.decision {
                Decision::Absent => format!("not present: {}", removal.label),
                Decision::RemoveLink => format!("would remove link: {}", removal.label),
                Decision::RemoveCopy => format!("would remove: {}", removal.label),
                Decision::Refused(reason) => {
                    refused = true;
                    format!("would refuse: {}: {reason}", removal.label)
                }
            };
            lines.push(with_target(&removal.target, &body));
        }
        for note in &self.notes {
            lines.push(with_target(&note.target, &note.message));
        }

        Report {
            lines,
            refused,
            failed: false,
        }
    }
}

/// Compute what `unlink` would remove for `manifest`, whose `source` is
/// resolved relative to `manifest_dir` exactly as [`crate::apply::plan`]
/// resolves it.
///
/// Reads the filesystem; writes nothing, removes nothing.
///
/// No `SymlinkProbe` is taken, and that is not an omission: a probe answers
/// "can this filesystem hold a symlink", which only ever changes what should
/// be *created*. What is already on disk is read by `classify`, which needs
/// no capability question — and the one place the probe's answer left a trace
/// (a declared `FileSymlink` that `materialize` downgraded to a plain copy)
/// is handled here by the byte comparison, which recognizes that copy without
/// having to know why it is a copy.
pub fn plan(manifest: &Manifest, manifest_dir: &Path) -> Plan {
    let source_root = crate::apply::resolved_source(manifest, manifest_dir);
    let mut removals = Vec::new();
    let mut notes = Vec::new();

    let mut target_names: Vec<&String> = manifest.targets.keys().collect();
    target_names.sort();

    if target_names.is_empty() {
        // Declared entries with nowhere they could have been materialized.
        // Reporting success here is precisely the silent-success shape this
        // whole command was rewritten to stop producing: it would tell a
        // script that an uninstall it never performed had succeeded.
        for label in manifest.link.iter().chain(manifest.copy.iter()) {
            removals.push(PlannedRemoval {
                target: None,
                label: label.clone(),
                declared: label.clone(),
                dest: PathBuf::new(),
                source: None,
                decision: Decision::Refused(
                    "the manifest declares no [targets.*], so there is no \
                     destination to remove anything from; nothing was removed"
                        .to_string(),
                ),
            });
        }
        return Plan { removals, notes };
    }

    for target_name in &target_names {
        let dest_root = &manifest.targets[*target_name].dest;
        plan_target(
            manifest,
            &source_root,
            target_name,
            dest_root,
            &mut removals,
            &mut notes,
        );
    }

    Plan { removals, notes }
}

/// Plan every declared entry for one target: `link` entries first, then
/// `copy`, matching the order the rest of the CLI reports them in.
fn plan_target(
    manifest: &Manifest,
    source_root: &Path,
    target_name: &str,
    dest_root: &Path,
    out: &mut Vec<PlannedRemoval>,
    notes: &mut Vec<Note>,
) {
    // A target root that is not there cannot be reported as "nothing to
    // remove": the overwhelmingly likelier cause is a manifest pointing
    // somewhere wrong, and answering exit 0 to that is the same silent
    // success this command is being fixed for. Refused, with the reason.
    if !dest_root.is_dir() {
        let reason = format!(
            "the target root {} does not exist (or is not a directory), so \
             nothing can be removed from it; nothing was removed. Check the \
             manifest's `dest`, or mount the volume it lives on, and re-run.",
            dest_root.display()
        );
        for label in manifest.link.iter().chain(manifest.copy.iter()) {
            out.push(refusal(target_name, label, reason.clone()));
        }
        return;
    }

    for name in &manifest.link {
        out.push(plan_link_entry(source_root, target_name, dest_root, name));
    }
    for name in &manifest.copy {
        plan_copy_entry(source_root, target_name, dest_root, name, out);
    }

    note_backups(target_name, dest_root, notes);
}

/// A `link` entry maps 1:1 and is never expanded, exactly as on the write
/// side.
///
/// **A missing source is not fatal here, and that is the deliberate
/// difference from `apply`.** `apply` refuses a `link` entry whose source is
/// gone because it has nothing to link to. `unlink` still has something to
/// do: if a link sits at the destination, removing it destroys nothing
/// whether or not the source survives, and a user who has already deleted
/// their canonical tree is exactly the user who most needs the leftover links
/// cleaned up. A *real file* at that destination is a different matter — the
/// only thing that could make it removable is a byte comparison against a
/// source that is no longer there — so that one is refused below.
fn plan_link_entry(
    source_root: &Path,
    target_name: &str,
    dest_root: &Path,
    name: &str,
) -> PlannedRemoval {
    let source = source_root.join(name);
    let source = source.exists().then_some(source);
    let dest = match resolve_removal_dest(dest_root, name, Path::new(name)) {
        Ok(dest) => dest,
        Err(reason) => return refusal(target_name, name, reason),
    };
    let decision = decide(&dest, source.as_deref());
    PlannedRemoval {
        target: Some(target_name.to_string()),
        label: name.to_string(),
        declared: name.to_string(),
        dest,
        source,
        decision,
    }
}

/// A `copy` entry naming a file is one destination; a `copy` entry naming a
/// directory expands to one destination per file the *source* still holds,
/// mirroring `apply`'s expansion so the two agree on which paths this tool
/// owns.
///
/// **Only the source's current contents are enumerated — never the
/// destination's.** That is the scope boundary, and it is why an undeclared
/// file a user dropped inside a materialized directory is not merely spared
/// but never even named: there is no expression in this function that turns a
/// destination directory into a list. `apply`'s prune, which does walk a
/// destination subtree, needs `prune_scope` to bound it; `unlink` needs no
/// such machinery because it never walks one.
///
/// A missing source is refused outright, unlike the `link` case above: there
/// is neither a list to expand nor bytes to compare, and a removal this
/// function cannot justify is a removal it does not make.
fn plan_copy_entry(
    source_root: &Path,
    target_name: &str,
    dest_root: &Path,
    name: &str,
    out: &mut Vec<PlannedRemoval>,
) {
    let source = source_root.join(name);
    if !source.exists() {
        out.push(refusal(
            target_name,
            name,
            format!(
                "the declared source {} does not exist, so nothing at the \
                 destination can be confirmed to be a copy this tool \
                 materialized; nothing was removed. Restore the source (mount \
                 the volume it lives on, if it is removable) and re-run, or \
                 delete the destination yourself.",
                source.display()
            ),
        ));
        return;
    }

    if !source.is_dir() {
        let dest = match resolve_removal_dest(dest_root, name, Path::new(name)) {
            Ok(dest) => dest,
            Err(reason) => {
                out.push(refusal(target_name, name, reason));
                return;
            }
        };
        let decision = decide(&dest, Some(&source));
        out.push(PlannedRemoval {
            target: Some(target_name.to_string()),
            label: name.to_string(),
            declared: name.to_string(),
            dest,
            source: Some(source),
            decision,
        });
        return;
    }

    let mut relative = Vec::new();
    if let Err(e) = collect_files(&source, Path::new(""), &mut relative) {
        // A *partial* source listing under-names what this tool owns, which
        // only ever removes less than it should — but a run whose report
        // depends on how far a failing walk happened to get is not one
        // anybody can diff, so it removes nothing at all and says why.
        out.push(refusal(
            target_name,
            name,
            format!(
                "could not enumerate the declared directory {}: {e}; nothing \
                 was removed.",
                source.display()
            ),
        ));
        return;
    }

    for rel in &relative {
        // Built component-by-component rather than `Path::new(name).join(rel)`:
        // `name` is the raw, always-forward-slash manifest text, and a literal
        // join never rewrites the separators already embedded inside it, only
        // the one it inserts. On Windows that would leave the label — which is
        // exactly what a report or refusal line shows — mixing `/` and `\`.
        // The same fix `apply` applies in three places, for the same reason.
        let full_rel: PathBuf = Path::new(name)
            .components()
            .chain(rel.components())
            .collect();
        let label = full_rel.to_string_lossy().into_owned();
        let dest = match resolve_removal_dest(dest_root, name, &full_rel) {
            Ok(dest) => dest,
            Err(reason) => {
                out.push(PlannedRemoval {
                    target: Some(target_name.to_string()),
                    label,
                    declared: name.to_string(),
                    dest: PathBuf::new(),
                    source: None,
                    decision: Decision::Refused(reason),
                });
                continue;
            }
        };
        let file_source = source.join(rel);
        let decision = decide(&dest, Some(&file_source));
        out.push(PlannedRemoval {
            target: Some(target_name.to_string()),
            label,
            declared: name.to_string(),
            dest,
            source: Some(file_source),
            decision,
        });
    }
}

/// Where `unlink` says this target's backups were left.
///
/// Asked with `classify` and nothing else — deliberately no `read_dir`, so
/// the directory is never *counted*. `apply::linked_backup_root`'s doc
/// comment explains why that restraint matters: a `read_dir` of a backup root
/// that is itself a link reads straight through it into whatever it points
/// at. A count would be a nicer report line and is not worth reopening that
/// question, so the line names the path and stops.
fn note_backups(target_name: &str, dest_root: &Path, notes: &mut Vec<Note>) {
    let backups = dest_root.join(BACKUP_DIR);
    // `NotFound` is the normal state when `apply` never had to replace
    // anything here, and it is not a finding. Every other outcome — a real
    // directory, a link, an inspection failure — means something is at that
    // path, and the user should be told it is staying.
    if classify(&backups).is_err_and(|e| e.kind() == io::ErrorKind::NotFound) {
        return;
    }
    notes.push(Note {
        target: Some(target_name.to_string()),
        message: format!(
            "backups left in place: {}. `unlink` never removes or restores \
             them — each one holds only the files a single `apply` run \
             replaced, so no one of them is a snapshot of the state before \
             this tool was installed. Delete it yourself if you no longer \
             want the history.",
            backups.display()
        ),
    });
}

/// Walk from `dest_root` down through every component of `rel` **except the
/// last**, classifying each level before descending.
///
/// The removal-side counterpart of `apply::resolve_scoped_dest`, and a
/// separate function rather than a shared one for the reason that file's own
/// doc comment gives about `prune_scope`: folding walks with different
/// termination rules together means threading a flag through the one place
/// correctness matters most. The rules that differ here are the wording of
/// every refusal ("nothing was removed", which a caller must be able to print
/// verbatim) and what the *last* component may be — on the write side it is
/// something about to be created, here it is something about to be deleted,
/// and [`decide`] owns that question.
///
/// What it refuses, and why each one matters to a command that deletes:
///
///   - **A link at any intermediate component.** This is the scope escape.
///     `dest_root.join("modes/one.md")` is resolved by the kernel *through*
///     `dest_root/modes` before `classify` ever sees the end of the path, so
///     if `dest_root/modes` is a link, the file this function would hand back
///     for removal can live anywhere on the disk — and on the copy path it
///     would then be compared, byte for byte, against a source that a
///     deliberately-staged link could easily make match. Walking level by
///     level is what turns "where does this path end up" into "does getting
///     there cross anything I must not cross".
///   - **A component that is not a plain name** (`..`, an absolute root, a
///     Windows prefix). `manifest::validate_entry` checks only an entry's net
///     depth, so `"a/b/../../c"` reaches here today; refusing the component
///     outright means no `..` ever takes part in building a path this code is
///     about to delete.
///   - **A component named [`BACKUP_DIR`], at any position including the
///     last.** This tool's own bookkeeping directory is never removed through
///     a declared entry, and unlike the link and inspection checks (which
///     correctly stop one component short) that holds for the final component
///     too.
///
/// An intermediate component that does not exist returns the joined path
/// rather than an error, and that is not a copy-paste artifact of the write
/// side: nothing can exist beneath a path component that is not there, so the
/// destination cannot exist either, and [`decide`] will classify it `Absent`
/// a moment later. Returning "nothing to remove" by the ordinary route beats
/// inventing a second one.
fn resolve_removal_dest(dest_root: &Path, declared: &str, rel: &Path) -> Result<PathBuf, String> {
    let mut names = Vec::new();
    for component in rel.components() {
        match component {
            Component::Normal(name) => {
                if name == BACKUP_DIR {
                    return Err(format!(
                        "the declared entry {declared:?} would reach {BACKUP_DIR:?} \
                         inside its own destination path; {BACKUP_DIR} is this \
                         tool's own backup directory, and nothing is ever removed \
                         through or as it. Nothing was removed."
                    ));
                }
                names.push(name);
            }
            // Names the same directory; neither descends nor needs checking.
            Component::CurDir => continue,
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "the declared entry {declared:?} contains a path component \
                     that is not a plain name, so where it lands cannot be \
                     established without resolving the filesystem; nothing was \
                     removed. Spell it as a plain relative path."
                ));
            }
        }
    }

    // Always rebuilt from `names` — the components this walk itself split out
    // — never from `rel` directly, so every separator in the result is one a
    // real `.push()` inserted. See `plan_copy_entry`'s `full_rel` for the
    // Windows display bug this avoids.
    let joined = |names: &[&std::ffi::OsStr]| {
        let mut path = dest_root.to_path_buf();
        for name in names {
            path.push(name);
        }
        path
    };

    // `split_last` returning `None` is unreachable in practice —
    // `manifest::validate_entry` already refuses an entry that normalizes to
    // no components — but treated as "nothing to check" rather than assumed
    // away, so a future caller that got this wrong fails safe.
    let Some((_last, intermediate)) = names.split_last() else {
        return Ok(joined(&names));
    };

    let mut current = dest_root.to_path_buf();
    for name in intermediate {
        current.push(name);

        // classify() before any metadata question: `is_dir()` and
        // `is_symlink()` can both be true at once on a junction, so `is_dir()`
        // alone never answers "is it safe to descend here".
        match classify(&current) {
            Ok(LinkClass::NotALink) => {}
            Ok(LinkClass::DirLink | LinkClass::FileLink) => {
                return Err(format!(
                    "{} is a link, so reaching the declared entry {declared:?} \
                     would mean resolving through it and possibly outside {}; \
                     nothing was removed. Remove the link yourself if it is \
                     not wanted, then re-run.",
                    current.display(),
                    dest_root.display()
                ));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(joined(&names)),
            Err(e) => {
                return Err(format!(
                    "could not inspect {} while resolving the declared entry \
                     {declared:?}: {e}; nothing was removed.",
                    current.display()
                ));
            }
        }
    }

    Ok(joined(&names))
}

/// Decide what one destination is, and therefore whether it may be removed.
///
/// `source` is `None` only for a `link` entry whose declared source has since
/// been deleted — see [`plan_link_entry`].
fn decide(dest: &Path, source: Option<&Path>) -> Decision {
    match classify(dest) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Decision::Absent,
        Err(e) => Decision::Refused(format!(
            "could not inspect the destination {}: {e}; nothing was removed.",
            dest.display()
        )),
        // Removing a link destroys nothing — `remove_link` unlinks the link
        // object itself and never traverses it — so where it points cannot
        // change this answer, and no target comparison is made. This holds
        // for a `copy` entry's destination too: `apply` already treats a link
        // found at one as satisfying that entry (`apply::is_same`), so a link
        // there is this tool's to clear either way.
        Ok(LinkClass::DirLink | LinkClass::FileLink) => Decision::RemoveLink,
        Ok(LinkClass::NotALink) => decide_over_real_data(dest, source),
    }
}

/// The destination holds real data. This is the whole risk of the command:
/// there is no link indirection here, so anything removed is removed for
/// good.
fn decide_over_real_data(dest: &Path, source: Option<&Path>) -> Decision {
    let meta = match std::fs::symlink_metadata(dest) {
        Ok(meta) => meta,
        // Classified a moment ago and gone now. Nothing to remove.
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Decision::Absent,
        Err(e) => {
            return Decision::Refused(format!(
                "could not inspect the destination {}: {e}; nothing was removed.",
                dest.display()
            ));
        }
    };

    if meta.is_dir() {
        // The brief's own example of the refusal that must never become a
        // deletion: an unrelated real directory where a link was declared.
        // Removing it would mean recursing into a tree this tool did not
        // create, which is the one thing it will not do.
        return Decision::Refused(format!(
            "a real directory already exists at {}; this tool never removes a \
             directory it did not materialize as a link, so nothing was \
             removed. Delete it yourself if it is not wanted.",
            dest.display()
        ));
    }

    if !meta.is_file() {
        return Decision::Refused(format!(
            "{} is neither a link nor a plain file, so it is not something \
             this tool materialized; nothing was removed.",
            dest.display()
        ));
    }

    let Some(source) = source else {
        return Decision::Refused(format!(
            "a real file exists at {} and the declared source it would be \
             compared against no longer exists, so it cannot be confirmed to \
             be a copy this tool materialized; nothing was removed. Delete it \
             yourself if it is not wanted.",
            dest.display()
        ));
    };

    if has_identical_bytes(source, dest) {
        Decision::RemoveCopy
    } else {
        // Constraint 4: this could delete the only copy of an edit the user
        // made. Refused, not backed-up-and-deleted — the latter would make
        // `unlink` a command that writes, and would silently discard an edit
        // into a directory the user then has to know to look in.
        Decision::Refused(format!(
            "the file at {} does not match the declared source {} byte for \
             byte — it was edited after it was materialized, or the source \
             changed kind — so removing it could destroy the only copy of \
             that content; nothing was removed. Save or delete it yourself, \
             then re-run.",
            dest.display(),
            source.display()
        ))
    }
}

/// Carry out `plan`.
///
/// Acts only on what the plan already decided: nothing here re-derives
/// *whether* a destination is this tool's. What it does re-check is the
/// filesystem, immediately before each removal — the same caution
/// `apply::prune_one` applies, for the same reason: an object that has
/// changed since the plan was computed must not be deleted on the strength of
/// a stale snapshot.
///
/// **A re-check that says "leave it" sets the refused flag, and that is a
/// deliberate divergence from `prune_one`, which does not.** Prune's
/// leave-in-place cases are about objects nobody declared (a stray link
/// inside a materialized directory), and failing a run over one would turn an
/// editor's backup file into a permanently red CI gate. Every destination
/// here is *declared work*: a removal the manifest asked for and this run did
/// not perform. Reporting exit 0 for that is the silent-success shape this
/// command exists to stop producing.
pub fn execute(plan: &Plan) -> Report {
    let mut lines = Vec::with_capacity(plan.removals.len() + plan.notes.len());
    let mut refused = false;
    let mut failed = false;

    for removal in &plan.removals {
        let body = match &removal.decision {
            Decision::Absent => format!("not present: {}", removal.label),
            Decision::Refused(reason) => {
                refused = true;
                // Reported, then skipped. One refusal must never decide the
                // fate of the others; it only sets the run's exit code.
                format!("refused: {}: {reason}", removal.label)
            }
            Decision::RemoveLink | Decision::RemoveCopy => match remove_one(removal) {
                Ok(None) => match removal.decision {
                    Decision::RemoveLink => format!("removed link: {}", removal.label),
                    _ => format!("removed: {}", removal.label),
                },
                Ok(Some(reason)) => {
                    refused = true;
                    format!("left in place: {}: {reason}", removal.label)
                }
                Err(e) => {
                    failed = true;
                    format!("failed: could not remove {}: {e}", removal.label)
                }
            },
        };
        lines.push(with_target(&removal.target, &body));
    }

    for note in &plan.notes {
        lines.push(with_target(&note.target, &note.message));
    }

    Report {
        lines,
        refused,
        failed,
    }
}

/// Remove one destination, or explain why it was left alone.
///
/// `Ok(None)` means it was removed; `Ok(Some(reason))` means the filesystem
/// no longer matches what the plan decided about it, and it was not.
///
/// The two removal calls in this function are the only two in the module, and
/// each is the narrowest primitive that can do its job:
/// `safelink::remove::remove_link`, which refuses anything that is not a link
/// and never traverses one (Constraint 5 — this layer never reimplements
/// that), and `std::fs::remove_file`, which cannot remove a directory and
/// cannot recurse. There is no third.
fn remove_one(removal: &PlannedRemoval) -> io::Result<Option<String>> {
    let class = match classify(&removal.dest) {
        Ok(class) => class,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(Some("it is no longer there".to_string()));
        }
        Err(e) => return Err(e),
    };

    match (&removal.decision, class) {
        (Decision::RemoveLink, LinkClass::DirLink | LinkClass::FileLink) => {
            safelink::remove::remove_link(&removal.dest)?;
            Ok(None)
        }
        (Decision::RemoveLink, LinkClass::NotALink) => Ok(Some(
            "it stopped being a link after this run was planned, so it is \
             real data now; refusing to remove it"
                .to_string(),
        )),
        (Decision::RemoveCopy, LinkClass::DirLink | LinkClass::FileLink) => Ok(Some(
            "it became a link after this run was planned; refusing to remove \
             it or to delete anything through it"
                .to_string(),
        )),
        (Decision::RemoveCopy, LinkClass::NotALink) => {
            if !std::fs::symlink_metadata(&removal.dest)?.is_file() {
                return Ok(Some(
                    "it is no longer a plain file; refusing to remove it".to_string(),
                ));
            }
            // The byte comparison is made again, not trusted from plan time.
            // The window is small, but what is on the other side of it is a
            // user's edit being deleted, and re-reading two files is cheap
            // next to that.
            let matches = removal
                .source
                .as_deref()
                .is_some_and(|source| has_identical_bytes(source, &removal.dest));
            if !matches {
                return Ok(Some(
                    "it stopped matching the declared source after this run \
                     was planned; refusing to remove it"
                        .to_string(),
                ));
            }
            std::fs::remove_file(&removal.dest)?;
            Ok(None)
        }
        // `execute` only calls this for the two removal decisions.
        (Decision::Absent | Decision::Refused(_), _) => Ok(Some(
            "internal error: nothing was planned to be removed here".to_string(),
        )),
    }
}

/// A refusal reached before a destination could be resolved at all.
fn refusal(target_name: &str, label: &str, reason: String) -> PlannedRemoval {
    PlannedRemoval {
        target: Some(target_name.to_string()),
        label: label.to_string(),
        declared: label.to_string(),
        dest: PathBuf::new(),
        source: None,
        decision: Decision::Refused(reason),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three shapes `resolve_removal_dest` refuses outright, none of
    /// which needs a filesystem to reach.
    #[test]
    fn a_path_that_is_not_a_plain_relative_name_is_refused() {
        let root = Path::new("/dest");

        let err = resolve_removal_dest(root, "a/../../b", Path::new("a/../../b")).unwrap_err();
        assert!(err.contains("not a plain name"), "got: {err}");
        assert!(err.contains("nothing was removed"), "got: {err}");

        let err = resolve_removal_dest(root, BACKUP_DIR, Path::new(BACKUP_DIR)).unwrap_err();
        assert!(err.contains(BACKUP_DIR), "got: {err}");

        // Including as a non-final component.
        let nested = format!("{BACKUP_DIR}/20240101T000000Z/note.md");
        let err = resolve_removal_dest(root, &nested, Path::new(&nested)).unwrap_err();
        assert!(err.contains(BACKUP_DIR), "got: {err}");
    }
}
