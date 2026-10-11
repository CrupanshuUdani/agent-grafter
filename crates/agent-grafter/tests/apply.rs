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

/// Report lines carry file paths formatted with the platform's own separator
/// (`Path::join` + `Display`), so a literal like `"modes/one.md"` only matches
/// the report text on Unix. Use this wherever a test checks report *text* for
/// a multi-segment path; a plain literal is fine for a single path segment.
fn sep(path: &str) -> String {
    path.replace('/', std::path::MAIN_SEPARATOR_STR)
}

/// A temp-dir sandbox laid out the way a real install is: a `source/`
/// directory holding the canonical files, a `dest/` directory standing in for
/// a target root like an agent's own config directory, and a manifest file
/// between them.
struct Env {
    dir: tempfile::TempDir,
}

impl Env {
    /// Create the sandbox with both `source/` and `dest/` already existing.
    /// `dest/` exists because the tool refuses a target root that doesn't exist —
    /// one test explicitly removes it to exercise that refusal.
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

    /// Write a file (creating parents) under `dest/` — a file this tool did
    /// not put there, standing in for whatever the user already had.
    fn write_dest(&self, rel: &str, bytes: &[u8]) {
        let path = self.dest().join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create dest parent");
        }
        fs::write(path, bytes).expect("write dest file");
    }

    /// Delete a file from `source/`, the way a user editing their canonical
    /// tree would.
    fn remove_source(&self, rel: &str) {
        fs::remove_file(self.source().join(rel)).expect("remove source file");
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
// 1. The basic case: a real target root and a real source file behind
//    the declared entry.
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
// 6. The tricky one: a declared `FileSymlink` that is downgraded to a copy
//    leaves an ordinary file at `dest`, which `classify()` cannot distinguish
//    from "never materialized". A second run must still report `same`.
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

    assert!(
        first.contains("wrote: AGENTS.md (as a copy:"),
        "a link that became a copy must say so, or the user believes edits \
         at dest reach the source: {first}"
    );

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

#[cfg(unix)]
#[test]
fn only_a_file_link_that_became_a_copy_carries_the_copy_notice() {
    let env = Env::new();
    env.write_source("AGENTS.md", b"linked");
    env.write_source("rules.md", b"copied");
    let manifest = env.write_manifest("link = [\"AGENTS.md\"]\ncopy = [\"rules.md\"]");

    let out = env.apply(&manifest, &ForcedProbe(true)).text();
    assert!(out.contains("wrote: AGENTS.md"), "{out}");
    assert!(out.contains("wrote: rules.md"), "{out}");
    assert!(
        !out.contains("as a copy"),
        "a real symlink, or a declared copy, is not a fallback: {out}"
    );
}

// ---------------------------------------------------------------------------
// 7. A refusal is per-entry, not a whole-run abort, and it sets the run's
//    exit code to 4.
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
// 8. A target root that does not exist is refused outright, rather than
//    silently built from a typo.
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
// 9. The `source` path resolves relative to the manifest file, not the
//    process's working directory.
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
// Dry-run honesty: the same plan drives both dry-run and real modes.
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

    // A refusal is per-entry: the uncontested sibling from the same
    // declared directory is processed normally.
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

// ===========================================================================
// Prune: A file that used to be in the source, was materialized to the
// destination, and has since been deleted from the source, is removed from
// the destination — and *nothing else is*.
//
// The first two tests below are the load-bearing ones. The obvious wording
// for this feature ("destination entries absent from the manifest and from the
// source are removed") would, read literally against a real target root like
// an agent's config directory, authorize deleting a user's credentials,
// settings and session transcripts. These two tests are what stands between
// that reading and the user's disk, so they are written to fail loudly if
// prune's scope ever widens from "inside a declared directory entry's own
// subtree" to "the target root".
// ===========================================================================

#[test]
fn prune_never_touches_anything_at_the_target_root() {
    let env = Env::new();
    env.write_source("modes/one.md", b"first mode");
    let manifest = env.write_manifest("copy = [\"modes\"]");
    env.apply(&manifest, &RealProbe);

    // The user's own files, sitting at the target root — exactly the shape of
    // the real data a target like an agent's config directory holds. Every one
    // of them is "absent from the manifest and absent from the source".
    env.write_dest(".credentials.json", b"{\"token\":\"secret\"}");
    env.write_dest("settings.json", b"{\"theme\":\"dark\"}");
    env.write_dest("unrelated.txt", b"hand written");
    env.write_dest("projects/session-transcript.jsonl", b"a long conversation");

    // Give prune real work to do in the same run, so this is not passing
    // merely because prune never ran at all.
    env.remove_source("modes/one.md");
    env.write_source("modes/two.md", b"second mode");

    let out = env.apply(&manifest, &RealProbe);

    assert!(
        out.text().contains(&sep("pruned: modes/one.md")),
        "the fixture must actually exercise prune, or it proves nothing: {}",
        out.text()
    );

    assert_eq!(
        fs::read(env.dest().join(".credentials.json")).unwrap(),
        b"{\"token\":\"secret\"}",
        "a credential file at the target root must never be pruned"
    );
    assert_eq!(
        fs::read(env.dest().join("settings.json")).unwrap(),
        b"{\"theme\":\"dark\"}"
    );
    assert_eq!(
        fs::read(env.dest().join("unrelated.txt")).unwrap(),
        b"hand written"
    );
    assert_eq!(
        fs::read(env.dest().join("projects/session-transcript.jsonl")).unwrap(),
        b"a long conversation"
    );

    // Not even mentioned: prune must not enumerate the target root at all, so
    // nothing there can appear in the report either.
    let text = out.text();
    for name in [
        ".credentials.json",
        "settings.json",
        "unrelated.txt",
        "session-transcript.jsonl",
    ] {
        assert!(
            !text.contains(name),
            "prune must not so much as enumerate {name} at the target root: {text}"
        );
    }
}

#[test]
fn prune_is_scoped_to_each_declared_directorys_own_subtree() {
    // Distinct from the test above: this one is about *per-entry* scoping. A
    // whole directory tree at the target root that shares no name with
    // anything declared is outside every declared subtree, so prune must not
    // descend into it — not even to look.
    let env = Env::new();
    env.write_source("modes/one.md", b"first mode");
    env.write_source("modes/nested/deep.md", b"nested mode");
    let manifest = env.write_manifest("copy = [\"modes\"]");
    env.apply(&manifest, &RealProbe);

    env.write_dest("some-other-dir/notes.md", b"user's own notes");
    env.write_dest("some-other-dir/further/down.md", b"further down");
    env.write_dest("plugins/config.json", b"{}");

    env.remove_source("modes/nested/deep.md");
    let before_others = snapshot(&env.dest().join("some-other-dir"));

    let out = env.apply(&manifest, &RealProbe);

    assert!(
        out.text().contains(&sep("pruned: modes/nested/deep.md")),
        "the declared subtree's own stale file must still be pruned: {}",
        out.text()
    );
    assert!(
        !env.dest().join("modes/nested/deep.md").exists(),
        "the stale file inside the declared subtree is gone"
    );

    assert_eq!(
        before_others,
        snapshot(&env.dest().join("some-other-dir")),
        "an undeclared directory at the target root must be untouched"
    );
    assert_eq!(
        fs::read(env.dest().join("plugins/config.json")).unwrap(),
        b"{}"
    );
}

#[test]
fn a_file_deleted_from_the_source_is_backed_up_and_then_removed_from_dest() {
    let env = Env::new();
    env.write_source("modes/keep.md", b"still declared");
    env.write_source("modes/gone.md", b"about to be deleted");
    let manifest = env.write_manifest("copy = [\"modes\"]");
    env.apply(&manifest, &RealProbe);
    assert_eq!(
        fs::read(env.dest().join("modes/gone.md")).unwrap(),
        b"about to be deleted"
    );

    env.remove_source("modes/gone.md");

    let plan = apply::plan(&manifest, env.root(), &RealProbe);
    let timestamp = plan.run_timestamp.clone();
    let out = apply::execute(&plan, &RealProbe);

    assert!(!out.failed, "prune must not fail: {}", out.text());
    assert!(
        out.text().contains(&sep("pruned: modes/gone.md")),
        "the prune must be reported: {}",
        out.text()
    );
    assert!(
        !env.dest().join("modes/gone.md").exists(),
        "the file must be gone from the destination"
    );
    assert_eq!(
        fs::read(env.dest().join("modes/keep.md")).unwrap(),
        b"still declared",
        "a still-declared sibling must survive"
    );

    // Two separate `.join()` calls, not one `.join("modes/gone.md")`: a
    // single call pushes its argument as one opaque segment, preserving
    // whatever separator character it was typed with, so it would not match
    // `backup.display()`'s all-native-separator rendering on Windows.
    let backup = env
        .dest()
        .join(".agrafter-backups")
        .join(&timestamp)
        .join("modes")
        .join("gone.md");
    assert_eq!(
        fs::read(&backup).unwrap_or_else(|e| panic!("no backup at {}: {e}", backup.display())),
        b"about to be deleted",
        "a pruned file's content must survive in the backup"
    );
    assert!(
        out.text().contains(&backup.display().to_string()),
        "the report must name where the content went: {}",
        out.text()
    );
}

#[test]
fn dry_run_reports_a_prune_without_performing_it() {
    let env = Env::new();
    env.write_source("modes/gone.md", b"about to be deleted");
    let manifest = env.write_manifest("copy = [\"modes\"]");
    env.apply(&manifest, &RealProbe);
    env.remove_source("modes/gone.md");

    let before = snapshot(&env.dest());
    let described = apply::plan(&manifest, env.root(), &RealProbe).describe();

    assert!(
        described
            .text()
            .contains(&sep("would prune: modes/gone.md")),
        "dry-run must say what it would prune: {}",
        described.text()
    );
    assert_eq!(
        before,
        snapshot(&env.dest()),
        "a dry run must not remove, back up, or create anything"
    );
    assert_eq!(
        fs::read(env.dest().join("modes/gone.md")).unwrap(),
        b"about to be deleted"
    );
}

#[test]
fn a_link_entrys_destination_is_never_walked_for_pruning() {
    // A `link` entry's destination *is* the source, reached through a link.
    // Walking it for prune candidates would mean deleting through the link,
    // into the tree this whole tool exists to treat as the source of truth.
    let env = Env::new();
    env.write_source("memory/note.md", b"remembered");
    let manifest = env.write_manifest("link = [\"memory\"]");
    let first = env.apply(&manifest, &RealProbe);
    assert!(!first.refused && !first.failed, "setup: {}", first.text());

    // A file placed "inside the destination" — which, the destination being a
    // link, means inside the source. It is not declared anywhere.
    fs::write(env.dest().join("memory/scratch.md"), b"written by hand").unwrap();

    let out = env.apply(&manifest, &RealProbe);

    assert_eq!(
        fs::read(env.source().join("memory/scratch.md")).unwrap(),
        b"written by hand",
        "prune must never reach through a link into the source"
    );
    assert!(
        !out.text().contains("scratch.md"),
        "a link entry's destination must not even be enumerated: {}",
        out.text()
    );
    assert!(
        !out.text().contains("prune"),
        "a link entry contributes no prune candidates at all: {}",
        out.text()
    );
}

#[test]
fn a_link_found_where_a_prune_candidate_was_expected_is_left_in_place() {
    // `is_dir()` and `is_symlink()` can both be true at once, so deciding
    // "recurse here" or "delete this" from a bare metadata check is wrong.
    // Every step of the prune walk asks `classify` first.
    let env = Env::new();
    env.write_source("modes/one.md", b"first mode");
    let manifest = env.write_manifest("copy = [\"modes\"]");
    env.apply(&manifest, &RealProbe);

    // Real data somewhere else entirely, and two links to it planted inside
    // the declared subtree where prune candidates would otherwise be.
    let elsewhere = env.root().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    fs::write(elsewhere.join("precious.md"), b"real user data").unwrap();
    safelink::create::create_dir_link(&elsewhere, &env.dest().join("modes/linked-dir")).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        elsewhere.join("precious.md"),
        env.dest().join("modes/linked-file.md"),
    )
    .unwrap();

    let out = env.apply(&manifest, &RealProbe);

    assert!(
        !out.failed,
        "a link found mid-walk is not a failure: {}",
        out.text()
    );
    assert_eq!(
        fs::read(elsewhere.join("precious.md")).unwrap(),
        b"real user data",
        "nothing may be deleted through the link"
    );
    assert!(
        env.dest().join("modes/linked-dir").exists(),
        "the directory link itself must survive"
    );
    assert!(
        out.text().contains(&sep("modes/linked-dir")),
        "and it must be reported, not silently skipped: {}",
        out.text()
    );
    #[cfg(unix)]
    {
        assert!(
            fs::symlink_metadata(env.dest().join("modes/linked-file.md"))
                .unwrap()
                .file_type()
                .is_symlink(),
            "the file link itself must survive"
        );
        assert!(
            out.text().contains("modes/linked-file.md"),
            "and it must be reported too: {}",
            out.text()
        );
    }
}

#[test]
fn a_source_directory_that_has_vanished_prunes_nothing_at_all() {
    // The asymmetry that makes prune safe: under-reading the *destination*
    // finds fewer candidates (harmless), while under-reading the *source*
    // makes every materialized file look undeclared. An unmounted volume is
    // not a licence to wipe the destination.
    let env = Env::new();
    env.write_source("modes/one.md", b"first mode");
    env.write_source("modes/nested/two.md", b"second mode");
    let manifest = env.write_manifest("copy = [\"modes\"]");
    env.apply(&manifest, &RealProbe);

    let before = snapshot(&env.dest().join("modes"));
    env.remove_source("modes/nested/two.md");
    fs::remove_dir(env.source().join("modes/nested")).unwrap();
    env.remove_source("modes/one.md");
    fs::remove_dir(env.source().join("modes")).unwrap();

    let out = env.apply(&manifest, &RealProbe);

    assert!(
        out.refused,
        "a vanished source is refused, not acted on: {}",
        out.text()
    );
    assert!(
        !out.text().contains("prune"),
        "and it must produce no prune candidates whatsoever: {}",
        out.text()
    );
    assert_eq!(
        before,
        snapshot(&env.dest().join("modes")),
        "the materialized subtree must be exactly as it was"
    );
}

#[test]
fn an_empty_source_directory_still_prunes_what_it_no_longer_holds() {
    // The legitimate twin of the test above: present-but-empty is a real,
    // declarable state, and everything the destination still holds for it is
    // genuinely stale.
    let env = Env::new();
    env.write_source("modes/one.md", b"first mode");
    let manifest = env.write_manifest("copy = [\"modes\"]");
    env.apply(&manifest, &RealProbe);

    env.remove_source("modes/one.md");
    assert!(env.source().join("modes").is_dir(), "still a directory");

    let out = env.apply(&manifest, &RealProbe);

    assert!(
        out.text().contains(&sep("pruned: modes/one.md")),
        "an emptied source directory still prunes: {}",
        out.text()
    );
    assert!(!env.dest().join("modes/one.md").exists());
}

// ===========================================================================
// Retention and this run's own backup-writing, in one single `apply`
// invocation.
// ===========================================================================

#[test]
fn one_run_sweeps_old_backups_and_writes_its_own_without_either_disturbing_the_other() {
    let env = Env::new();
    env.write_source("modes/one.md", b"version one");
    env.write_source("modes/gone.md", b"about to be deleted");
    let manifest = env.write_manifest("copy = [\"modes\"]");
    env.apply(&manifest, &RealProbe);

    // Ten backup directories already on disk, fabricated rather than produced
    // by ten timed runs. This run makes an eleventh, so exactly one must go.
    let backups = env.dest().join(".agrafter-backups");
    fs::create_dir_all(&backups).unwrap();
    let fabricated: Vec<String> = (1..=10)
        .map(|day| format!("202401{day:02}T000000Z"))
        .collect();
    for name in &fabricated {
        let dir = backups.join(name);
        fs::create_dir_all(dir.join("modes")).unwrap();
        fs::write(dir.join("modes/one.md"), name.as_bytes()).unwrap();
    }

    // This run both replaces a file (a backup) and prunes one (another backup).
    env.write_source("modes/one.md", b"version two");
    env.remove_source("modes/gone.md");

    let plan = apply::plan(&manifest, env.root(), &RealProbe);
    let timestamp = plan.run_timestamp.clone();
    let out = apply::execute(&plan, &RealProbe);
    let text = out.text();

    assert!(!out.failed, "nothing here should fail: {text}");
    assert!(
        text.contains("pruned backup: 20240101T000000Z"),
        "the oldest backup must be swept, by name: {text}"
    );
    assert!(
        text.contains(&sep("wrote: modes/one.md")) && text.contains(&sep("pruned: modes/gone.md")),
        "the run's own work must still have happened: {text}"
    );

    let mut present: Vec<String> = fs::read_dir(&backups)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    present.sort();
    assert_eq!(
        present.len(),
        10,
        "exactly the limit must remain, got {present:?}"
    );
    assert!(
        present.contains(&timestamp),
        "this run's own backup directory must be among the survivors: {present:?}"
    );
    assert!(
        !present.contains(&fabricated[0]),
        "the oldest must be the one that went: {present:?}"
    );
    assert!(
        !backups.join(&fabricated[0]).exists(),
        "and it must be gone from disk, contents and all"
    );
    for name in &fabricated[1..] {
        assert_eq!(
            fs::read(backups.join(name).join("modes/one.md")).unwrap(),
            name.as_bytes(),
            "a surviving backup's contents must be intact"
        );
    }

    // The sweep ran first and left this run's own backups completely alone.
    let mine = backups.join(&timestamp);
    assert_eq!(
        fs::read(mine.join("modes/one.md")).unwrap(),
        b"version one",
        "the replaced file's old content must be in this run's backup"
    );
    assert_eq!(
        fs::read(mine.join("modes/gone.md")).unwrap(),
        b"about to be deleted",
        "the pruned file's content must be in this run's backup"
    );
    assert_eq!(
        fs::read(env.dest().join("modes/one.md")).unwrap(),
        b"version two"
    );
    assert!(!env.dest().join("modes/gone.md").exists());
}

#[test]
fn dry_run_reports_the_retention_sweep_without_removing_anything() {
    let env = Env::new();
    env.write_source("modes/one.md", b"version one");
    let manifest = env.write_manifest("copy = [\"modes\"]");
    env.apply(&manifest, &RealProbe);

    let backups = env.dest().join(".agrafter-backups");
    fs::create_dir_all(&backups).unwrap();
    for day in 1..=12 {
        fs::create_dir_all(backups.join(format!("202401{day:02}T000000Z"))).unwrap();
    }

    let before = snapshot(&backups);
    let described = apply::plan(&manifest, env.root(), &RealProbe).describe();

    assert!(
        described
            .text()
            .contains("would prune backup: 20240101T000000Z")
            && described
                .text()
                .contains("would prune backup: 20240102T000000Z"),
        "dry-run must name the backups it would sweep: {}",
        described.text()
    );
    assert_eq!(
        before,
        snapshot(&backups),
        "a dry run must not sweep anything"
    );
}

#[test]
fn a_linked_intermediate_component_of_a_declared_path_is_never_walked_through() {
    // Every other fixture in this file declares a single-segment entry, which
    // is exactly why this went unnoticed: with one component there is only one
    // path to classify, and classifying the joined result is the same thing.
    // With two, it is not. `dest.join("agents/modes")` is resolved by the
    // kernel *through* `dest/agents` before `classify` ever sees the final
    // path, so classifying only the end of the path reports `NotALink` about
    // an object that can live entirely outside the target root.
    let env = Env::new();
    env.write_source("agents/modes/declared.md", b"declared");
    let manifest = env.write_manifest("copy = [\"agents/modes\"]");

    // Somewhere else entirely — standing in for whatever a stale link points
    // at after the user moved their source tree.
    let outside = env.root().join("outside");
    fs::create_dir_all(outside.join("modes")).unwrap();
    fs::write(outside.join("modes/stray.md"), b"not ours to delete").unwrap();
    fs::write(outside.join("unrelated.md"), b"also not ours").unwrap();

    // A link at the *intermediate* component, not at the scope root.
    safelink::create::create_dir_link(&outside, &env.dest().join("agents")).unwrap();

    let out = env.apply(&manifest, &RealProbe);
    let text = out.text();

    assert_eq!(
        fs::read(outside.join("modes/stray.md")).unwrap(),
        b"not ours to delete",
        "a file outside the target root must never be pruned — reached here \
         only by resolving through a linked intermediate path component: {text}"
    );
    assert_eq!(
        fs::read(outside.join("unrelated.md")).unwrap(),
        b"also not ours"
    );
    assert!(
        !text.contains("pruned: ") && !text.contains("would prune: "),
        "nothing may be planned for removal once the path escapes: {text}"
    );
    assert!(
        text.contains("prune skipped"),
        "and the refusal must be reported, not silent: {text}"
    );
}

#[test]
fn a_linked_intermediate_component_of_a_declared_path_is_never_written_through() {
    // The write-side twin of the prune fixture above. `plan_copy_entry` built
    // each file's destination via a plain, un-classified `ctx.dest.join(name)
    // .join(rel)` — resolved by the kernel through whatever `dest/agents`
    // actually is on disk at write time, so a symlinked intermediate
    // destination component made `apply` *write* — and file a backup for —
    // content outside `target.dest`.
    let env = Env::new();
    env.write_source("agents/modes/declared.md", b"new content");
    let manifest = env.write_manifest("copy = [\"agents/modes\"]");

    // Somewhere else entirely — standing in for whatever a stray symlink
    // happens to point at. Pre-seeded with content of its own so that an
    // unguarded write would both overwrite it *and* have something to file a
    // (wrong-location) backup of.
    let outside = env.root().join("outside");
    fs::create_dir_all(outside.join("modes")).unwrap();
    fs::write(outside.join("modes/declared.md"), b"not ours to overwrite").unwrap();

    // A link at the *intermediate* destination component, not at the scope
    // root.
    safelink::create::create_dir_link(&outside, &env.dest().join("agents")).unwrap();

    let out = env.apply(&manifest, &RealProbe);
    let text = out.text();

    assert_eq!(
        fs::read(outside.join("modes/declared.md")).unwrap(),
        b"not ours to overwrite",
        "a file outside the target root must never be written to — reached \
         here only by resolving through a linked intermediate destination \
         component: {text}"
    );
    assert!(
        !text.contains("backed up to") && !text.contains("would back up to"),
        "no backup may ever be filed for a path outside target.dest: {text}"
    );
    assert!(
        matches!(
            safelink::classify::classify(&env.dest().join("agents")),
            Ok(safelink::classify::LinkClass::DirLink)
        ),
        "the link itself must be left alone, neither unlinked nor written \
         through"
    );
    assert!(
        text.contains("refused"),
        "the refusal must be reported, not a silent no-op: {text}"
    );
    assert!(
        !env.dest().join(".agrafter-backups").exists(),
        "nothing inside target.dest should have needed a backup at all: {text}"
    );
}

#[test]
fn a_linked_intermediate_component_of_a_declared_link_entrys_path_is_never_written_through() {
    // `plan_link_entry` builds its destination the same way `plan_copy_entry`
    // does, and is a distinct code path from the directory-copy expansion
    // above: this proves the fix covers it too, not just the `copy` side.
    let env = Env::new();
    env.write_source("agents/modes/file.md", b"content");
    let manifest = env.write_manifest("link = [\"agents/modes\"]");

    let outside = env.root().join("outside");
    fs::create_dir_all(&outside).unwrap();

    safelink::create::create_dir_link(&outside, &env.dest().join("agents")).unwrap();

    let out = env.apply(&manifest, &RealProbe);
    let text = out.text();

    assert!(
        !outside.join("modes").exists(),
        "a directory link must never be created outside the target root — \
         reached here only by resolving through a linked intermediate \
         destination component: {text}"
    );
    assert!(
        matches!(
            safelink::classify::classify(&env.dest().join("agents")),
            Ok(safelink::classify::LinkClass::DirLink)
        ),
        "the link itself must be left alone, neither unlinked nor written \
         through"
    );
    assert!(
        text.contains("refused"),
        "the refusal must be reported, not a silent no-op: {text}"
    );
}

#[test]
fn a_linked_intermediate_component_of_a_single_file_copy_entry_is_never_written_through() {
    // The third and last call site the fix has to cover: a `copy` entry
    // naming a single file (not a directory), which never reaches
    // `plan_prunes` at all and is planned entirely inside
    // `plan_copy_entry`'s non-directory branch.
    let env = Env::new();
    env.write_source("agents/file.md", b"new content");
    let manifest = env.write_manifest("copy = [\"agents/file.md\"]");

    let outside = env.root().join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("file.md"), b"not ours to overwrite").unwrap();

    safelink::create::create_dir_link(&outside, &env.dest().join("agents")).unwrap();

    let out = env.apply(&manifest, &RealProbe);
    let text = out.text();

    assert_eq!(
        fs::read(outside.join("file.md")).unwrap(),
        b"not ours to overwrite",
        "a file outside the target root must never be written to — reached \
         here only by resolving through a linked intermediate destination \
         component: {text}"
    );
    assert!(
        !text.contains("backed up to") && !text.contains("would back up to"),
        "no backup may ever be filed for a path outside target.dest: {text}"
    );
    assert!(
        text.contains("refused"),
        "the refusal must be reported, not a silent no-op: {text}"
    );
}

#[test]
fn an_entry_nested_under_a_link_entry_created_by_the_same_run_is_refused() {
    // `resolve_scoped_dest` only classifies what already exists on disk at
    // plan time. A `link` entry that does not exist yet resolves as "absent,
    // therefore a clean write" — correct in isolation — but says nothing
    // about a *different*, nested entry planned in the same run. `link`
    // entries execute before `copy` entries for the same target (see
    // `plan_target`), so by the time the nested `copy` entry would write,
    // the `link` entry has already turned its own destination into a real
    // directory link, and the nested write would resolve *through* it —
    // the same scope escape, reached by two entries agreeing with each
    // other instead of by a link already on disk.
    let env = Env::new();
    env.write_source("agents/modes/file.md", b"content");
    let manifest = env.write_manifest("link = [\"agents\"]\ncopy = [\"agents/modes\"]");

    let out = env.apply(&manifest, &RealProbe);
    let text = out.text();

    assert!(
        text.contains("wrote: agents"),
        "the link entry itself has nothing nested under *it* and must still \
         succeed: {text}"
    );
    assert!(
        text.contains(&sep("refused: agents/modes/file.md")),
        "the entry nested under the about-to-be-created link must be \
         refused, never silently written through it: {text}"
    );
    assert!(
        text.contains("nested inside"),
        "the refusal must say why: {text}"
    );
    assert!(
        !text.contains("backed up to") && !text.contains("would back up to"),
        "nothing was written for the refused entry, so nothing should have \
         needed a backup: {text}"
    );
    assert!(
        matches!(
            safelink::classify::classify(&env.dest().join("agents")),
            Ok(safelink::classify::LinkClass::DirLink)
        ),
        "the link entry must actually have materialized, for this to be a \
         meaningful test of what comes after it"
    );
}

#[cfg(unix)]
#[test]
fn a_source_path_that_cannot_be_read_is_not_treated_as_deleted() {
    // "Absent from the source" was decided by `symlink_metadata(...).is_ok()`,
    // which reads *every* error as "gone" — including permission denied. An
    // unreadable source must never become a deletion.
    //
    // Reached without any race: `collect_files` records a directory symlink in
    // the source as a traversal leaf without descending, so the source walk
    // succeeds while the per-file stat underneath it does not.
    use std::os::unix::fs::PermissionsExt;

    let env = Env::new();
    env.write_source("modes/one.md", b"first mode");
    env.write_source("modes/sub/keep.md", b"materialized earlier");
    let manifest = env.write_manifest("copy = [\"modes\"]");
    env.apply(&manifest, &RealProbe);
    assert_eq!(
        fs::read(env.dest().join("modes/sub/keep.md")).unwrap(),
        b"materialized earlier",
        "setup: the file must be materialized before the source becomes unreadable"
    );

    // Replace the real source subdirectory with a link to one the process
    // cannot traverse.
    let locked = env.root().join("locked");
    fs::create_dir(&locked).unwrap();
    env.remove_source("modes/sub/keep.md");
    fs::remove_dir(env.source().join("modes/sub")).unwrap();
    safelink::create::create_dir_link(&locked, &env.source().join("modes/sub")).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

    if fs::read(env.source().join("modes/sub/keep.md")).is_err()
        && fs::metadata(&locked).is_ok_and(|m| m.permissions().mode() & 0o777 == 0)
        && std::fs::symlink_metadata(env.source().join("modes/sub/keep.md"))
            .err()
            .map(|e| e.kind())
            != Some(std::io::ErrorKind::PermissionDenied)
    {
        // Privileges ignore the mode bits (root in a container). The case
        // cannot be produced here; asserting would test the runner.
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }

    let out = env.apply(&manifest, &RealProbe);
    let text = out.text();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(
        fs::read(env.dest().join("modes/sub/keep.md")).unwrap(),
        b"materialized earlier",
        "an unreadable source path must not be read as a deleted one: {text}"
    );
    assert!(
        !text.contains("pruned: modes/sub/keep.md"),
        "and it must not even be planned for removal: {text}"
    );
    assert!(
        text.contains("prune skipped"),
        "the undecidable path must be reported, not silently skipped: {text}"
    );
}

#[cfg(unix)]
#[test]
fn a_source_directory_replaced_by_a_plain_file_still_prunes_what_it_held() {
    // The case the fix above must not regress: the source now has a plain
    // *file* where a directory used to be, so stat'ing a path *under* it fails
    // with "not a directory". That is genuinely gone, not undecidable.
    let env = Env::new();
    env.write_source("modes/sub/old.md", b"materialized earlier");
    let manifest = env.write_manifest("copy = [\"modes\"]");
    env.apply(&manifest, &RealProbe);
    assert!(env.dest().join("modes/sub/old.md").exists(), "setup");

    env.remove_source("modes/sub/old.md");
    fs::remove_dir(env.source().join("modes/sub")).unwrap();
    fs::write(env.source().join("modes/sub"), b"now a file").unwrap();

    let out = env.apply(&manifest, &RealProbe);

    assert!(
        out.text().contains("pruned: modes/sub/old.md"),
        "a path whose source parent is now a plain file is genuinely gone: {}",
        out.text()
    );
    assert!(!env.dest().join("modes/sub/old.md").exists());
}

#[cfg(unix)]
#[test]
fn a_destination_subtree_that_cannot_be_read_prunes_nothing_and_says_so() {
    // The safe direction is already forced (no candidates from a partial
    // walk). This is about honesty: a prune pass that quietly did nothing
    // reads exactly like one that had nothing to do.
    use std::os::unix::fs::PermissionsExt;

    let env = Env::new();
    env.write_source("modes/one.md", b"first mode");
    env.write_source("modes/nested/two.md", b"second mode");
    let manifest = env.write_manifest("copy = [\"modes\"]");
    env.apply(&manifest, &RealProbe);

    env.remove_source("modes/nested/two.md");
    let locked = env.dest().join("modes/nested");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

    if fs::read_dir(&locked).is_ok() {
        // Running with privileges that ignore the mode bits (root in a
        // container, typically). The case cannot be produced here, and
        // asserting anyway would be asserting about the runner, not the code.
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }

    let described = apply::plan(&manifest, env.root(), &RealProbe).describe();
    let text = described.text();

    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();

    assert!(
        text.contains("prune skipped") && text.contains("modes"),
        "an unreadable destination subtree must be reported, not swallowed: {text}"
    );
    assert!(
        !text.contains("would prune:"),
        "and nothing may be planned for removal from a walk that failed: {text}"
    );
    assert_eq!(
        fs::read(locked.join("two.md")).unwrap(),
        b"second mode",
        "the file is still there, unpruned"
    );
}

#[test]
fn two_targets_sharing_a_root_sweep_its_backups_once_not_once_each() {
    // Two `[targets.*]` may point at the same directory — the destination
    // conflict check already exists because of exactly that. They share one
    // `.agrafter-backups/`, so sweeping it per-target would remove each
    // expired directory once and then fail removing it again, reporting an
    // unexpected error over a directory that is already correctly gone.
    let env = Env::new();
    env.write_source("modes/one.md", b"first mode");
    let toml = format!(
        "source = \"source\"\ncopy = [\"modes\"]\n\n\
         [targets.agent-a]\ndest = '{0}'\n\n[targets.agent-b]\ndest = '{0}'\n",
        env.dest().display()
    );
    fs::write(env.manifest_path(), &toml).unwrap();
    let manifest = manifest::parse(&toml).unwrap();

    let backups = env.dest().join(".agrafter-backups");
    fs::create_dir_all(&backups).unwrap();
    for day in 1..=12 {
        let dir = backups.join(format!("202401{day:02}T000000Z"));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("kept.md"), b"backed up").unwrap();
    }

    let plan = apply::plan(&manifest, env.root(), &RealProbe);

    let planned: Vec<&PathBuf> = plan
        .retention
        .iter()
        .flat_map(|sweep| sweep.removals.iter())
        .collect();
    assert_eq!(
        planned.len(),
        2,
        "one shared backup directory must be swept once, not once per \
         target — a plan that lists a removal twice also prints it twice \
         under --dry-run: {planned:?}"
    );

    let out = apply::execute(&plan, &RealProbe);
    assert!(
        !out.failed,
        "removing an already-removed backup must not be reported as an \
         unexpected error: {}",
        out.text()
    );
    assert_eq!(
        fs::read_dir(&backups).unwrap().count(),
        10,
        "and the sweep must still have done its job"
    );
}

#[test]
fn the_backup_directory_is_never_itself_a_prune_candidate() {
    // `.agrafter-backups` is this tool's bookkeeping, not an entry's content.
    // A manifest pathological enough to declare it by name must still not put
    // it in the prune walk.
    let env = Env::new();
    env.write_source(".agrafter-backups/decoy.md", b"declared");
    let manifest = env.write_manifest("copy = [\".agrafter-backups\"]");
    env.apply(&manifest, &RealProbe);

    // A real backup, of the shape a normal run would leave behind.
    let real_backup = env
        .dest()
        .join(".agrafter-backups/20240101T000000Z/modes/one.md");
    fs::create_dir_all(real_backup.parent().unwrap()).unwrap();
    fs::write(&real_backup, b"an earlier version").unwrap();

    let out = env.apply(&manifest, &RealProbe);

    assert!(
        !out.text().contains("prune"),
        "nothing under the backup directory may ever be a prune candidate: {}",
        out.text()
    );
    assert_eq!(
        fs::read(&real_backup).unwrap(),
        b"an earlier version",
        "a real backup must survive"
    );
}

#[test]
fn a_declared_entry_naming_the_backup_directory_is_refused_not_written() {
    // A single-component entry has no intermediate component for
    // `resolve_scoped_dest`'s link/inspection walk to check at all -- so
    // without a dedicated check, `link = [".agrafter-backups"]` sailed
    // through as an ordinary, not-yet-existing destination and planned as a
    // clean `Decision::Write`. At execute time that turns this tool's own
    // backup directory into a real directory link, and every backup this
    // run subsequently files for the target resolves through it instead of
    // landing in `target.dest` -- silently, since the report line for a
    // successful write and a successful backup both read as perfectly
    // normal.
    let env = Env::new();
    env.write_source(".agrafter-backups/decoy.md", b"declared");
    let manifest = env.write_manifest("link = [\".agrafter-backups\"]");

    let out = env.apply(&manifest, &RealProbe);
    let text = out.text();

    assert!(
        text.contains("refused"),
        "declaring the backup directory's own name must be refused, not \
         silently written: {text}"
    );
    assert!(
        !text.contains("wrote"),
        "nothing should have been written at all: {text}"
    );
    assert!(
        !matches!(
            safelink::classify::classify(&env.dest().join(".agrafter-backups")),
            Ok(safelink::classify::LinkClass::DirLink)
        ),
        "the backup directory must never become a link to source content"
    );
}

// ===========================================================================
// The backup *root* itself (`target.dest/.agrafter-backups`) was
// never classified before `apply` read from it (retention) or wrote into it
// (`backup_path`) -- only the entries *inside* it were. A linked backup root
// therefore let both the retention sweep and a backed-up write escape
// `target.dest`, the same severity class as the intermediate-destination-
// component escape fixed above, one level higher up the path.
// ===========================================================================

#[test]
fn a_linked_backup_root_is_never_read_or_deleted_through_for_retention() {
    // `plan_target_retention` used to hand `target.dest.join(".agrafter-backups")`
    // straight to `plan_retention`, which calls `std::fs::read_dir` on it with
    // no classification of that path itself -- only of the entries `read_dir`
    // turns up (`retention.rs:109`). If the backup root is itself a link,
    // `read_dir` follows it into whatever it points at, and any real
    // subdirectory there that happens to be named like a valid timestamp is
    // treated as this tool's own backup: counted toward `KEEP_BACKUPS`, and
    // once it ages out, deleted by `remove_backup_dir` -- which classifies
    // that specific subdirectory (it is a real, unlinked directory) but has
    // no way to know the path arrived here by resolving through a link one
    // level up. A linked backup root turns retention into a delete of a real
    // directory outside `target.dest`, not merely a write into one.
    let env = Env::new();
    env.write_source("modes/one.md", b"first mode");
    let manifest = env.write_manifest("copy = [\"modes\"]");
    env.apply(&manifest, &RealProbe);

    // A real directory outside `target.dest`, holding eleven directories
    // shaped like this tool's own backup names -- one more than
    // `KEEP_BACKUPS` (10), so the oldest is eligible for eviction purely from
    // what is already on disk, with no dependency on this run also writing a
    // backup of its own.
    let elsewhere = env.root().join("elsewhere-backups");
    fs::create_dir_all(&elsewhere).unwrap();
    let names: Vec<String> = (1..=11)
        .map(|day| format!("202401{day:02}T000000Z"))
        .collect();
    for name in &names {
        let dir = elsewhere.join(name);
        fs::create_dir_all(dir.join("modes")).unwrap();
        fs::write(dir.join("modes/one.md"), name.as_bytes()).unwrap();
    }

    // The backup root is a directory link, pointing at that real directory --
    // not declared anywhere in the manifest (that path is already refused);
    // it becomes one some other way, which is exactly what this test covers.
    safelink::create::create_dir_link(&elsewhere, &env.dest().join(".agrafter-backups")).unwrap();

    let before: Vec<PathBuf> = fs::read_dir(&elsewhere)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(before.len(), 11, "setup: eleven real directories present");

    let out = env.apply(&manifest, &RealProbe);
    let text = out.text();

    assert!(
        elsewhere.join(&names[0]).exists(),
        "the oldest directory outside target.dest must not be removed just \
         because it is reachable through a linked backup root: {text}"
    );
    for name in &names {
        assert!(
            elsewhere.join(name).exists(),
            "no directory reachable only through the link may be touched: {name}: {text}"
        );
    }
    assert!(
        !text.contains("pruned backup:") && !text.contains("would prune backup:"),
        "nothing may be reported as swept when the backup root is a link: {text}"
    );
    assert!(
        text.contains("backup retention skipped") && text.contains(".agrafter-backups"),
        "the skip must be reported, not silent, and must name the reason: {text}"
    );
    assert!(
        matches!(
            safelink::classify::classify(&env.dest().join(".agrafter-backups")),
            Ok(safelink::classify::LinkClass::DirLink)
        ),
        "the link itself must be left alone, neither followed nor removed"
    );
}

#[test]
fn a_linked_backup_root_refuses_writes_that_would_need_a_backup() {
    // The write-side twin: `backup_path` never classified `.agrafter-backups`
    // itself, only the entry's own destination, so a symlinked backup root let
    // a replaced file's backup be filed through it — outside `target.dest`.
    // A write without the backup it would normally get is forbidden, so the
    // entry must be refused rather than silently written with no backup.
    let env = Env::new();
    env.write_source("file.md", b"new content");
    let manifest = env.write_manifest("copy = [\"file.md\"]");
    // Real, different content already at the destination -- a write here
    // would normally back it up before overwriting it.
    env.write_dest("file.md", b"old content");

    let outside = env.root().join("outside-backups");
    fs::create_dir_all(&outside).unwrap();
    safelink::create::create_dir_link(&outside, &env.dest().join(".agrafter-backups")).unwrap();

    let out = env.apply(&manifest, &RealProbe);
    let text = out.text();

    assert_eq!(
        fs::read(env.dest().join("file.md")).unwrap(),
        b"old content",
        "nothing may be written when the backup it would need cannot be \
         filed safely: {text}"
    );
    assert!(
        text.contains(&sep("refused: file.md")),
        "the entry must be refused, not written without the backup it would \
         normally get: {text}"
    );
    assert!(
        !text.contains("backed up to") && !text.contains("would back up to"),
        "no backup may ever be filed through a linked backup root: {text}"
    );
    assert!(
        fs::read_dir(&outside).unwrap().next().is_none(),
        "nothing may land outside target.dest through the link: {text}"
    );
    assert!(
        matches!(
            safelink::classify::classify(&env.dest().join(".agrafter-backups")),
            Ok(safelink::classify::LinkClass::DirLink)
        ),
        "the link itself must be left alone, neither followed nor replaced"
    );
}

#[test]
fn an_entry_needing_no_backup_is_unaffected_by_a_linked_backup_root() {
    // Only entries that would actually have gone through `backup_path` are
    // refused. A destination with nothing real at it yet needs no backup
    // either way, so a linked backup root must not stop it from being written.
    let env = Env::new();
    env.write_source("file.md", b"new content");
    let manifest = env.write_manifest("copy = [\"file.md\"]");

    let outside = env.root().join("outside-backups");
    fs::create_dir_all(&outside).unwrap();
    safelink::create::create_dir_link(&outside, &env.dest().join(".agrafter-backups")).unwrap();

    let out = env.apply(&manifest, &RealProbe);
    let text = out.text();

    assert!(
        !out.refused,
        "a write that needs no backup must not be refused just because the \
         backup root happens to be a link: {text}"
    );
    assert_eq!(
        fs::read(env.dest().join("file.md")).unwrap(),
        b"new content",
        "the write must actually have happened: {text}"
    );
}

#[test]
fn a_linked_backup_root_leaves_a_prune_candidate_in_place_rather_than_backing_it_up_outside_target_dest()
 {
    // The third caller of `backup_path`, distinct from both tests above:
    // `plan_prunes` computes a prune candidate's backup the same way `resolve`
    // computes an entry's, and was found -- empirically, with this exact
    // fixture -- to have the identical escape before this function was fixed:
    // the file was pruned from `target.dest` and its only surviving copy was
    // filed outside it, through the link.
    let env = Env::new();
    env.write_source("modes/one.md", b"first mode");
    let manifest = env.write_manifest("copy = [\"modes\"]");
    env.apply(&manifest, &RealProbe);
    env.remove_source("modes/one.md");

    let outside = env.root().join("outside-backups");
    fs::create_dir_all(&outside).unwrap();
    safelink::create::create_dir_link(&outside, &env.dest().join(".agrafter-backups")).unwrap();

    let out = env.apply(&manifest, &RealProbe);
    let text = out.text();

    assert!(
        env.dest().join("modes/one.md").exists(),
        "a file that would need a backup must not be removed when that \
         backup could only be filed through a linked backup root: {text}"
    );
    assert!(
        fs::read_dir(&outside).unwrap().next().is_none(),
        "no backup may ever land outside target.dest through the link: {text}"
    );
    assert!(
        text.contains(&sep("left in place: modes/one.md")),
        "the refusal must be reported, not a silent no-op: {text}"
    );
    assert!(
        !text.contains("pruned: ") && !text.contains("would prune: "),
        "nothing may be reported as pruned when its backup could not be \
         safely filed: {text}"
    );
    assert!(
        matches!(
            safelink::classify::classify(&env.dest().join(".agrafter-backups")),
            Ok(safelink::classify::LinkClass::DirLink)
        ),
        "the link itself must be left alone, neither followed nor removed"
    );
}
