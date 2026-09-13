//! Tests for `apply`'s plan/execute split: real materialization, idempotent
//! re-runs, backup-before-replace, and per-entry refusals.
//!
//! Most of these call `agent_grafter::apply::{plan, execute}` directly rather
//! than spawning the binary. That is deliberate: the `FileSymlink -> FileCopy`
//! downgrade is only reliably reachable by injecting `ForcedProbe(false)`, and
//! a probe cannot be injected across a process boundary. Adding an environment
//! variable to force it would put a testing hook in production code, so the
//! library surface is tested as a library. The three cases that genuinely need
//! a process — the exit code contract, and resolving `source` relative to the
//! manifest rather than the working directory — spawn the binary.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use agent_grafter::apply::{self, Report};
use agent_grafter::manifest::{self, Manifest};
use safelink::capability::{ForcedProbe, RealProbe, SymlinkProbe};

/// A temp-dir sandbox laid out the way a real install is: a `source/`
/// directory holding the canonical files, a `dest/` directory standing in for
/// a target root like an agent's own config directory, and a manifest file
/// between them.
struct Env {
    dir: tempfile::TempDir,
}

impl Env {
    /// Create the sandbox with both `source/` and `dest/` already existing.
    /// `dest/` exists because Ruling 8 refuses a target root that does not —
    /// the test that exercises that refusal removes it explicitly.
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

    /// Write a file (creating parents) under `source/`.
    fn write_source(&self, rel: &str, bytes: &[u8]) {
        let path = self.source().join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create source parent");
        }
        fs::write(path, bytes).expect("write source file");
    }

    /// Write a manifest declaring `source = "source"` and a single
    /// `[targets.example]` pointing at this env's `dest/`, with `body`
    /// supplying the `copy`/`link` lines. The destination is spelled as a
    /// TOML *literal* string so a Windows path's backslashes are not read as
    /// escapes.
    fn write_manifest(&self, body: &str) -> Manifest {
        let toml = format!(
            "source = \"source\"\n{body}\n\n[targets.example]\ndest = '{}'\n",
            self.dest().display()
        );
        fs::write(self.manifest_path(), &toml).expect("write manifest");
        manifest::parse(&toml).expect("parse manifest")
    }

    /// Plan and execute in one step, the way `cli::apply` does for a real run.
    fn apply(&self, manifest: &Manifest, probe: &dyn SymlinkProbe) -> Report {
        let plan = apply::plan(manifest, self.root(), probe);
        apply::execute(&plan, probe)
    }
}

/// A deterministic snapshot of a directory tree: relative path, kind, and
/// (for files) content. Used to prove an idempotent re-run changed nothing.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Kind> {
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

#[derive(Debug, PartialEq, Eq)]
enum Kind {
    File(Vec<u8>),
    Dir,
    Link(PathBuf),
}

fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Kind>) {
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
            Kind::Link(fs::read_link(&path).unwrap_or_default())
        } else if meta.is_dir() {
            walk(root, &path, out);
            Kind::Dir
        } else {
            Kind::File(fs::read(&path).unwrap_or_default())
        };
        out.insert(rel, kind);
    }
}

/// Run the built binary with `args`, from `cwd`.
fn spawn(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_agent-grafter"))
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("spawn agent-grafter")
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

// ---------------------------------------------------------------------------
// 1. The plan's own test, adapted per Ruling 9: a real target root and a real
//    source file behind the declared entry.
// ---------------------------------------------------------------------------

#[test]
fn a_second_apply_with_no_changes_writes_nothing() {
    let env = Env::new();
    env.write_source("AGENTS.md", b"canonical contents");
    let manifest = env.write_manifest("copy = [\"AGENTS.md\"]");

    let first = env.apply(&manifest, &RealProbe);
    assert!(
        first.text().contains("wrote"),
        "the first apply must actually write: {}",
        first.text()
    );

    let after_first = snapshot(&env.dest());
    let out = env.apply(&manifest, &RealProbe).text();

    assert_eq!(
        after_first,
        snapshot(&env.dest()),
        "idempotent re-run must not rewrite"
    );
    assert!(out.contains("same"), "second run must report same: {out}");
    assert!(
        !out.contains("wrote"),
        "second run must not report a write: {out}"
    );
}

// ---------------------------------------------------------------------------
// 2. A file not previously at `dest` is written and reported.
// ---------------------------------------------------------------------------

#[test]
fn a_new_file_is_written_and_reported() {
    let env = Env::new();
    env.write_source("AGENTS.md", b"hello");
    let manifest = env.write_manifest("copy = [\"AGENTS.md\"]");

    let out = env.apply(&manifest, &RealProbe);

    assert!(!out.refused, "nothing here is refusable: {}", out.text());
    assert!(!out.failed, "nothing here should fail: {}", out.text());
    assert!(
        out.text().contains("wrote: AGENTS.md"),
        "expected a write line, got: {}",
        out.text()
    );
    assert_eq!(fs::read(env.dest().join("AGENTS.md")).unwrap(), b"hello");
}

// ---------------------------------------------------------------------------
// 3. A changed source overwrites `dest`, after backing the old content up.
// ---------------------------------------------------------------------------

#[test]
fn a_changed_source_overwrites_dest_after_backing_the_old_content_up() {
    let env = Env::new();
    env.write_source("AGENTS.md", b"version one");
    let manifest = env.write_manifest("copy = [\"AGENTS.md\"]");
    env.apply(&manifest, &RealProbe);
    assert_eq!(
        fs::read(env.dest().join("AGENTS.md")).unwrap(),
        b"version one"
    );

    env.write_source("AGENTS.md", b"version two");
    let plan = apply::plan(&manifest, env.root(), &RealProbe);
    let timestamp = plan.run_timestamp.clone();
    let out = apply::execute(&plan, &RealProbe);

    assert!(
        out.text().contains("wrote: AGENTS.md"),
        "a changed source must be reported as written, not same: {}",
        out.text()
    );
    assert_eq!(
        fs::read(env.dest().join("AGENTS.md")).unwrap(),
        b"version two",
        "destination must hold the new content"
    );

    let backup = env
        .dest()
        .join(".agrafter-backups")
        .join(&timestamp)
        .join("AGENTS.md");
    assert_eq!(
        fs::read(&backup).unwrap_or_else(|e| panic!("no backup at {}: {e}", backup.display())),
        b"version one",
        "the replaced content must survive in the backup"
    );
    assert!(
        timestamp.len() == 16 && timestamp.ends_with('Z') && timestamp.contains('T'),
        "backup timestamp must be the lexically sortable YYYYMMDDTHHMMSSZ form, got {timestamp:?}"
    );
}

// ---------------------------------------------------------------------------
// 4. A `copy` entry naming a directory expands to one copy per file inside.
// ---------------------------------------------------------------------------

#[test]
fn a_copy_entry_naming_a_directory_expands_to_one_copy_per_file() {
    let env = Env::new();
    env.write_source("modes/one.md", b"first mode");
    env.write_source("modes/nested/two.md", b"second mode");
    let manifest = env.write_manifest("copy = [\"modes\"]");

    let out = env.apply(&manifest, &RealProbe);

    assert!(!out.refused && !out.failed, "unexpected: {}", out.text());
    assert_eq!(
        fs::read(env.dest().join("modes/one.md")).unwrap(),
        b"first mode"
    );
    assert_eq!(
        fs::read(env.dest().join("modes/nested/two.md")).unwrap(),
        b"second mode"
    );
    // Each expanded file is reported on its own line, not collapsed into one
    // line for the whole directory.
    let text = out.text();
    assert!(text.contains("one.md"), "{text}");
    assert!(text.contains("two.md"), "{text}");
    // A directory copy must produce real files, never a link to the source.
    assert!(
        !fs::symlink_metadata(env.dest().join("modes"))
            .unwrap()
            .file_type()
            .is_symlink(),
        "a `copy` entry must not become a link"
    );
}

// ---------------------------------------------------------------------------
// 5. A `link` entry naming a directory produces a real, traversable link.
// ---------------------------------------------------------------------------

#[test]
fn a_link_entry_produces_a_traversable_directory_link() {
    let env = Env::new();
    env.write_source("memory/note.md", b"remembered");
    let manifest = env.write_manifest("link = [\"memory\"]");

    let out = env.apply(&manifest, &RealProbe);
    assert!(!out.refused && !out.failed, "unexpected: {}", out.text());

    // Belt: the engine agrees it is a directory link.
    assert_eq!(
        safelink::classify::classify(&env.dest().join("memory")).unwrap(),
        safelink::classify::LinkClass::DirLink
    );
    // Braces: reading *through* the link sees the source's contents, and a
    // write through the link lands in the source — which a copy would not do.
    assert_eq!(
        fs::read(env.dest().join("memory/note.md")).unwrap(),
        b"remembered"
    );
    fs::write(env.dest().join("memory/added.md"), b"through the link").unwrap();
    assert_eq!(
        fs::read(env.source().join("memory/added.md")).unwrap(),
        b"through the link",
        "a real link must write through to the source"
    );
}

// ---------------------------------------------------------------------------
// 6. The tricky one (Ruling 4 + Ruling 6): a declared `FileSymlink` that the
//    engine downgrades to a copy leaves an ordinary file at `dest`, which
//    `classify()` cannot tell apart from "never materialized". The second run
//    must still report `same`.
// ---------------------------------------------------------------------------

#[test]
fn a_downgraded_file_symlink_is_recognized_as_same_on_the_second_run() {
    let env = Env::new();
    env.write_source("AGENTS.md", b"canonical contents");
    let manifest = env.write_manifest("link = [\"AGENTS.md\"]");

    // ForcedProbe(false) reaches the copy fallback on any host, including an
    // elevated CI runner where RealProbe would answer `true`.
    let first = env.apply(&manifest, &ForcedProbe(false)).text();
    assert!(first.contains("wrote"), "first run must write: {first}");

    let dest = env.dest().join("AGENTS.md");
    assert_eq!(
        safelink::classify::classify(&dest).unwrap(),
        safelink::classify::LinkClass::NotALink,
        "the downgrade must have produced an ordinary file, or this test is \
         not exercising the case it claims to"
    );
    assert_eq!(fs::read(&dest).unwrap(), b"canonical contents");

    let after_first = snapshot(&env.dest());
    let second = env.apply(&manifest, &ForcedProbe(false)).text();

    assert!(
        second.contains("same"),
        "a downgraded copy that still matches its source is `same`: {second}"
    );
    assert!(
        !second.contains("wrote"),
        "a downgraded copy must not be rewritten every run: {second}"
    );
    assert_eq!(
        after_first,
        snapshot(&env.dest()),
        "the second run must not have touched anything"
    );
}

// ---------------------------------------------------------------------------
// 7. Ruling 10: a refusal is per-entry, not a whole-run abort, and it sets the
//    run's exit code to 4.
// ---------------------------------------------------------------------------

#[test]
fn an_occupied_directory_destination_is_refused_without_stopping_other_entries() {
    let env = Env::new();
    env.write_source("memory/note.md", b"canonical");
    env.write_source("AGENTS.md", b"also declared");
    env.write_manifest("link = [\"memory\"]\ncopy = [\"AGENTS.md\"]");

    // A real, non-empty directory squatting where the directory link goes.
    let occupied = env.dest().join("memory");
    fs::create_dir(&occupied).unwrap();
    fs::write(occupied.join("hand-written.md"), b"not ours to delete").unwrap();

    let output = spawn(env.root(), &["apply"]);
    let text = stdout_of(&output);

    assert_eq!(
        output.status.code(),
        Some(4),
        "a refused entry must set the run's exit code.\nstdout:\n{text}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        text.contains("refused") && text.contains("memory"),
        "the refusal must name the entry: {text}"
    );
    // Nothing was deleted or merged.
    assert_eq!(
        fs::read(occupied.join("hand-written.md")).unwrap(),
        b"not ours to delete"
    );
    assert_eq!(
        fs::read_dir(&occupied).unwrap().count(),
        1,
        "the occupied directory must not have been merged into"
    );
    // And the *other* declared entry was still processed.
    assert!(
        text.contains("wrote") && text.contains("AGENTS.md"),
        "a refusal must not abort the rest of the run: {text}"
    );
    assert_eq!(
        fs::read(env.dest().join("AGENTS.md")).unwrap(),
        b"also declared"
    );
}

// ---------------------------------------------------------------------------
// 8. Ruling 8: a target root that does not exist is refused outright, rather
//    than silently built from a typo.
// ---------------------------------------------------------------------------

#[test]
fn a_target_root_that_does_not_exist_is_refused_without_creating_anything() {
    let env = Env::new();
    env.write_source("AGENTS.md", b"hello");
    let toml = format!(
        "source = \"source\"\ncopy = [\"AGENTS.md\"]\n\n[targets.example]\ndest = '{}'\n",
        env.root().join("typo-dest").display()
    );
    fs::write(env.manifest_path(), &toml).unwrap();

    let output = spawn(env.root(), &["apply"]);
    let text = stdout_of(&output);

    assert_eq!(
        output.status.code(),
        Some(4),
        "a missing target root is a refusal.\nstdout:\n{text}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        text.contains("refused") && text.contains("typo-dest"),
        "the refusal must name the missing target root: {text}"
    );
    assert!(
        !env.root().join("typo-dest").exists(),
        "a missing target root must not be created"
    );
}

// ---------------------------------------------------------------------------
// 9. Ruling 9's second half: `source` resolves relative to the manifest file,
//    not the process's working directory.
// ---------------------------------------------------------------------------

#[test]
fn source_resolves_relative_to_the_manifest_not_the_working_directory() {
    let env = Env::new();
    env.write_source("AGENTS.md", b"found from elsewhere");
    env.write_manifest("copy = [\"AGENTS.md\"]");

    // A completely unrelated working directory, with no `source/` in it.
    let elsewhere = tempfile::tempdir().unwrap();
    let manifest_arg = env.manifest_path();
    let output = spawn(
        elsewhere.path(),
        &["apply", "--manifest", manifest_arg.to_str().unwrap()],
    );
    let text = stdout_of(&output);

    assert_eq!(
        output.status.code(),
        Some(0),
        "source must be found relative to the manifest.\nstdout:\n{text}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(env.dest().join("AGENTS.md")).unwrap(),
        b"found from elsewhere"
    );
}

// ---------------------------------------------------------------------------
// Dry-run honesty (Constraint 3 / Ruling 3): the same plan drives both modes.
// ---------------------------------------------------------------------------

#[test]
fn dry_run_computes_the_same_plan_but_changes_nothing() {
    let env = Env::new();
    env.write_source("AGENTS.md", b"hello");
    let manifest = env.write_manifest("copy = [\"AGENTS.md\"]");

    let before = snapshot(&env.dest());
    let plan = apply::plan(&manifest, env.root(), &RealProbe);
    let described = plan.describe();

    assert_eq!(before, snapshot(&env.dest()), "planning must not mutate");
    assert!(
        described.text().contains("would write: AGENTS.md"),
        "dry-run must say what it would do: {}",
        described.text()
    );

    // The same plan, executed, reports the same decisions in the same order.
    let executed = apply::execute(&plan, &RealProbe);
    assert_eq!(described.lines.len(), executed.lines.len());
    assert_eq!(described.refused, executed.refused);
    assert!(executed.text().contains("wrote: AGENTS.md"));
}

// ---------------------------------------------------------------------------
// Two declared entries resolving to one destination. Running both would let
// the second entry back up what the first had just written, destroying the
// only copy of the user's original content — while both lines claim a backup
// was taken. Refused at plan time, before anything is written.
// ---------------------------------------------------------------------------

#[test]
fn two_entries_claiming_one_destination_are_refused_before_anything_is_written() {
    let env = Env::new();
    env.write_source("modes/one.md", b"NEW");
    // A second file under the same declared directory, claimed by nobody else.
    env.write_source("modes/two.md", b"uncontested");
    // `modes` expands to include `modes/one.md`, which is also declared in its
    // own right — so two planned entries resolve to `dest/modes/one.md`.
    env.write_manifest("copy = [\"modes\", \"modes/one.md\"]");

    // The user's own file, already sitting at the contested destination.
    let contested = env.dest().join("modes/one.md");
    fs::create_dir_all(contested.parent().unwrap()).unwrap();
    fs::write(&contested, b"USER").unwrap();

    let output = spawn(env.root(), &["apply"]);
    let text = stdout_of(&output);

    assert_eq!(
        output.status.code(),
        Some(4),
        "a destination conflict is a refusal.\nstdout:\n{text}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        text.contains("refused") && text.contains("claimed by more than one declared entry"),
        "the refusal must explain the conflict: {text}"
    );
    assert!(
        text.contains("\"modes\"") && text.contains("\"modes/one.md\""),
        "the refusal must name both declared entries so the user knows what to \
         edit — they share a label, so only the declared names distinguish \
         them: {text}"
    );

    // The point of the whole fix: the user's bytes are still exactly there.
    // Not "eventually correct", not restored from a backup — never touched.
    assert_eq!(
        fs::read(&contested).unwrap(),
        b"USER",
        "the contested file must not have been overwritten at all"
    );
    assert!(
        !env.dest().join(".agrafter-backups").exists(),
        "nothing was written, so no backup should have been taken either"
    );

    // A refusal is still per-entry (Ruling 10): the uncontested sibling from
    // the same declared directory was processed normally.
    assert_eq!(
        fs::read(env.dest().join("modes/two.md")).unwrap(),
        b"uncontested",
        "only the conflicting entries are refused, not the whole directory"
    );
}

#[test]
fn a_destination_conflict_is_decided_at_plan_time_not_during_execution() {
    // The companion to the test above, at the library level: `execute` must
    // never be handed two `Write` decisions for one path, so the refusal has
    // to be visible in the plan itself — including under `--dry-run`, which
    // never calls `execute` at all.
    let env = Env::new();
    env.write_source("modes/one.md", b"NEW");
    let manifest = env.write_manifest("copy = [\"modes\", \"modes/one.md\"]");

    let plan = apply::plan(&manifest, env.root(), &RealProbe);

    let contested = env.dest().join("modes/one.md");
    let claimants: Vec<&apply::PlannedEntry> = plan
        .entries
        .iter()
        .filter(|p| p.entry.as_ref().is_some_and(|e| e.dest == contested))
        .collect();
    assert_eq!(claimants.len(), 2, "the fixture must actually conflict");
    for planned in claimants {
        assert!(
            matches!(planned.decision, apply::Decision::Refused(_)),
            "every claimant must be refused, not just the later one: {:?}",
            planned.decision
        );
        assert!(
            planned.backup.is_none(),
            "a refused entry must not carry a backup path"
        );
    }
    assert!(
        plan.describe().refused,
        "dry-run must report the conflict too"
    );
}

#[test]
fn a_missing_source_is_refused_per_entry() {
    let env = Env::new();
    let manifest = env.write_manifest("copy = [\"never-created.md\"]");

    let out = env.apply(&manifest, &RealProbe);

    assert!(
        out.refused,
        "a missing source must be refused: {}",
        out.text()
    );
    assert!(
        out.text().contains("refused") && out.text().contains("never-created.md"),
        "{}",
        out.text()
    );
    assert!(
        !env.dest().join("never-created.md").exists(),
        "nothing may be created for a missing source"
    );
}
