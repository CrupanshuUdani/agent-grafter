//! Tests for `unlink`: what it removes, and — far more importantly — what it
//! refuses to remove.
//!
//! Every fixture here is built by really running `apply` first, so what
//! `unlink` is asked to remove is genuinely this tool's own output rather
//! than a hand-made approximation of it. The two commands then have to agree
//! about which destination paths they own, which is the property worth
//! testing; a fixture that laid the files out by hand would test only that
//! `unlink` removes files it was told about.
//!
//! **Two of these are safety tests, and both have been sabotage-verified**
//! (the standing project methodology: a safety test nobody has watched fail
//! is not yet evidence). Each is marked in place below with exactly what was
//! broken and what went red. In both cases the assertion watched was the
//! *content of the data that must survive*, not the exit code: an
//! exit-code-only check can stay green because an unrelated refusal happened
//! to fire instead, which would mean having watched the wrong thing fail.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Report lines carry paths formatted with the platform's own separator, so a
/// literal like `"modes/one.md"` only matches on Unix. Used wherever a test
/// checks report *text* for a multi-segment path.
fn sep(path: &str) -> String {
    path.replace('/', std::path::MAIN_SEPARATOR_STR)
}

/// A temp-dir sandbox laid out the way a real install is: `source/` holding
/// the canonical files, `dest/` standing in for a target root, and a manifest
/// between them.
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

    /// Write a file (creating parents) under `source/`.
    fn write_source(&self, rel: &str, bytes: &[u8]) {
        let path = self.source().join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create source parent");
        }
        fs::write(path, bytes).expect("write source file");
    }

    /// Write a file (creating parents) under `dest/` — something this tool
    /// did not put there.
    fn write_dest(&self, rel: &str, bytes: &[u8]) {
        let path = self.dest().join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create dest parent");
        }
        fs::write(path, bytes).expect("write dest file");
    }

    /// Write a manifest declaring `source = "source"` and one
    /// `[targets.example]` pointing at this env's `dest/`. The destination is
    /// spelled as a TOML *literal* string so a Windows path's backslashes are
    /// not read as escapes.
    fn write_manifest(&self, body: &str) {
        let toml = format!(
            "source = \"source\"\n{body}\n\n[targets.example]\ndest = '{}'\n",
            self.dest().display()
        );
        fs::write(self.root().join("agent-grafter.toml"), toml).expect("write manifest");
    }

    /// Write a manifest whose single target points somewhere other than this
    /// env's `dest/`. Used by the tests that aim a target at the source root.
    fn write_manifest_with_dest(&self, body: &str, dest: &Path) {
        let toml = format!(
            "source = \"source\"\n{body}\n\n[targets.example]\ndest = '{}'\n",
            dest.display()
        );
        fs::write(self.root().join("agent-grafter.toml"), toml).expect("write manifest");
    }

    /// Run the built binary inside this env's root.
    fn run(&self, args: &[&str]) -> Run {
        let output = Command::new(env!("CARGO_BIN_EXE_agent-grafter"))
            .args(args)
            .current_dir(self.root())
            .output()
            .expect("spawn agent-grafter");
        Run { output }
    }

    /// Really materialize the manifest, so the fixture `unlink` acts on is
    /// this tool's own output and not an imitation of it.
    fn apply(&self) -> Run {
        let run = self.run(&["apply"]);
        run.assert_exit(0);
        run
    }

    fn unlink(&self) -> Run {
        self.run(&["unlink"])
    }
}

struct Run {
    output: Output,
}

impl Run {
    fn assert_exit(&self, code: i32) -> &Self {
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

/// A deterministic snapshot of a tree: relative path, kind, and (for files)
/// content. Used to prove `--dry-run` changed nothing.
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
        // classify before any metadata question, and never descend into a
        // link — the same rule the code under test follows.
        let kind = match safelink::classify::classify(&path).expect("classify") {
            safelink::classify::LinkClass::DirLink | safelink::classify::LinkClass::FileLink => {
                Kind::Link(fs::read_link(&path).unwrap_or_default())
            }
            safelink::classify::LinkClass::NotALink => {
                let meta = fs::symlink_metadata(&path).expect("symlink_metadata");
                if meta.is_dir() {
                    walk(root, &path, out);
                    Kind::Dir
                } else {
                    Kind::File(fs::read(&path).unwrap_or_default())
                }
            }
        };
        out.insert(rel, kind);
    }
}

// ---------------------------------------------------------------------------
// The link path. `safelink::remove::remove_link` already guards it; these
// pin that `unlink` actually routes through it and refuses the rest.
// ---------------------------------------------------------------------------

#[test]
fn a_materialized_link_entry_is_removed_and_its_source_survives() {
    let env = Env::new();
    env.write_source("memory/note.md", b"remembered");
    env.write_manifest("link = [\"memory\"]");
    env.apply();
    assert!(
        env.dest().join("memory/note.md").exists(),
        "apply must link"
    );

    let run = env.unlink();
    run.assert_exit(0);
    assert!(
        run.stdout().contains("removed link: memory"),
        "unlink must say it removed the link, got: {}",
        run.stdout()
    );

    assert!(
        !env.dest().join("memory").exists(),
        "the link itself must be gone"
    );
    assert_eq!(
        fs::read(env.source().join("memory/note.md")).unwrap(),
        b"remembered",
        "removing a link must never delete through it into the source"
    );
}

#[test]
fn an_unrelated_real_directory_where_a_link_was_declared_is_refused() {
    // The brief's own example of the refusal that must never become a
    // deletion. `apply` refuses to materialize over this directory too, so
    // the fixture is built without it.
    let env = Env::new();
    env.write_source("memory/note.md", b"remembered");
    env.write_manifest("link = [\"memory\"]");
    env.write_dest("memory/hand-written.md", b"mine");

    let run = env.unlink();
    run.assert_exit(4);
    assert!(
        run.stdout().contains("refused: memory"),
        "got: {}",
        run.stdout()
    );
    assert_eq!(
        fs::read(env.dest().join("memory/hand-written.md")).unwrap(),
        b"mine",
        "a real directory where a link was declared must be left entirely alone"
    );
}

#[test]
fn a_link_entry_whose_source_is_gone_still_has_its_link_removed() {
    // A user who has already deleted their canonical tree is exactly the user
    // who most needs the leftover links cleaned up, and removing a link
    // destroys nothing whether or not the source survives.
    let env = Env::new();
    env.write_source("memory/note.md", b"remembered");
    env.write_manifest("link = [\"memory\"]");
    env.apply();

    fs::remove_file(env.source().join("memory/note.md")).expect("remove source file");
    fs::remove_dir(env.source().join("memory")).expect("remove source dir");

    env.unlink().assert_exit(0);
    assert!(
        !env.dest().join("memory").exists(),
        "the dangling link must still be removed"
    );
}

// ---------------------------------------------------------------------------
// The copy path. This is where the risk actually lives: real files, with no
// link indirection to protect them.
// ---------------------------------------------------------------------------

#[test]
fn a_materialized_copy_directory_has_its_files_removed_and_nothing_else() {
    let env = Env::new();
    env.write_source("modes/one.md", b"first");
    env.write_source("modes/two.md", b"second");
    env.write_manifest("copy = [\"modes\"]");
    env.apply();

    // Two things `unlink` must not touch: a file the user dropped inside the
    // materialized directory, and a file sitting beside it at the target
    // root. Neither is declared, so neither is ever even named — `unlink`
    // enumerates the *source*, never the destination.
    env.write_dest("modes/theirs.md", b"not ours");
    env.write_dest("unrelated.md", b"also not ours");

    let run = env.unlink();
    run.assert_exit(0);
    assert!(
        run.stdout()
            .contains(&format!("removed: {}", sep("modes/one.md"))),
        "got: {}",
        run.stdout()
    );

    assert!(!env.dest().join("modes/one.md").exists());
    assert!(!env.dest().join("modes/two.md").exists());
    assert_eq!(
        fs::read(env.dest().join("modes/theirs.md")).unwrap(),
        b"not ours",
        "an undeclared file inside a materialized directory must survive"
    );
    assert_eq!(
        fs::read(env.dest().join("unrelated.md")).unwrap(),
        b"also not ours",
        "nothing outside a declared entry's own destination may be touched"
    );
    assert!(
        env.dest().join("modes").is_dir(),
        "the directory itself is deliberately left behind, mirroring prune"
    );
}

/// **SAFETY TEST — sabotage-verified.**
///
/// The copy path has no link indirection protecting it, so the only thing
/// standing between `unlink` and a user's edited file is the byte comparison
/// against the declared source.
///
/// Sabotage, and what it turned up: forcing the *plan-time* comparison
/// (`unlink::decide_over_real_data`) to `true` was **not enough to delete the
/// file** — `remove_one`'s independent re-check caught it and the run came
/// out `left in place`, exit 4. That is the defense-in-depth working, and it
/// is why this test asserts the refusal's wording as well as its effect: only
/// the wording assertion moved. Forcing *both* comparisons then deleted the
/// hand-edited file, and this test went red on the content assertion below.
/// Both reverted.
#[test]
fn a_user_edited_copy_is_refused_and_never_removed() {
    let env = Env::new();
    env.write_source("modes/one.md", b"first");
    env.write_source("modes/two.md", b"second");
    env.write_manifest("copy = [\"modes\"]");
    env.apply();

    // The user edits the materialized copy. Its content now exists nowhere
    // else: removing it destroys the only copy.
    env.write_dest("modes/one.md", b"edited by hand, not in the source");

    let run = env.unlink();

    // The content assertion comes first, deliberately. An exit-code-only
    // check could stay green because some unrelated refusal happened to fire
    // instead, which would mean watching the wrong thing fail.
    assert_eq!(
        fs::read(env.dest().join("modes/one.md")).unwrap(),
        b"edited by hand, not in the source",
        "an edited copy must never be removed"
    );
    run.assert_exit(4);
    assert!(
        run.stdout().contains("byte for byte"),
        "the refusal must say why, got: {}",
        run.stdout()
    );
    assert!(
        !env.dest().join("modes/two.md").exists(),
        "one entry's refusal must not stop the others"
    );
}

/// **SAFETY TEST — sabotage-verified.**
///
/// The scope escape, and the highest-risk path in the command. A declared
/// entry's destination is reached component by component, classifying each
/// level, because the kernel resolves a joined path *through* an intermediate
/// link before `classify` ever sees the end of it. Here `dest/modes` is a
/// directory link pointing at real data outside the target, and that data
/// contains a file whose bytes match the declared source exactly — so the
/// byte check alone would wave it straight through.
///
/// Sabotage: the `classify` gate in `unlink::resolve_removal_dest`'s
/// intermediate loop was replaced with `Ok(_) => {}`. This test went red on
/// the `outside/one.md` content assertion below — the file outside the target
/// root was deleted through the link, exactly as the gate exists to prevent.
/// Reverted.
///
/// The assertions are on the *content of the data outside the target*, and
/// they come before the exit-code check for the reason this module's own doc
/// comment gives.
#[test]
fn a_directory_link_standing_in_for_a_copied_directory_is_never_deleted_through() {
    let env = Env::new();
    env.write_source("modes/one.md", b"first");
    env.write_manifest("copy = [\"modes\"]");
    env.apply();

    // The user's real data, elsewhere on the disk. `one.md` deliberately
    // holds exactly the declared source's bytes.
    let outside = env.root().join("outside");
    fs::create_dir(&outside).expect("create outside dir");
    fs::write(outside.join("one.md"), b"first").expect("write outside file");
    fs::write(outside.join("private.md"), b"theirs alone").expect("write outside file");

    // Swap the materialized directory for a link to it. Built with the engine
    // itself — link creation is never reimplemented in a test either.
    fs::remove_file(env.dest().join("modes/one.md")).expect("remove copy");
    fs::remove_dir(env.dest().join("modes")).expect("remove dir");
    safelink::create::create_dir_link(&outside, &env.dest().join("modes")).expect("create link");

    let run = env.unlink();

    assert_eq!(
        fs::read(outside.join("one.md")).unwrap(),
        b"first",
        "unlink must never delete through a link, even when the bytes match"
    );
    assert_eq!(
        fs::read(outside.join("private.md")).unwrap(),
        b"theirs alone",
        "nothing behind the link may be touched"
    );
    run.assert_exit(4);
    assert!(
        run.stdout().contains("is a link"),
        "the refusal must name the link it declined to resolve through, got: {}",
        run.stdout()
    );
}

// ---------------------------------------------------------------------------
// `--dry-run` honesty and the exit-code parity that makes it useful.
// ---------------------------------------------------------------------------

#[test]
fn dry_run_reports_what_it_would_remove_and_changes_nothing() {
    let env = Env::new();
    env.write_source("memory/note.md", b"remembered");
    env.write_source("modes/one.md", b"first");
    env.write_manifest("link = [\"memory\"]\ncopy = [\"modes\"]");
    env.apply();

    let before = snapshot(env.root());
    let run = env.run(&["unlink", "--dry-run"]);
    run.assert_exit(0);

    let out = run.stdout();
    assert!(
        out.contains("would remove link: memory"),
        "dry-run must read as hypothetical, got: {out}"
    );
    assert!(
        out.contains(&format!("would remove: {}", sep("modes/one.md"))),
        "got: {out}"
    );
    assert!(
        out.contains("example:"),
        "dry-run must name which target it would act on, got: {out}"
    );
    assert_eq!(
        before,
        snapshot(env.root()),
        "--dry-run must not touch the filesystem"
    );
}

#[test]
fn a_dry_run_reports_the_same_exit_code_a_real_run_would() {
    // A CI gate that runs `unlink --dry-run` to detect refusals must see the
    // same code the real run produces, or dry-run hides the one thing the
    // gate exists to catch.
    let env = Env::new();
    env.write_source("memory/note.md", b"remembered");
    env.write_manifest("link = [\"memory\"]");
    env.write_dest("memory/hand-written.md", b"mine");

    env.run(&["unlink", "--dry-run"]).assert_exit(4);
    env.unlink().assert_exit(4);
    assert_eq!(
        fs::read(env.dest().join("memory/hand-written.md")).unwrap(),
        b"mine",
        "neither run may touch the occupying directory"
    );
}

// ---------------------------------------------------------------------------
// Idempotence, backups, and the refusals reached before a destination is even
// resolved.
// ---------------------------------------------------------------------------

#[test]
fn a_second_unlink_reports_nothing_present_and_still_succeeds() {
    let env = Env::new();
    env.write_source("modes/one.md", b"first");
    env.write_manifest("copy = [\"modes\"]");
    env.apply();

    env.unlink().assert_exit(0);
    let again = env.unlink();
    again.assert_exit(0);
    assert!(
        again
            .stdout()
            .contains(&format!("not present: {}", sep("modes/one.md"))),
        "a re-run must say the work is already done, not invent work: {}",
        again.stdout()
    );
}

#[test]
fn backups_are_left_in_place_and_named_in_the_report() {
    let env = Env::new();
    env.write_source("modes/one.md", b"first");
    env.write_manifest("copy = [\"modes\"]");
    // A file the user already had, which `apply` replaces and therefore backs
    // up. That backup is the only surviving copy of "theirs".
    env.write_dest("modes/one.md", b"theirs");
    env.apply();

    let backups = env.dest().join(".agrafter-backups");
    assert!(backups.is_dir(), "apply must have taken a backup");

    let run = env.unlink();
    run.assert_exit(0);
    assert!(
        run.stdout().contains("backups left in place"),
        "orphaning the backups silently is the surprise this must avoid: {}",
        run.stdout()
    );
    assert!(
        backups.is_dir(),
        "unlink must neither remove the backups nor restore them"
    );
    assert!(
        !env.dest().join("modes/one.md").exists(),
        "the materialized copy itself is still removed"
    );
}

#[test]
fn a_manifest_with_no_targets_refuses_rather_than_reporting_success() {
    // The failure shape this whole command was rewritten to stop producing:
    // reporting exit 0 for an uninstall that removed nothing.
    let env = Env::new();
    env.write_source("modes/one.md", b"first");
    fs::write(
        env.root().join("agent-grafter.toml"),
        "source = \"source\"\ncopy = [\"modes\"]\n",
    )
    .expect("write manifest");

    let run = env.unlink();
    run.assert_exit(4);
    assert!(
        run.stdout().contains("no [targets.*]"),
        "got: {}",
        run.stdout()
    );
}

#[test]
fn a_target_root_that_does_not_exist_refuses_rather_than_reporting_success() {
    let env = Env::new();
    env.write_source("modes/one.md", b"first");
    env.write_manifest("copy = [\"modes\"]");
    fs::remove_dir(env.dest()).expect("remove dest root");

    env.unlink().assert_exit(4);
}

#[test]
fn a_declared_entry_naming_the_backup_directory_is_refused_not_removed() {
    // `.agrafter-backups` is this tool's own bookkeeping, and nothing is ever
    // removed through or as it — including when a manifest names it outright.
    let env = Env::new();
    env.write_source(".agrafter-backups/20240101T000000Z/note.md", b"a backup");
    env.write_manifest("copy = [\".agrafter-backups/20240101T000000Z/note.md\"]");
    env.write_dest(".agrafter-backups/20240101T000000Z/note.md", b"a backup");

    let run = env.unlink();
    run.assert_exit(4);
    // Matched against the wording unique to `resolve_removal_dest`'s
    // BACKUP_DIR arm, not merely against the directory's name: the name
    // appears in several other refusals too, and a test that accepted any of
    // them would pin a coincidence rather than this guard.
    assert!(
        run.stdout()
            .contains("nothing is ever removed through or as it"),
        "the refusal must be the backup-directory one, got: {}",
        run.stdout()
    );
    assert_eq!(
        fs::read(
            env.dest()
                .join(".agrafter-backups/20240101T000000Z/note.md")
        )
        .unwrap(),
        b"a backup",
        "nothing inside the backup directory may be removed by a declared entry"
    );
}

#[test]
fn a_copy_entry_whose_source_is_gone_is_refused_not_guessed_at() {
    // Without the source there is nothing to compare bytes against, so
    // nothing at the destination can be confirmed to be this tool's.
    let env = Env::new();
    env.write_source("modes/one.md", b"first");
    env.write_manifest("copy = [\"modes\"]");
    env.apply();

    fs::remove_file(env.source().join("modes/one.md")).expect("remove source file");
    fs::remove_dir(env.source().join("modes")).expect("remove source dir");

    let run = env.unlink();
    run.assert_exit(4);
    assert_eq!(
        fs::read(env.dest().join("modes/one.md")).unwrap(),
        b"first",
        "a copy with no source to verify it against must be left in place"
    );
}

// ---------------------------------------------------------------------------
// Ownership, as distinct from content.
//
// Byte equality answers "same content". Removal needs a different question
// answered: "is this a separate artifact from the source, or is it the
// source?" That is filesystem identity, and byte equality answers it
// vacuously in the affirmative when the two paths are one object — a file is
// always byte-identical to itself.
//
// Both tests below were watched FAILING against the pre-fix code, each
// destroying the only copy of the content, before the identity guard existed.
// ---------------------------------------------------------------------------

/// **SAFETY TEST — regression, fix round 1.**
///
/// A target whose `dest` *is* the source root. Every declared entry then
/// resolves to its own source, and the content comparison says "identical"
/// because it is literally the same file. Pre-fix, `unlink` reported
/// `removed`, exited 0, and deleted the user's canonical tree.
#[test]
fn a_target_whose_dest_is_the_source_root_is_refused_not_removed() {
    let env = Env::new();
    env.write_source("modes/one.md", b"first");
    env.write_source("memory/note.md", b"remembered");
    let source_root = env.source();
    env.write_manifest_with_dest("copy = [\"modes\"]\nlink = [\"memory\"]", &source_root);

    let run = env.unlink();

    // Content first, exit code second, for the reason this module's doc
    // comment gives.
    assert_eq!(
        fs::read(env.source().join("modes/one.md")).unwrap(),
        b"first",
        "unlink must never delete the canonical source it compares against"
    );
    assert!(
        env.source().join("memory/note.md").exists(),
        "nor anything else in the source tree"
    );
    run.assert_exit(4);
}

/// **SAFETY TEST — regression, fix round 1.**
///
/// A declared source entry that is a link *into* the destination. The source
/// side then resolves to the very file at the destination, so the content
/// comparison is again vacuously true — and unlike the case above, the two
/// roots are entirely disjoint, so no amount of comparing `dest` against the
/// source *root* would catch it. Pre-fix, `unlink` deleted the only real copy
/// and left the source entry dangling.
#[test]
fn a_source_entry_linking_into_the_destination_is_refused_not_removed() {
    let env = Env::new();
    // The only real copy of this content lives under `dest/`.
    env.write_dest("shared/one.md", b"the only copy");
    // The declared source entry is a link pointing straight at it. Built with
    // the engine itself — link creation is never reimplemented in a test.
    safelink::create::create_dir_link(&env.dest().join("shared"), &env.source().join("shared"))
        .expect("create source link");
    env.write_manifest("copy = [\"shared\"]");

    let run = env.unlink();

    assert_eq!(
        fs::read(env.dest().join("shared/one.md")).unwrap(),
        b"the only copy",
        "unlink must not delete a file the declared source itself resolves to"
    );
    assert!(
        fs::read(env.source().join("shared/one.md")).is_ok(),
        "the source entry must not be left dangling"
    );
    run.assert_exit(4);
}
