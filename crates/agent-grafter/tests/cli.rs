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

    /// Lay out an env that `apply` can really act on: a `source/` directory
    /// holding a declared `memory/` directory, and a real target root. Since
    /// Task S3a, `apply` refuses a target root that does not exist and a
    /// source that does not exist, so a fixture that omits either is only
    /// ever testing the refusal path.
    fn with_real_source_and_target(body: &str) -> Self {
        let env = TestEnv::with_manifest("");
        fs::create_dir_all(env.path().join("source/memory")).expect("create source");
        fs::write(env.path().join("source/memory/note.md"), b"remembered").expect("write source");
        fs::create_dir(env.dest_root()).expect("create dest root");
        env.write_manifest(&format!(
            "source = \"source\"\n{body}\n\n[targets.example]\ndest = '{}'\n",
            env.dest_root().display()
        ));
        env
    }

    fn dest_root(&self) -> PathBuf {
        self.dir.path().join("dest_root")
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

#[test]
fn dry_run_changes_nothing_on_disk() {
    // The fixture has real work to do — a real source and a real target root
    // — so "nothing changed" is evidence about `--dry-run` and not merely a
    // restatement of "there was nothing to change".
    let env = TestEnv::with_real_source_and_target("link = [\"memory\"]");
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
    let env = TestEnv::with_real_source_and_target("link = [\"memory\"]");
    let out = env.run(&["apply", "--dry-run"]).stdout();
    assert!(
        out.contains("memory"),
        "dry-run must name what it would act on"
    );
    assert!(
        out.contains("example"),
        "dry-run must name which target it would act on"
    );
    assert!(
        out.contains("would write"),
        "dry-run must say what it would do, got: {out}"
    );
}

#[test]
fn status_exits_3_when_a_declared_link_has_drifted() {
    // The target's dest is a real directory (the env's own temp dir), but
    // nothing exists at dest.join("memory") — that is drift.
    let env = TestEnv::with_manifest("link = [\"memory\"]");
    env.write_manifest(&format!(
        "link = [\"memory\"]\n\n[targets.example]\ndest = '{}'\n",
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
        "link = [\"memory\"]\n\n[targets.example]\ndest = '{}'\n",
        dest_root.display()
    ));

    env.run(&["status"]).assert_exit_code(0);
}

#[test]
fn a_dry_run_and_a_real_run_differ_in_verb_and_in_effect() {
    // Since Task S3a there is real materialization to do, so the honest
    // distinction is no longer "neither run does anything": it is that both
    // runs decide the same thing, only one of them acts, and the output says
    // which happened.
    let env = TestEnv::with_real_source_and_target("link = [\"memory\"]");
    let before = env.snapshot();

    let dry_run_out = env.run(&["apply", "--dry-run"]).assert_success().stdout();
    assert!(
        dry_run_out.contains("would write"),
        "dry-run output should read as hypothetical, got: {dry_run_out}"
    );
    assert_eq!(
        before,
        env.snapshot(),
        "--dry-run must not touch the filesystem"
    );

    let plain_out = env.run(&["apply"]).assert_success().stdout();
    assert_ne!(
        dry_run_out, plain_out,
        "plain apply and --dry-run must not print identical output"
    );
    assert!(
        plain_out.contains("wrote") && !plain_out.contains("would"),
        "a real run reports what it did, not what it would do: {plain_out}"
    );
    assert_ne!(
        before,
        env.snapshot(),
        "a real apply must actually materialize something"
    );
    assert_eq!(
        fs::read(env.dest_root().join("memory/note.md")).unwrap(),
        b"remembered",
        "the materialized link must be traversable"
    );
}

#[test]
fn a_dry_run_reports_the_same_exit_code_a_real_run_would() {
    // A CI gate that runs `apply --dry-run` to detect refusals must see the
    // same code the real run would produce, or dry-run is hiding the one
    // thing the gate exists to catch.
    let env = TestEnv::with_real_source_and_target("link = [\"memory\"]");
    let occupied = env.dest_root().join("memory");
    fs::create_dir(&occupied).expect("create occupying dir");
    fs::write(occupied.join("hand-written.md"), b"mine").expect("write");

    env.run(&["apply", "--dry-run"]).assert_exit_code(4);
    env.run(&["apply"]).assert_exit_code(4);
    assert_eq!(
        fs::read(occupied.join("hand-written.md")).unwrap(),
        b"mine",
        "neither run may touch the occupying directory"
    );
}
