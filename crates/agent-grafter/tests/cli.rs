//! Black-box CLI tests: spawn the built `agent-grafter` binary and observe
//! its exit code, stdout, and effect (or lack of one) on the filesystem.
//!
//! `TestEnv` below is test infrastructure only, not production code.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A temp-directory sandbox with a manifest file (`agent-grafter.toml`,
/// the CLI's default) already written, that the binary under test is run
/// inside of.
struct TestEnv {
    dir: tempfile::TempDir,
}

impl TestEnv {
    /// Create a temp dir and write `toml` as its manifest.
    fn with_manifest(toml: &str) -> Self {
        let dir = tempfile::tempdir().expect("create temp dir");
        fs::write(dir.path().join("agent-grafter.toml"), toml).expect("write manifest");
        TestEnv { dir }
    }

    /// Overwrite this env's manifest. Used when the manifest content
    /// depends on a path only known after the env's temp dir exists (e.g.
    /// an absolute destination path).
    fn write_manifest(&self, toml: &str) {
        fs::write(self.dir.path().join("agent-grafter.toml"), toml).expect("write manifest");
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    /// A deterministic snapshot of the temp dir's file tree: relative path,
    /// entry kind, and (for files) content — enough to prove `--dry-run`
    /// changed nothing. Deliberately excludes timestamps, which would make
    /// the equality assertion flaky for reasons unrelated to dry-run.
    fn snapshot(&self) -> Snapshot {
        let mut entries = BTreeMap::new();
        walk(self.dir.path(), self.dir.path(), &mut entries);
        Snapshot(entries)
    }

    /// Run the built `agent-grafter` binary with `args`, inside this env's
    /// temp dir, and capture the result.
    fn run(&self, args: &[&str]) -> RunResult {
        let output = Command::new(env!("CARGO_BIN_EXE_agent-grafter"))
            .args(args)
            .current_dir(self.dir.path())
            .output()
            .expect("spawn agent-grafter");
        RunResult { output }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Snapshot(BTreeMap<PathBuf, EntryKind>);

#[derive(Debug, PartialEq, Eq)]
enum EntryKind {
    File(Vec<u8>),
    Dir,
    Symlink(PathBuf),
}

fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, EntryKind>) {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .expect("read_dir")
        .map(|e| e.expect("dir entry"))
        .collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let path = entry.path();
        let rel = path.strip_prefix(root).expect("under root").to_path_buf();
        let meta = fs::symlink_metadata(&path).expect("symlink_metadata");

        let kind = if meta.file_type().is_symlink() {
            EntryKind::Symlink(fs::read_link(&path).unwrap_or_default())
        } else if meta.is_dir() {
            walk(root, &path, out);
            EntryKind::Dir
        } else {
            EntryKind::File(fs::read(&path).unwrap_or_default())
        };
        out.insert(rel, kind);
    }
}

struct RunResult {
    output: Output,
}

impl RunResult {
    fn assert_success(&self) -> &Self {
        assert!(
            self.output.status.success(),
            "expected exit 0, got {:?}\nstdout:\n{}\nstderr:\n{}",
            self.output.status.code(),
            self.stdout(),
            self.stderr()
        );
        self
    }

    fn assert_exit_code(&self, code: i32) -> &Self {
        assert_eq!(
            self.output.status.code(),
            Some(code),
            "stdout:\n{}\nstderr:\n{}",
            self.stdout(),
            self.stderr()
        );
        self
    }

    fn stdout(&self) -> String {
        String::from_utf8_lossy(&self.output.stdout).into_owned()
    }

    fn stderr(&self) -> String {
        String::from_utf8_lossy(&self.output.stderr).into_owned()
    }
}

/// A manifest with one declared link entry ("memory") and one target
/// ("claude") whose `dest` is some placeholder that need not exist —
/// sufficient for `apply`'s read-only reporting, which never touches it.
const ONE_TARGET_MANIFEST: &str =
    "link = [\"memory\"]\n\n[targets.claude]\ndest = \"placeholder\"\n";

#[test]
fn dry_run_changes_nothing_on_disk() {
    let env = TestEnv::with_manifest(ONE_TARGET_MANIFEST);
    let before = env.snapshot();
    env.run(&["apply", "--dry-run"]).assert_success();
    assert_eq!(
        before,
        env.snapshot(),
        "--dry-run must not touch the filesystem"
    );
}

#[test]
fn dry_run_still_reports_the_work_it_would_do() {
    let env = TestEnv::with_manifest(ONE_TARGET_MANIFEST);
    let out = env.run(&["apply", "--dry-run"]).stdout();
    assert!(
        out.contains("memory"),
        "dry-run must name what it would act on"
    );
    assert!(
        out.contains("claude"),
        "dry-run must name which target it would act on"
    );
}

#[test]
fn status_exits_3_when_a_declared_link_has_drifted() {
    // The target's dest is a real directory (the env's own temp dir), but
    // nothing exists at dest.join("memory") — that is drift.
    let env = TestEnv::with_manifest("link = [\"memory\"]");
    env.write_manifest(&format!(
        "link = [\"memory\"]\n\n[targets.claude]\ndest = '{}'\n",
        env.path().display()
    ));
    env.run(&["status"]).assert_exit_code(3);
}

#[test]
fn status_exits_3_when_no_targets_are_declared_at_all() {
    // A declared `link` entry with nowhere to check it against is drift,
    // not a silent "in sync" — there is no destination anywhere that could
    // be correctly materialized.
    let env = TestEnv::with_manifest("link = [\"memory\"]");
    env.run(&["status"]).assert_exit_code(3);
}

#[test]
fn dry_run_still_reports_the_work_it_would_do_with_no_targets_declared() {
    // Constraint 3: `--dry-run` must stay honest about what it would act on
    // even when the manifest declares no `[targets.*]` at all — the listing
    // must not silently disappear.
    let env = TestEnv::with_manifest("link = [\"memory\"]");
    let out = env.run(&["apply", "--dry-run"]).stdout();
    assert!(
        out.contains("memory"),
        "dry-run must name what it would act on even with no targets declared"
    );
}

#[test]
fn status_exits_0_when_the_declared_link_is_already_in_place() {
    let env = TestEnv::with_manifest("link = [\"memory\"]");

    // Build a real directory link with the engine itself (never reimplement
    // link creation here — Global Constraint 5) and point a `[targets.*]`
    // entry's `dest` at its parent, so `status` resolves
    // `dest.join("memory")` to the link and finds it in sync. Use the
    // absolute path, as a TOML *literal* (single-quoted) string so a
    // Windows path's backslashes are not read as escapes.
    let source_dir = env.path().join("source");
    fs::create_dir(&source_dir).expect("create source dir");
    let dest_root = env.path().join("dest_root");
    fs::create_dir(&dest_root).expect("create dest root");
    let link = dest_root.join("memory");
    safelink::create::create_dir_link(&source_dir, &link).expect("create link");

    env.write_manifest(&format!(
        "link = [\"memory\"]\n\n[targets.claude]\ndest = '{}'\n",
        dest_root.display()
    ));

    env.run(&["status"]).assert_exit_code(0);
}

#[test]
fn plain_apply_does_not_claim_to_have_completed_a_materialization() {
    // S2 does not implement real materialization (that's S3's job), so
    // plain `apply` must not print output indistinguishable from a
    // completed apply, and must not touch the filesystem either — there is
    // nothing yet that could honestly do so.
    let env = TestEnv::with_manifest(ONE_TARGET_MANIFEST);
    let before = env.snapshot();

    let dry_run_out = env.run(&["apply", "--dry-run"]).stdout();
    let plain_out = env.run(&["apply"]).assert_success().stdout();

    assert_ne!(
        dry_run_out, plain_out,
        "plain apply and --dry-run must not print identical output"
    );
    assert!(
        dry_run_out.contains("would"),
        "dry-run output should read as hypothetical, got: {dry_run_out}"
    );
    assert_eq!(
        before,
        env.snapshot(),
        "plain apply must not touch the filesystem either, since S2 has no materialization to do"
    );
}
