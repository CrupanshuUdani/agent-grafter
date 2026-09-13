//! Tests for backup retention: which backup directories a sweep names for
//! removal, and how one is actually removed.
//!
//! Every fixture here fabricates directory *names* rather than producing them
//! by running `apply` repeatedly and waiting for the clock to tick. Retention
//! is a question about names, so a test of it has no business sleeping — and a
//! test that did could only ever build a handful of one-second-apart entries,
//! never the twelve the limit needs to be exercised at all.

use std::fs;
use std::path::{Path, PathBuf};

use agent_grafter::retention::{KEEP_BACKUPS, plan_retention, remove_backup_dir};

/// Create `names` as directories inside a fresh `.agrafter-backups/`, each
/// holding one file, and return the directory.
fn backups_with(names: &[&str]) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let backups = tmp.path().join(".agrafter-backups");
    fs::create_dir(&backups).expect("create backups dir");
    for name in names {
        let dir = backups.join(name);
        fs::create_dir(&dir).expect("create backup dir");
        fs::write(dir.join("AGENTS.md"), name.as_bytes()).expect("write backed-up file");
    }
    (tmp, backups)
}

/// Twelve validly named, one-day-apart backup directories, oldest first.
fn twelve_names() -> Vec<String> {
    (1..=12)
        .map(|day| format!("202401{day:02}T000000Z"))
        .collect()
}

fn names_of(paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect()
}

// ---------------------------------------------------------------------------
// Required test 7. Retention keeps the most recent 10.
// ---------------------------------------------------------------------------

#[test]
fn retention_names_only_the_oldest_entries_beyond_the_limit() {
    let names = twelve_names();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let (_tmp, backups) = backups_with(&refs);

    let removals = plan_retention(&backups, KEEP_BACKUPS, None).expect("plan retention");

    assert_eq!(
        names_of(&removals),
        vec![names[0].clone(), names[1].clone()],
        "with 12 on disk and a limit of 10, exactly the 2 oldest go"
    );
    // And nothing was touched: this function decides, it does not act.
    assert_eq!(
        fs::read_dir(&backups).unwrap().count(),
        12,
        "planning retention must not remove anything"
    );
}

#[test]
fn this_runs_own_backup_directory_counts_toward_the_limit_but_is_never_removed() {
    // The prospective directory is the newest of all, so it survives — and it
    // pushes the effective count to 13, so one more old entry has to go than
    // the `None` case above named.
    let names = twelve_names();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let (_tmp, backups) = backups_with(&refs);

    let new_run = "20240201T000000Z";
    let removals = plan_retention(&backups, KEEP_BACKUPS, Some(new_run)).expect("plan retention");

    assert_eq!(
        names_of(&removals),
        vec![names[0].clone(), names[1].clone(), names[2].clone()],
        "counting this run's own prospective directory must retire one more"
    );
    assert!(
        !removals.iter().any(|p| p.ends_with(new_run)),
        "the directory this run is about to write into must never be named \
         for removal: {removals:?}"
    );
}

#[test]
fn a_prospective_directory_that_already_exists_is_not_counted_twice() {
    // Two `apply` runs inside the same second share a backup directory name.
    let names = twelve_names();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let (_tmp, backups) = backups_with(&refs);

    let removals =
        plan_retention(&backups, KEEP_BACKUPS, Some(&names[11])).expect("plan retention");

    assert_eq!(
        names_of(&removals),
        vec![names[0].clone(), names[1].clone()],
        "re-using an existing directory adds nothing to the count"
    );
}

#[test]
fn nothing_is_removed_when_the_limit_is_not_reached() {
    let (_tmp, backups) = backups_with(&["20240101T000000Z", "20240102T000000Z"]);

    assert!(
        plan_retention(&backups, KEEP_BACKUPS, Some("20240103T000000Z"))
            .expect("plan retention")
            .is_empty()
    );
}

#[test]
fn a_backups_directory_that_does_not_exist_yet_is_not_an_error() {
    let tmp = tempfile::tempdir().unwrap();

    let removals = plan_retention(&tmp.path().join(".agrafter-backups"), KEEP_BACKUPS, None)
        .expect("a missing backups directory is the normal pre-first-backup state");

    assert!(removals.is_empty());
}

// ---------------------------------------------------------------------------
// Required test 8. Retention ignores anything that is not one of its own
// timestamp-named directories, whatever position a lexical sort would give it.
// ---------------------------------------------------------------------------

#[test]
fn entries_that_are_not_timestamp_named_are_neither_counted_nor_removed() {
    let names = twelve_names();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let (_tmp, backups) = backups_with(&refs);

    // Three intruders, chosen to sort *before* every real timestamp (so a
    // naive sort-then-truncate would delete them first) and *after* every real
    // one (so a naive implementation would count them as the newest and retire
    // real backups instead).
    fs::write(backups.join("00-read-me.txt"), b"a user's own note").unwrap();
    fs::create_dir(backups.join("0000-archive")).unwrap();
    fs::write(backups.join("0000-archive/keep.md"), b"hand-filed").unwrap();
    fs::create_dir(backups.join("zzzz-scratch")).unwrap();

    let removals = plan_retention(&backups, KEEP_BACKUPS, None).expect("plan retention");

    assert_eq!(
        names_of(&removals),
        vec![names[0].clone(), names[1].clone()],
        "the intruders must not shift which real backups are retired"
    );

    // Acting on the plan leaves every one of them exactly where it was.
    for path in &removals {
        remove_backup_dir(path).expect("remove planned backup");
    }
    assert_eq!(
        fs::read(backups.join("00-read-me.txt")).unwrap(),
        b"a user's own note"
    );
    assert_eq!(
        fs::read(backups.join("0000-archive/keep.md")).unwrap(),
        b"hand-filed"
    );
    assert!(backups.join("zzzz-scratch").is_dir());
}

#[test]
fn a_link_named_like_a_backup_directory_is_left_alone() {
    // Not counted and not removed: following it would delete whatever it
    // points at, which by definition is not this tool's bookkeeping.
    let names = twelve_names();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let (tmp, backups) = backups_with(&refs);

    let real = tmp.path().join("somewhere-real");
    fs::create_dir(&real).unwrap();
    fs::write(real.join("precious.md"), b"not a backup").unwrap();
    safelink::create::create_dir_link(&real, &backups.join("20230101T000000Z")).unwrap();

    let removals = plan_retention(&backups, KEEP_BACKUPS, None).expect("plan retention");

    assert_eq!(
        names_of(&removals),
        vec![names[0].clone(), names[1].clone()],
        "a link must not be counted, even though its name is the oldest of all"
    );
    assert_eq!(fs::read(real.join("precious.md")).unwrap(), b"not a backup");
}

// ---------------------------------------------------------------------------
// Required test 9. The removal itself: a nested backup tree goes end to end,
// through a hand-rolled walk rather than the standard library's recursive
// delete (which the standing grep over this crate proves is absent).
// ---------------------------------------------------------------------------

#[test]
fn removing_a_backup_directory_clears_a_nested_tree_end_to_end() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("20240101T000000Z");
    // The shape a directory-copy entry's backup actually takes: relative paths
    // mirrored under the timestamp, several levels deep.
    fs::create_dir_all(dir.join("modes/nested/deeper")).unwrap();
    fs::write(dir.join("AGENTS.md"), b"top level").unwrap();
    fs::write(dir.join("modes/one.md"), b"one").unwrap();
    fs::write(dir.join("modes/nested/two.md"), b"two").unwrap();
    fs::write(dir.join("modes/nested/deeper/three.md"), b"three").unwrap();
    // An empty directory too: `remove_dir` has to unwind these as well.
    fs::create_dir(dir.join("modes/empty")).unwrap();

    remove_backup_dir(&dir).expect("remove the backup directory");

    assert!(!dir.exists(), "the whole tree must be gone");
    assert!(
        tmp.path().is_dir(),
        "and nothing above it may have been touched"
    );
}

#[test]
fn removing_a_backup_directory_refuses_a_link_inside_it_instead_of_deleting_through() {
    // Nothing should ever put a link inside a backup directory — only plain
    // files are ever backed up. Finding one means something is wrong, and the
    // response is to stop, not to follow it.
    let tmp = tempfile::tempdir().unwrap();
    let outside = tmp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("precious.md"), b"real user data").unwrap();

    let dir = tmp.path().join("20240101T000000Z");
    fs::create_dir(&dir).unwrap();
    fs::write(dir.join("AGENTS.md"), b"backed up").unwrap();
    safelink::create::create_dir_link(&outside, &dir.join("smuggled")).unwrap();

    let err = remove_backup_dir(&dir).expect_err("a link inside must stop the removal");
    assert!(
        err.to_string().contains("smuggled"),
        "the refusal must name what stopped it: {err}"
    );

    assert_eq!(
        fs::read(outside.join("precious.md")).unwrap(),
        b"real user data",
        "nothing may be deleted through the link"
    );
    assert!(
        dir.join("smuggled").exists(),
        "and the link itself is left where it was"
    );
}

#[test]
fn removing_a_backup_directory_refuses_when_the_directory_itself_is_a_link() {
    let tmp = tempfile::tempdir().unwrap();
    let real = tmp.path().join("real");
    fs::create_dir(&real).unwrap();
    fs::write(real.join("precious.md"), b"real user data").unwrap();

    let link: &Path = &tmp.path().join("20240101T000000Z");
    safelink::create::create_dir_link(&real, link).unwrap();

    remove_backup_dir(link).expect_err("a link must never be swept as a backup directory");
    assert_eq!(
        fs::read(real.join("precious.md")).unwrap(),
        b"real user data"
    );
}
