//! `doctor`: read-only diagnostics over the current install.
//!
//! Every check here inspects and prints; none of them write, link, delete,
//! or back anything up. That is a stronger promise than `status`'s, which
//! reports on `link` entries only. Both take each entry's verdict from the
//! same [`crate::apply::plan`], so they cannot disagree about one; `doctor`
//! additionally answers "is the current state ambiguous in a way `apply`
//! cannot see", which needs more than that per-entry decision — link-target
//! drift and backup-root health among them.
//!
//! Checks 3 and 4 below are laid out as separate, individually named
//! functions rather than folded into one big loop, on purpose: the
//! Windows-only elevation and Developer Mode reporting hooks into this
//! module by adding another such function and calling it from [`diagnose`],
//! not by re-reading this file's control flow to find a place to splice in.
//! That is [`report_windows_environment`] below: its formatting logic
//! is cross-platform and unit-tested here via [`crate::winenv::ForcedWindowsFacts`],
//! but [`diagnose`] only ever calls it inside a `#[cfg(windows)]` block with
//! the real [`crate::winenv::RealWindowsFacts`] -- on every other platform
//! this section of the report does not run at all, rather than printing a
//! stub answer that would mean nothing there.
//!
//! **Check 3 is built on [`crate::apply::plan`], not a second, parallel
//! drift-checker.** A `doctor` that disagreed with what `apply --dry-run`
//! would do would be dishonest; building on the same `plan()` output makes
//! that disagreement structurally impossible. `doctor` never calls
//! [`crate::apply::execute`] — only `describe()`-shaped reporting over the
//! plan `plan()` already computed.

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

    // Windows-only, report-only: never folded into `drifted` or `failed`.
    // See this module's doc comment and `report_windows_environment`'s own
    // doc comment for why this section is skipped entirely, not stubbed,
    // on every other platform.
    #[cfg(windows)]
    lines.extend(report_windows_environment(&crate::winenv::RealWindowsFacts));

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
/// stale file the source no longer has.
///
/// `PruneDecision::LeftInPlace` is drift only when its own destination is
/// *not* itself a link: a link sitting where a prune candidate was expected
/// is a deliberate non-action `apply` refuses to touch, and reporting that as
/// something to fix would be wrong. But that is not `LeftInPlace`'s only
/// producer: a plain stale file
/// that `apply` left in place because it could not safely back it up before
/// removing it — currently, only because the target's own backup root is a
/// link (see Check 5) — genuinely is drift, with the same remedy Check 5
/// already gives, and saying nothing about it here would silently drop a
/// finding `doctor` already has on hand. The two are told apart below by
/// reclassifying `prune.dest` itself, not by matching `reason`'s text, so a
/// future third producer of `LeftInPlace` is judged by what is actually on
/// disk rather than by what words its message happens to use.
///
/// `plan.notes` are printed too, and printed first, matching
/// [`crate::apply::Plan::describe`]'s own ordering: a note is what the
/// planner *could not determine* (an unreadable prune subtree, a skipped
/// retention sweep). Honesty requires reporting what a run could not decide
/// just as much as what it decided. A note never changes the exit code, the
/// same as it never changes `apply`'s.
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
        match &prune.decision {
            PruneDecision::Remove => {
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
            PruneDecision::LeftInPlace(reason) => {
                // A link sitting where a prune candidate was expected is
                // deliberate, not drift — `prune.dest` itself classifies as a
                // link. Anything else left in place is drift: a stale file the
                // source no longer has that could not be safely removed. `reason`
                // already carries the specific remedy.
                if !matches!(
                    classify(&prune.dest),
                    Ok(LinkClass::DirLink | LinkClass::FileLink)
                ) {
                    drifted = true;
                    lines.push(with_target(
                        &prune.target,
                        &format!("drift: {}: {reason}", prune.label),
                    ));
                }
            }
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
/// not wired into `apply`'s decision logic. `apply` continues to treat such
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
/// **The "dangling" outcome below is a TOCTOU catch, not the path either
/// platform takes for a link that was already dangling when `plan()`
/// classified it — that case never reaches this function at all, on either
/// platform.** An earlier version of this comment claimed a Windows
/// junction is classified "structurally, regardless of target validity,"
/// and that a from-the-start-dangling junction therefore reaches here as
/// `Decision::Same`. A real `windows-latest` CI run (this branch's PR, its
/// first Windows run) disproved that outright: the equivalent test entry
/// resolved to `Decision::Write` and was reported by [`report_plan_drift`]'s
/// generic message, the same outcome POSIX already has for this scenario,
/// never reaching this function. The likely mechanism (traced by reading
/// the vendored `junction` crate's source, not re-guessed): its `exists()`
/// opens with `if !path.exists() { return Ok(false) }`, and `Path::exists`
/// follows the reparse point, so a dangling junction fails that check
/// before the structural reparse-tag query ever runs — but the exact
/// resulting `LinkClass` was not independently pinned by a Windows-side
/// unit assertion (this dev environment cannot run one), so it is
/// documented here as the probable cause, not asserted as unverified fact;
/// doing so would repeat the exact mistake this correction exists to fix.
/// What is
/// pinned by a real, already-passing assertion on every platform this runs
/// on: `is_same()` returns `false` for a from-the-start-dangling directory
/// link (this module's own test
/// `canonicalize_round_trips_through_a_directory_link_and_fails_once_the_target_is_gone`
/// asserts the POSIX `LinkClass::FileLink` fold directly; the Windows side
/// is pinned only at the integration level, by
/// `tests/doctor.rs`'s `reports_a_dangling_link_as_drift`, which now
/// asserts the actually-observed `Decision::Write` wording instead of the
/// disproven "reaches check 4" claim). What this function's `Err(_)` arm
/// below actually catches: a link `plan()` classified as `Same` (a real,
/// resolvable target at plan time) whose target is removed, or otherwise
/// stops resolving, in the window between `plan()` and this check running
/// — the same live-reclassification TOCTOU this module's other checks (5,
/// and check 3's `LeftInPlace` disambiguation) already have, not a new
/// class of risk.
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
                // *source* below) means the link itself is dangling: its target
                // no longer exists. Reported distinctly from "wrong target"
                // because the fix is the same but the finding is a different fact
                // about the filesystem, and conflating them would obscure which
                // one a user is looking at.
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
/// severity class as the write-scope-escape bugs already fixed in `apply`:
/// backups written during a future `apply` run could then land somewhere
/// other than this target, or a restore could read from somewhere else
/// entirely. `apply` itself closes the write/delete side of this; this check
/// is only the diagnostic.
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

/// Check 6 (Windows only): process elevation and Developer Mode.
///
/// **Report-only.** Neither fact ever sets `drifted` or `failed` -- see the
/// call site in [`diagnose`], which does not even look at this function's
/// return value beyond appending it to `lines`. A standard, non-elevated
/// user with Developer Mode off is the fully supported, intended case this
/// entire project exists for (junctions need neither), so this prints the
/// two facts as a diagnostic courtesy, not a pass/fail verdict -- the
/// wording below says so explicitly, so "not elevated" or "off" does not
/// read as something to fix.
///
/// Takes `facts` rather than querying the OS itself so this formatting
/// logic is unit-testable on any platform, including this Linux dev
/// machine, via [`crate::winenv::ForcedWindowsFacts`] -- see
/// `crate::winenv`'s own doc comment for the full reasoning, which mirrors
/// `safelink::capability::SymlinkProbe`.
///
/// [`diagnose`]'s only call site is `#[cfg(windows)]`-gated, so on every
/// other platform this function's sole caller is its own `#[cfg(test)]`
/// module below -- hence the `allow` here, rather than a warning that would
/// otherwise fail CI's `-D warnings` clippy gate on non-Windows builds.
///
/// Each fact is `Option<bool>` (see [`crate::winenv::WindowsFacts`]'s own
/// doc comment): `None` gets its own "could not be determined" wording
/// rather than being folded into `false`, so a query that did not complete
/// is never printed as the observed fact "not elevated" / "off" it never
/// actually confirmed. `None` is still report-only, the same as `Some(_)`.
#[cfg_attr(not(windows), allow(dead_code))]
fn report_windows_environment(facts: &dyn crate::winenv::WindowsFacts) -> Vec<String> {
    vec![
        format!(
            "elevation: {}. Not something to fix either way -- a standard, \
             non-elevated process is the fully supported, intended case; \
             junctions need no elevation.",
            match facts.is_elevated() {
                Some(true) => "this process is running elevated",
                Some(false) => "this process is not elevated",
                None => "could not be determined",
            }
        ),
        format!(
            "developer mode: {}. Not something to fix either way -- \
             junctions do not need it; it only affects whether file \
             symlinks can be created without elevation.",
            match facts.developer_mode_enabled() {
                Some(true) => "enabled",
                Some(false) => "off",
                None => "could not be determined",
            }
        ),
    ]
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

    /// Runs on every platform, including this Linux dev machine: it is the
    /// *formatting* of the two facts into `doctor`'s output under test, not
    /// the real OS query -- see `crate::winenv`'s doc comment for why that
    /// split exists. Covers all four boolean combinations so neither fact's
    /// wording is silently coupled to the other's value.
    #[test]
    fn report_windows_environment_names_both_facts_as_report_only() {
        use crate::winenv::ForcedWindowsFacts;

        let elevated_and_dev_mode = report_windows_environment(&ForcedWindowsFacts {
            elevated: Some(true),
            developer_mode: Some(true),
        });
        assert!(elevated_and_dev_mode[0].contains("elevation: this process is running elevated"));
        assert!(elevated_and_dev_mode[1].contains("developer mode: enabled"));

        let neither = report_windows_environment(&ForcedWindowsFacts {
            elevated: Some(false),
            developer_mode: Some(false),
        });
        assert!(neither[0].contains("elevation: this process is not elevated"));
        assert!(neither[1].contains("developer mode: off"));

        // Every combination's wording says explicitly that this is not a
        // pass/fail verdict — users should not read "not elevated" as a
        // problem to fix.
        for combo in [
            (Some(true), Some(false)),
            (Some(false), Some(true)),
            (Some(true), Some(true)),
            (Some(false), Some(false)),
        ] {
            let lines = report_windows_environment(&ForcedWindowsFacts {
                elevated: combo.0,
                developer_mode: combo.1,
            });
            for line in &lines {
                assert!(
                    line.contains("Not something to fix"),
                    "must not read as a health-check verdict: {line}"
                );
            }
        }
    }

    /// The honesty case: a query that did not complete must never be printed
    /// as the observed fact "not elevated" / "off", because that would report
    /// an assumption as a fact. `None` gets its own distinct wording instead,
    /// and it must still read as report-only.
    #[test]
    fn report_windows_environment_never_reports_unknown_as_a_definite_fact() {
        use crate::winenv::ForcedWindowsFacts;

        let unknown = report_windows_environment(&ForcedWindowsFacts {
            elevated: None,
            developer_mode: None,
        });
        assert!(
            unknown[0].contains("could not be determined")
                && !unknown[0].contains("not elevated")
                && !unknown[0].contains("running elevated"),
            "{}",
            unknown[0]
        );
        assert!(
            unknown[1].contains("could not be determined")
                && !unknown[1].contains("developer mode: off")
                && !unknown[1].contains("developer mode: enabled"),
            "{}",
            unknown[1]
        );
        for line in &unknown {
            assert!(
                line.contains("Not something to fix"),
                "must not read as a health-check verdict: {line}"
            );
        }
    }

    /// The empirical probe `report_link_targets`'s comparison logic is built
    /// on, run before trusting any assumption about what `canonicalize` does
    /// to a directory link. Platform behavior should be tested, not guessed.
    ///
    /// Confirms the round-trip while the link is healthy, then confirms —
    /// on POSIX, with a real assertion, not just prose — what happens once
    /// the target is gone: `classify` itself already folds a dangling
    /// directory symlink into `LinkClass::FileLink` (its own
    /// metadata-follow-through fails the same way `canonicalize` does), so
    /// `apply`'s `decide()` never reaches `Decision::Same` for it and
    /// `report_link_targets` never sees it at all — it is check 3
    /// (`report_plan_drift`) that reports a POSIX dangling link. A real
    /// `windows-latest` CI run of this branch's PR showed the identical
    /// outcome on Windows (`Decision::Write`, reported by check 3, not
    /// check 4) for the same reason `report_link_targets`'s own doc comment
    /// now explains in full — but this test does not assert a specific
    /// Windows-side `LinkClass` for it, since that would need a real
    /// Windows machine or CI run to verify, not this Linux dev environment;
    /// the Windows-side regression coverage for this behavior lives in
    /// `tests/doctor.rs`'s `reports_a_dangling_link_as_drift`, at the
    /// integration level, asserting what CI actually showed rather than what
    /// seemed like it should be true. Earlier versions of these comments made
    /// assumptions that proved wrong.
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

        // Use `remove_dir` (not `remove_dir_all`): `target` is the empty
        // directory created above, so the narrow operation is safe.
        std::fs::remove_dir(&target).unwrap();

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
