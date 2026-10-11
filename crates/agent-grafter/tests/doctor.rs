//! Tests for `doctor`'s read-only diagnostics: reused-plan drift (check 3),
//! link-target drift (check 4), backup-root health (check 5), and the
//! CLI-level contract (exit codes, `--manifest`).
//!
//! Most of these call `agent_grafter::doctor::diagnose` directly, the same
//! way `tests/apply.rs` calls `agent_grafter::apply::{plan, execute}`
//! directly — no probe injection is needed here (`RealProbe` is enough,
//! since none of these scenarios depend on the `FileSymlink -> FileCopy`
//! downgrade), but `diagnose` is a pure library function and testing it as
//! one avoids a process boundary for every case. The one exception is the
//! missing-manifest exit-code test, which needs a real process: `cli::doctor`
//! is not `pub`, so the exit-code contract can only be observed by spawning
//! the built binary.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use agent_grafter::doctor;
use agent_grafter::manifest::{self, Manifest};
use safelink::capability::RealProbe;

/// A temp-dir sandbox laid out the way a real install is: a `source/`
/// directory holding the canonical files, a `dest/` directory standing in
/// for a target root, and a manifest file between them. Mirrors
/// `tests/apply.rs`'s own `Env` — deliberately not shared between the two
/// files, the same way `tests/cli.rs`'s `TestEnv` and `tests/apply.rs`'s
/// `Env` are each their own small thing.
struct Env {
    dir: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("create temp dir");
        fs::create_dir(dir.path().join("source")).expect("create source dir");
        fs::create_dir(dir.path().join("dest")).expect("create dest dir");
        Env { dir }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn source(&self) -> PathBuf {
        self.dir.path().join("source")
    }

    fn dest(&self) -> PathBuf {
        self.dir.path().join("dest")
    }

    fn manifest_path(&self) -> PathBuf {
        self.dir.path().join("agent-grafter.toml")
    }

    fn write_source(&self, rel: &str, bytes: &[u8]) {
        let path = self.source().join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create source parent");
        }
        fs::write(path, bytes).expect("write source file");
    }

    /// Write a manifest declaring `source = "source"` and a single
    /// `[targets.example]` pointing at this env's `dest/`, with `body`
    /// supplying the `copy`/`link` lines. Returns the parsed `Manifest`, the
    /// same shortcut `tests/apply.rs`'s `Env::write_manifest` takes.
    fn write_manifest(&self, body: &str) -> Manifest {
        let toml = format!(
            "source = \"source\"\n{body}\n\n[targets.example]\ndest = '{}'\n",
            self.dest().display()
        );
        fs::write(self.manifest_path(), &toml).expect("write manifest");
        manifest::parse(&toml).expect("parse manifest")
    }

    /// Plan and execute in one step, materializing the manifest for real —
    /// the starting point for every drift scenario below, which then
    /// disturbs what this just put in place.
    fn apply(&self, manifest: &Manifest) {
        let plan = agent_grafter::apply::plan(manifest, self.root(), &RealProbe);
        let report = agent_grafter::apply::execute(&plan, &RealProbe);
        assert!(
            !report.failed && !report.refused,
            "fixture setup must not itself fail or refuse: {}",
            report.text()
        );
    }

    fn diagnose(&self, manifest: &Manifest) -> doctor::DoctorReport {
        doctor::diagnose(manifest, &self.manifest_path(), self.root(), &RealProbe)
    }
}

const TOOLS_LINE: &str = "tools: none required (no external process dependencies)";

// ---------------------------------------------------------------------------
// In sync: a correctly materialized copy and link both report clean.
// ---------------------------------------------------------------------------

#[test]
fn reports_in_sync_when_everything_is_correctly_materialized() {
    let env = Env::new();
    env.write_source("AGENTS.md", b"canonical contents");
    env.write_source("memory/note.md", b"remembered");
    let manifest = env.write_manifest("copy = [\"AGENTS.md\"]\nlink = [\"memory\"]");
    env.apply(&manifest);

    let report = env.diagnose(&manifest);
    let text = report.lines.join("\n");

    assert!(!report.drifted, "expected no drift: {text}");
    assert!(!report.failed, "expected no failure: {text}");
    assert!(text.contains("in sync: AGENTS.md"), "{text}");
    assert!(text.contains("in sync: memory"), "{text}");
    assert!(
        report.lines.contains(&TOOLS_LINE.to_string()),
        "the no-external-tools line must always be present: {text}"
    );
}

// ---------------------------------------------------------------------------
// A drifted copy: the source changed after the copy was materialized.
// ---------------------------------------------------------------------------

#[test]
fn reports_drift_for_a_copy_whose_source_has_changed_since() {
    let env = Env::new();
    env.write_source("AGENTS.md", b"version one");
    let manifest = env.write_manifest("copy = [\"AGENTS.md\"]");
    env.apply(&manifest);

    // The destination still holds "version one"; the source has since moved
    // on. `doctor` must not need a fresh `apply` to notice.
    env.write_source("AGENTS.md", b"version two");

    let report = env.diagnose(&manifest);
    let text = report.lines.join("\n");

    assert!(report.drifted, "a changed source must be reported: {text}");
    assert!(!report.failed, "{text}");
    assert!(text.contains("drift: AGENTS.md"), "{text}");
    assert!(
        report.lines.contains(&TOOLS_LINE.to_string()),
        "the no-external-tools line must always be present, drift or not: {text}"
    );
}

// ---------------------------------------------------------------------------
// A drifted link of the wrong kind: a directory link sits where a file
// symlink is declared.
// ---------------------------------------------------------------------------

#[test]
fn reports_drift_for_a_link_of_the_wrong_kind() {
    let env = Env::new();
    // A *file* source, so the declared entry resolves to `LinkOp::FileSymlink`.
    env.write_source("note.md", b"a single file");
    let manifest = env.write_manifest("link = [\"note.md\"]");

    // Never materialized via `apply` at all: a *directory* link already
    // occupies the destination, standing in for a stray directory link left
    // over from some earlier, different manifest.
    let elsewhere = env.root().join("elsewhere");
    fs::create_dir(&elsewhere).expect("create elsewhere dir");
    safelink::create::create_dir_link(&elsewhere, &env.dest().join("note.md"))
        .expect("create wrong-kind link");

    let report = env.diagnose(&manifest);
    let text = report.lines.join("\n");

    assert!(
        report.drifted,
        "a link of the wrong kind must be reported as drift: {text}"
    );
    assert!(!report.failed, "{text}");
    assert!(text.contains("drift: note.md"), "{text}");
}

// ---------------------------------------------------------------------------
// A dangling link: the on-disk link's recorded target has been removed,
// while the manifest's *declared* source is untouched and still exists.
//
// Both platforms catch this the same way, through check 3
// (`report_plan_drift`), not check 4: POSIX's `classify` already folds a
// dangling directory symlink into `FileLink`, and a real `windows-latest`
// CI run of this branch's PR confirmed a dangling junction resolves to
// `Decision::Write` (check 3's generic message) too, not `Decision::Same`
// (which would have routed it to check 4's dedicated dangling wording
// instead) — see `doctor::report_link_targets`'s own doc comment for the
// full account, including the earlier, disproven assumption this comment
// used to encode. So this test asserts only the platform-independent
// contract: drift is reported, with a remedy naming the target and telling
// the user to re-run `apply`.
// ---------------------------------------------------------------------------

#[test]
fn reports_a_dangling_link_as_drift() {
    let env = Env::new();
    env.write_source("memory/note.md", b"remembered");
    let manifest = env.write_manifest("link = [\"memory\"]");
    env.apply(&manifest);

    // Replace the correctly materialized link with one pointing somewhere
    // else entirely, then remove that somewhere-else — simulating a link
    // left over from a manifest edit whose old target has since vanished,
    // while `memory` (the *currently* declared source) is never touched.
    safelink::remove::remove_link(&env.dest().join("memory")).expect("remove real link");
    let stale_target = env.root().join("stale_target");
    fs::create_dir(&stale_target).expect("create stale target");
    safelink::create::create_dir_link(&stale_target, &env.dest().join("memory"))
        .expect("create stale link");
    fs::remove_dir_all(&stale_target).expect("remove stale target");

    let report = env.diagnose(&manifest);
    let text = report.lines.join("\n");

    assert!(report.drifted, "a dangling link must be reported: {text}");
    assert!(!report.failed, "{text}");
    assert!(text.contains("memory"), "{text}");
    assert!(
        text.contains("re-run `apply`"),
        "the remedy must say to re-run apply: {text}"
    );
}

// ---------------------------------------------------------------------------
// A link with the wrong target: the on-disk link points at a real, still-
// existing directory that is not the one currently declared. This is the
// actual `is_same()` gap, and is reachable identically on every platform —
// this is also the test that asserts on remedy text.
// ---------------------------------------------------------------------------

#[test]
fn reports_a_link_with_the_wrong_target_as_drift_and_names_the_remedy() {
    let env = Env::new();
    env.write_source("current/note.md", b"the current source");
    let manifest = env.write_manifest("link = [\"current\"]");
    env.apply(&manifest);

    // A real, still-existing directory that is simply not what the manifest
    // currently declares for this entry — standing in for a source that was
    // renamed or re-pointed in a manifest edit, without `apply` re-running.
    let old_source = env.root().join("old_source");
    fs::create_dir(&old_source).expect("create old source");
    fs::write(old_source.join("stale.md"), b"old content").expect("write old content");

    safelink::remove::remove_link(&env.dest().join("current")).expect("remove real link");
    safelink::create::create_dir_link(&old_source, &env.dest().join("current"))
        .expect("create wrong-target link");

    let report = env.diagnose(&manifest);
    let text = report.lines.join("\n");

    assert!(
        report.drifted,
        "a link pointing at the wrong (but real) target must be drift: {text}"
    );
    assert!(!report.failed, "{text}");
    assert!(text.contains("current"), "{text}");
    assert!(
        text.contains("Remove the link, then re-run `apply`"),
        "the remedy must say to remove the link and re-run apply, not merely \
         \"re-run apply\" (which would do nothing -- apply still treats this \
         link as `Same`): {text}"
    );
}

// ---------------------------------------------------------------------------
// A linked backup root: `.agrafter-backups` itself is a link.
// ---------------------------------------------------------------------------

#[test]
fn reports_a_linked_backup_root_as_drift() {
    let env = Env::new();
    let manifest = env.write_manifest("");

    let elsewhere = env.root().join("elsewhere");
    fs::create_dir(&elsewhere).expect("create elsewhere dir");
    safelink::create::create_dir_link(&elsewhere, &env.dest().join(".agrafter-backups"))
        .expect("create linked backup root");

    let report = env.diagnose(&manifest);
    let text = report.lines.join("\n");

    assert!(
        report.drifted,
        "a linked backup root must be reported as drift: {text}"
    );
    assert!(!report.failed, "{text}");
    assert!(text.contains(".agrafter-backups"), "{text}");
    assert!(
        text.to_lowercase().contains("remove or rename"),
        "the remedy must name removing or renaming what occupies it: {text}"
    );
}

// ---------------------------------------------------------------------------
// `PruneDecision::LeftInPlace` now has two producers and check 3 must tell
// them apart rather than treating both as the deliberate, non-drift outcome.
// ---------------------------------------------------------------------------

#[test]
fn reports_a_stale_file_blocked_by_a_linked_backup_root_as_drift() {
    // The tool leaves a stale file in place, rather than pruning it, when its
    // backup could only be filed through a linked backup root. That is genuinely
    // something to fix — unlike the other `LeftInPlace` producer below — and
    // check 3 must say so, not silently drop it.
    let env = Env::new();
    env.write_source("modes/one.md", b"first mode");
    let manifest = env.write_manifest("copy = [\"modes\"]");
    env.apply(&manifest);
    fs::remove_file(env.source().join("modes/one.md")).expect("remove source file");

    let outside = env.root().join("outside-backups");
    fs::create_dir(&outside).expect("create outside dir");
    safelink::create::create_dir_link(&outside, &env.dest().join(".agrafter-backups"))
        .expect("create linked backup root");

    let report = env.diagnose(&manifest);
    let text = report.lines.join("\n");

    assert!(
        report.drifted,
        "a stale file that apply could not safely remove must be reported \
         as drift, not silently dropped: {text}"
    );
    assert!(!report.failed, "{text}");
    assert!(
        text.contains("modes/one.md") || text.contains("modes\\one.md"),
        "the specific stale file must be named: {text}"
    );
    assert!(
        text.to_lowercase().contains("remove or rename"),
        "the remedy must name removing or renaming what occupies the \
         backup root, the same remedy check 5 gives: {text}"
    );
}

#[test]
fn does_not_report_a_link_found_where_a_prune_candidate_was_expected_as_drift() {
    // The original `LeftInPlace` producer: a link sitting where a prune
    // candidate would otherwise be. `apply` deliberately leaves it alone,
    // and this remains a distinct, non-drift outcome — the fix above must not
    // widen check 3 to flag this too.
    let env = Env::new();
    env.write_source("modes/one.md", b"first mode");
    let manifest = env.write_manifest("copy = [\"modes\"]");
    env.apply(&manifest);

    let elsewhere = env.root().join("elsewhere");
    fs::create_dir(&elsewhere).expect("create elsewhere dir");
    safelink::create::create_dir_link(&elsewhere, &env.dest().join("modes/linked-dir"))
        .expect("create link inside the declared directory's destination subtree");

    let report = env.diagnose(&manifest);
    let text = report.lines.join("\n");

    assert!(
        !report.drifted,
        "a link left in place where a prune candidate was expected remains \
         a deliberate outcome, not drift: {text}"
    );
    assert!(!report.failed, "{text}");
}

// ---------------------------------------------------------------------------
// CLI-level: a missing manifest exits 2, the same as `apply`/`status`.
// `cli::doctor` is not `pub`, so this is the one case that needs a process.
// ---------------------------------------------------------------------------

fn spawn(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_agent-grafter"))
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("spawn agent-grafter")
}

#[test]
fn doctor_exits_2_for_a_missing_manifest() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let output = spawn(dir.path(), &["doctor"]);
    assert_eq!(
        output.status.code(),
        Some(2),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
