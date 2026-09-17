//! `doctor`: read-only diagnostics over the current install.
//!
//! Every check here inspects and prints; none of them write, link, delete,
//! or back anything up. That is a stronger promise than `status`'s (which
//! only checks `link` entries against `safelink::classify`): `doctor` also
//! answers "is the current state ambiguous in a way `apply` cannot see",
//! which needs more than a kind check at each destination.
//!
//! Checks 3 and 4 below are laid out as separate, individually named
//! functions rather than folded into one big loop, on purpose: a later task
//! (Windows-only elevation and Developer Mode reporting) hooks into this
//! module by adding another such function and calling it from [`diagnose`],
//! not by re-reading this file's control flow to find a place to splice in.
//!
//! **Check 3 is built on [`crate::apply::plan`], not a second, parallel
//! drift-checker.** A `doctor` that disagreed with what `apply --dry-run`
//! would do is the exact honesty failure Constraint 3 exists to prevent for
//! `apply` itself; building `doctor` on the same `plan()` output makes that
//! disagreement structurally impossible rather than a discipline to
//! remember. `doctor` never calls [`crate::apply::execute`] — only
//! `describe()`-shaped reporting over the plan `plan()` already computed.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use safelink::capability::SymlinkProbe;
use safelink::classify::{LinkClass, classify};
use safelink::op::LinkOp;

use crate::apply::{self, BACKUP_DIR, Decision, Plan, PruneDecision, with_target};
use crate::manifest::Manifest;

/// What one `doctor` run found.
#[derive(Debug, Clone)]
pub struct DoctorReport {
    /// One line per finding, in check order (environment, tools, plan
    /// drift and prunes, link-target drift, backup-root health).
    pub lines: Vec<String>,
    /// Any check 3-5 finding disagreed with the declared, in-sync state.
    /// Maps to exit code 3. `doctor` never refuses anything (it never
    /// writes), so it never produces exit code 4.
    pub drifted: bool,
    /// A genuine, unexpected I/O failure while inspecting the filesystem —
    /// not merely "missing". Maps to exit code 1.
    pub failed: bool,
}

/// Run every diagnostic check and assemble the full report.
///
/// `manifest_path` is printed verbatim (check 1); `manifest_dir` is the
/// directory the manifest's own `source` and every `plan()` computation
/// resolve against, exactly as `cli::apply` resolves it — the two are
/// threaded through separately because the CLI's `--manifest` flag is not
/// itself a directory.
pub fn diagnose(
    manifest: &Manifest,
    manifest_path: &Path,
    manifest_dir: &Path,
    probe: &dyn SymlinkProbe,
) -> DoctorReport {
    let mut lines = Vec::new();
    let mut drifted = false;
    let mut failed = false;

    lines.extend(report_environment(manifest, manifest_path, manifest_dir));
    lines.push(report_tools());

    let plan = apply::plan(manifest, manifest_dir, probe);

    let (plan_lines, plan_drifted) = report_plan_drift(&plan);
    lines.extend(plan_lines);
    drifted |= plan_drifted;

    let (link_lines, link_drifted, link_failed) = report_link_targets(&plan);
    lines.extend(link_lines);
    drifted |= link_drifted;
    failed |= link_failed;

    let (backup_lines, backup_drifted, backup_failed) = report_backup_roots(manifest);
    lines.extend(backup_lines);
    drifted |= backup_drifted;
    failed |= backup_failed;

    DoctorReport {
        lines,
        drifted,
        failed,
    }
}

/// Check 1: the resolved environment — OS, manifest path, resolved
/// `source`, and every declared target's name and `dest`.
fn report_environment(
    manifest: &Manifest,
    manifest_path: &Path,
    manifest_dir: &Path,
) -> Vec<String> {
    let mut lines = vec![
        format!("os: {}", std::env::consts::OS),
        format!("manifest: {}", manifest_path.display()),
        format!(
            "source: {}",
            apply::resolved_source(manifest, manifest_dir).display()
        ),
    ];
    for name in sorted_target_names(manifest) {
        let target = &manifest.targets[name];
        lines.push(format!("target {name}: {}", target.dest.display()));
    }
    lines
}

/// Check 2: required external tools.
///
/// `agent-grafter` shells out to nothing in production code — every
/// materialization goes through `safelink`'s own primitives
/// (`create_dir_link`, `copy_atomic`, `symlink_file`), never
/// `std::process::Command`. Said outright rather than silently omitted, so
/// a report that lists nothing here reads as "checked, none needed" and not
/// as "this check was forgotten".
fn report_tools() -> String {
    "tools: none required (no external process dependencies)".to_string()
}

/// Check 3: every declared `copy` and `link` entry against the same
/// decision `apply` itself would reach, via [`crate::apply::plan`] — see
/// this module's own doc comment for why that reuse is the point. A prune
/// candidate destined for removal (`PruneDecision::Remove`) is drift too: a
/// stale file the source no longer has. `PruneDecision::LeftInPlace` is not
/// drift here — it is already a link `apply` refuses to touch, which is a
/// distinct, deliberate outcome rather than something to fix.
///
/// `plan.notes` are printed too, and printed first, matching
/// [`crate::apply::Plan::describe`]'s own ordering: a note is what the
/// planner *could not determine* (an unreadable prune subtree, a skipped
/// retention sweep), and Constraint 3's honesty requirement covers what a
/// run could not decide just as much as what it decided. A note never
/// changes the exit code, the same as it never changes `apply`'s.
fn report_plan_drift(plan: &Plan) -> (Vec<String>, bool) {
    let mut lines = Vec::new();
    let mut drifted = false;

    for note in &plan.notes {
        lines.push(with_target(&note.target, &note.message));
    }

    for planned in &plan.entries {
        match &planned.decision {
            Decision::Same => {
                lines.push(with_target(
                    &planned.target,
                    &format!("in sync: {}", planned.label),
                ));
            }
            Decision::Write => {
                drifted = true;
                lines.push(with_target(
                    &planned.target,
                    &format!(
                        "drift: {} is not correctly materialized; re-run `apply` \
                         to fix it.",
                        planned.label
                    ),
                ));
            }
            Decision::Refused(reason) => {
                drifted = true;
                lines.push(with_target(
                    &planned.target,
                    &format!("drift: {}: {reason}", planned.label),
                ));
            }
        }
    }

    for prune in &plan.prunes {
        if matches!(prune.decision, PruneDecision::Remove) {
            drifted = true;
            lines.push(with_target(
                &prune.target,
                &format!(
                    "drift: {} is a stale file the source no longer has; re-run \
                     `apply` to remove it.",
                    prune.label
                ),
            ));
        }
    }

    (lines, drifted)
}

/// Check 4: does each already-in-sync directory link actually resolve to
/// the source it is declared for, not just the right *kind* of object at
/// its destination?
///
/// This is the gap [`crate::apply::is_same`]'s own doc comment names: kind
/// alone reports a link left over from a previous manifest edit — one that
/// now points at a source no longer declared — as `same`. Closing it here,
/// not in `is_same()`: this is a **doctor-only diagnostic**, deliberately
/// not wired into `apply`'s decision logic (which just went through several
/// review rounds of its own for Task S3c). `apply` continues to treat such
/// a link as `Same` and will not touch it — so the fix this reports is
/// "remove the link, then re-run `apply`", never "re-run apply" alone,
/// which would do nothing: `apply` would see the same stale link and leave
/// it alone again.
///
/// Only reachable for a [`LinkOp::DirectoryLink`] entry whose plan-time
/// decision is [`Decision::Same`]: a `Decision::Write` entry (wrong kind,
/// dangling in a way `classify` already caught, or absent) is already
/// reported as drift by [`report_plan_drift`], and double-reporting it here
/// would say the same thing twice under two different names.
///
/// **The "dangling" outcome below is Windows-reachable far more often than
/// POSIX-reachable, and that asymmetry is a fact about `classify`, not a gap
/// in this function.** A POSIX directory symlink whose target has been
/// removed makes `classify`'s own metadata-follow-through fail, which folds
/// it into `LinkClass::FileLink` (see `safelink::classify`'s unix impl) —
/// `is_same()` then already returns `false` for it, `decide()` already
/// returns `Decision::Write`, and `report_plan_drift` already reports it.
/// This function never sees it, because it only runs on `Decision::Same`.
/// A Windows junction is classified *structurally*: `junction::exists`
/// reports `DirLink` regardless of whether the target it names still
/// exists, so a dangling junction reaches here as `Decision::Same`, and
/// `canonicalize(&entry.dest)` is what actually discovers it is dangling.
/// Verified empirically, not assumed — see this module's own test
/// `canonicalize_round_trips_through_a_directory_link_and_fails_once_the_target_is_gone`,
/// per Constraint 8.
fn report_link_targets(plan: &Plan) -> (Vec<String>, bool, bool) {
    let mut lines = Vec::new();
    let mut drifted = false;
    let mut failed = false;

    for planned in &plan.entries {
        let Some(entry) = &planned.entry else {
            continue;
        };
        if entry.op != LinkOp::DirectoryLink || planned.decision != Decision::Same {
            continue;
        }

        match std::fs::canonicalize(&entry.dest) {
            Err(_) => {
                // Resolves through the link to what it actually points at.
                // Failing here (as opposed to failing to canonicalize the
                // *source* below) means the link itself is dangling: its
                // target no longer exists. Reported distinctly from "wrong
                // target" below, per the task brief — the fix reads the
                // same, but the finding is a different fact about the
                // filesystem and conflating them would obscure which one a
                // user is looking at.
                drifted = true;
                lines.push(with_target(
                    &planned.target,
                    &format!(
                        "drift: {} is a dangling link: its target no longer \
                         exists. Remove the link, then re-run `apply` so it is \
                         materialized correctly.",
                        planned.label
                    ),
                ));
            }
            Ok(actual) => match std::fs::canonicalize(&entry.source) {
                Ok(expected) if actual == expected => {
                    lines.push(with_target(
                        &planned.target,
                        &format!("in sync: {} link target", planned.label),
                    ));
                }
                Ok(expected) => {
                    drifted = true;
                    lines.push(with_target(
                        &planned.target,
                        &format!(
                            "drift: {} points at {} but the manifest declares {}. \
                             Remove the link, then re-run `apply` so it is \
                             materialized correctly.",
                            planned.label,
                            actual.display(),
                            expected.display()
                        ),
                    ));
                }
                Err(e) => {
                    // The plan itself already confirmed this entry's source
                    // exists (a missing source is refused before an `Entry`
                    // is ever built) — reaching an error here means it
                    // disappeared between planning and this check, or some
                    // other genuine I/O failure. Undecidable, not drift.
                    failed = true;
                    lines.push(with_target(
                        &planned.target,
                        &format!(
                            "could not resolve the expected target for {}: {e}",
                            planned.label
                        ),
                    ));
                }
            },
        }
    }

    (lines, drifted, failed)
}

/// Check 5: each target's own backup root ([`BACKUP_DIR`]) must be a plain
/// directory, never a link.
///
/// A linked backup directory is a genuinely dangerous finding, the same
/// severity class as the write-scope-escape bugs already fixed in `apply`
/// (S3b/S3c): backups written during a future `apply` run could then land
/// somewhere other than this target, or a restore could read from
/// somewhere else entirely. Task 2 of this plan closes the write/delete
/// side of this in `apply` itself; this check is only the diagnostic.
///
/// Deduplicated by the resolved backup path, the same way
/// `crate::apply::plan_target_retention` dedupes its own sweep: two
/// `[targets.*]` may point at the same `dest` and therefore share one
/// `.agrafter-backups/`, and reporting it twice would say the same finding
/// under two target names for what is actually one directory.
fn report_backup_roots(manifest: &Manifest) -> (Vec<String>, bool, bool) {
    let mut lines = Vec::new();
    let mut drifted = false;
    let mut failed = false;
    let mut seen: HashSet<PathBuf> = HashSet::new();

    for name in sorted_target_names(manifest) {
        let target = &manifest.targets[name];
        let backups = target.dest.join(BACKUP_DIR);
        if !seen.insert(backups.clone()) {
            continue;
        }
        let target_name = Some(name.clone());

        match classify(&backups) {
            Ok(LinkClass::NotALink) => {
                lines.push(with_target(
                    &target_name,
                    &format!("in sync: {BACKUP_DIR} backup root"),
                ));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // No backups have ever been written here. Not a finding.
            }
            Ok(LinkClass::DirLink | LinkClass::FileLink) => {
                drifted = true;
                lines.push(with_target(
                    &target_name,
                    &format!(
                        "drift: {BACKUP_DIR} at {} is a link; backups written \
                         during `apply` could then be written to, or read from, \
                         somewhere other than this target. Remove or rename \
                         whatever currently occupies {BACKUP_DIR}, then re-run.",
                        backups.display()
                    ),
                ));
            }
            Err(e) => {
                failed = true;
                lines.push(with_target(
                    &target_name,
                    &format!("could not inspect {}: {e}", backups.display()),
                ));
            }
        }
    }

    (lines, drifted, failed)
}

/// Target names in a deterministic (sorted) order, for the same reason
/// `cli::sorted_target_names` sorts them: `Manifest::targets` is a
/// `HashMap`, whose iteration order is otherwise unspecified, and a report
/// whose target order shuffles between runs is one nobody can diff.
fn sorted_target_names(manifest: &Manifest) -> Vec<&String> {
    let mut names: Vec<&String> = manifest.targets.keys().collect();
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_line_matches_the_documented_wording() {
        assert_eq!(
            report_tools(),
            "tools: none required (no external process dependencies)"
        );
    }

    /// The empirical probe `report_link_targets`'s comparison logic is built
    /// on, run before trusting any assumption about what `canonicalize`
    /// does to a directory link (Constraint 8: "the last three Windows CI
    /// rounds" happened from guessing this instead of testing it).
    ///
    /// Confirms the round-trip while the link is healthy, then confirms —
    /// and documents — the platform asymmetry once the target is gone: on
    /// POSIX, `classify` itself already folds a dangling directory symlink
    /// into `LinkClass::FileLink` (its own metadata-follow-through fails the
    /// same way `canonicalize` does), so `apply`'s `decide()` never reaches
    /// `Decision::Same` for it and `report_link_targets` never sees it at
    /// all. This is not asserted as a POSIX limitation to work around; it is
    /// the reason check 3 already reports a POSIX dangling link, and check
    /// 4's own dangling branch is what fires on Windows instead, where
    /// `junction::exists` classifies structurally regardless of target
    /// validity.
    #[test]
    fn canonicalize_round_trips_through_a_directory_link_and_fails_once_the_target_is_gone() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target");
        std::fs::create_dir(&target).unwrap();
        let link = tmp.path().join("link");
        safelink::create::create_dir_link(&target, &link).unwrap();

        assert_eq!(
            std::fs::canonicalize(&link).unwrap(),
            std::fs::canonicalize(&target).unwrap(),
            "canonicalize must resolve a healthy directory link to its target"
        );

        std::fs::remove_dir_all(&target).unwrap();

        assert!(
            std::fs::canonicalize(&link).is_err(),
            "canonicalize of a dangling link must fail"
        );

        #[cfg(unix)]
        assert_eq!(
            classify(&link).unwrap(),
            LinkClass::FileLink,
            "POSIX classify() folds a dangling directory symlink into FileLink, \
             not DirLink -- documented here so the doc comment above cannot \
             silently drift from what the platform actually does"
        );
    }
}
