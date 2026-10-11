//! The canonical source lives on a removable or shared volume, so
//! "source path does not exist" is the single most likely real-world state,
//! not an exotic edge case.
//!
//! A missing source can never justify mutating the destination. The four
//! cases below get one test each:
//!
//!   1. source missing, dest absent      -> refuse, name the path, create nothing
//!   2. source missing, dest is a link   -> refuse, leave the stale link alone
//!   3. source missing, dest is real data-> refuse, leave the real data alone
//!   4. source present but empty         -> proceed, emptiness is legitimate
//!
//! Every assertion here uses `symlink_metadata`, never `exists()`:
//! `exists()` follows the link, so it reports `false` for a *dangling* link
//! and `true` only by accident. Against the exact failure these tests prevent
//! — creating a dangling link — an `exists()` assertion is vacuous.

use safelink::capability::ForcedProbe;
use safelink::classify::{LinkClass, classify};
use safelink::create::create_dir_link;
use safelink::materialize::{MaterializeOptions, materialize};
use safelink::op::{Entry, LinkOp};
use std::fs;
use std::io::ErrorKind;

/// True if *something* occupies `path`, link or not, dangling or not.
fn anything_at(path: &std::path::Path) -> bool {
    path.symlink_metadata().is_ok()
}

/// Row 1. `DirectoryLink` is used deliberately: unguarded, `create_dir_link`
/// **succeeds** against a missing target on POSIX and leaves a dangling link
/// behind. `FileCopy` would fail on its own for an unrelated reason and hide
/// whether the guard exists at all.
#[test]
fn refuses_to_create_a_link_to_a_missing_source() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("unmounted_volume/canonical");
    let dest = tmp.path().join("dest_link");
    assert!(!anything_at(&source), "precondition: source must be absent");

    let entry = Entry {
        source: source.clone(),
        dest: dest.clone(),
        op: LinkOp::DirectoryLink,
    };
    let err = materialize(&entry, &ForcedProbe(true), &MaterializeOptions::default())
        .expect_err("a missing source must never materialize anything");

    assert_eq!(err.kind(), ErrorKind::NotFound);
    assert!(
        err.to_string().contains(&source.display().to_string()),
        "the error must name the missing source path so the user knows which \
         volume to mount. Got: {err}"
    );
    assert!(
        !anything_at(&dest),
        "SAFETY VIOLATION: a dangling link was left at {}",
        dest.display()
    );
}

/// Row 3. Real data at the destination is never touched on the strength of a
/// missing source.
#[test]
fn leaves_an_existing_real_directory_untouched_when_the_source_is_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("unmounted_volume/canonical");

    let dest = tmp.path().join("dest_dir");
    fs::create_dir(&dest).unwrap();
    fs::write(dest.join("real.txt"), b"irreplaceable").unwrap();

    let entry = Entry {
        source: source.clone(),
        dest: dest.clone(),
        op: LinkOp::DirectoryLink,
    };
    let err = materialize(&entry, &ForcedProbe(true), &MaterializeOptions::default())
        .expect_err("must refuse");

    assert_eq!(err.kind(), ErrorKind::NotFound);
    assert!(
        dest.symlink_metadata().unwrap().is_dir(),
        "the real directory was replaced or removed"
    );
    assert_eq!(
        fs::read(dest.join("real.txt")).unwrap(),
        b"irreplaceable",
        "SAFETY VIOLATION: real data at the destination was destroyed because \
         the source volume happened to be unmounted"
    );
}

/// Row 2, the stale-link case — the one the sabotage check below targets.
///
/// An unmounted volume makes an otherwise perfect link dangle. Removing it
/// destroys a working configuration for no benefit: the volume may come back.
/// So `materialize` must fail *and* leave the link byte-for-byte alone.
///
/// The link is built by create-then-delete rather than by pointing at a path
/// that never existed: relying on `junction::create` accepting a missing
/// target is a Windows-only assumption this host cannot check, and this repo
/// has already been bitten once by exactly that class of guess.
///
/// Verified by sabotage, twice; both caught here and by no other test:
///   1. `remove_file(&entry.dest)` before the bail-out — the plausible
///      "tidy up the dangling link" mistake. Caught by the existence check.
///   2. Re-pointing the link at a placeholder directory instead of deleting
///      it. This one *survives* an existence check — the link object is
///      still there — and is caught only by the target comparison below.
///      That is why this test captures the target before the call rather
///      than settling for `assert!(dest.exists())`.
///   3. Replacing the link with a real directory of the same name. Caught by
///      the `classify` assertion. That assertion exists because the earlier
///      `is_symlink() || cfg!(windows)` form it replaced was unconditionally
///      true on Windows — vacuous on the one platform where swapping a
///      junction for a real directory actually destroys data.
#[test]
fn leaves_a_stale_link_to_the_missing_source_untouched() {
    let tmp = tempfile::tempdir().unwrap();

    let source = tmp.path().join("removable_volume");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("payload.txt"), b"on the removable volume").unwrap();

    let dest = tmp.path().join("dest_link");
    create_dir_link(&source, &dest).unwrap();
    assert!(
        dest.join("payload.txt").exists(),
        "precondition: the link must be live before the volume is unmounted"
    );

    // "Unmount the volume." We use `remove_dir_all` only here in the test to
    // simulate this — it is aimed at the source, not through any link.
    fs::remove_dir_all(&source).unwrap();
    assert!(!source.exists(), "precondition: source is now missing");

    // Captured before the call so the after-state is compared against a real
    // observation, not against a restatement of what we hope happened.
    #[cfg(unix)]
    let target_before = fs::read_link(&dest).unwrap();

    let entry = Entry {
        source: source.clone(),
        dest: dest.clone(),
        op: LinkOp::DirectoryLink,
    };
    let err = materialize(&entry, &ForcedProbe(true), &MaterializeOptions::default())
        .expect_err("a stale link must be reported, not silently re-created");
    assert_eq!(err.kind(), ErrorKind::NotFound);
    assert!(err.to_string().contains(&source.display().to_string()));

    assert!(
        anything_at(&dest),
        "SAFETY VIOLATION: the stale link at {} was deleted. The volume may \
         come back; removing it destroys a working configuration for no \
         benefit",
        dest.display()
    );
    // Not merely "something is there" — still a *link*. Asserted via the
    // crate's own `classify` so the check is real on both platforms: a
    // stale POSIX symlink lands on `FileLink` (its `fs::metadata` fails) and
    // a stale Windows junction on `DirLink`, so `!= NotALink` is the only
    // portable shape. A raw `is_symlink()` check would have to be waived on
    // Windows, which is precisely where replacing a junction with a real
    // directory is destructive — see tests/constraints.rs.
    assert_ne!(
        classify(&dest).unwrap(),
        LinkClass::NotALink,
        "SAFETY VIOLATION: the stale link at {} was replaced by a real \
         filesystem object instead of being left alone",
        dest.display()
    );
    #[cfg(unix)]
    assert_eq!(
        fs::read_link(&dest).unwrap(),
        target_before,
        "SAFETY VIOLATION: the stale link was re-pointed at a different target"
    );
}

/// Row 4. The guard must key on *non-existence*, not on emptiness. A
/// length- or entry-count-based check would wrongly refuse here.
#[test]
fn an_empty_source_directory_is_legitimate_and_proceeds() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("empty_but_present");
    fs::create_dir(&source).unwrap();
    let dest = tmp.path().join("dest_link");

    let entry = Entry {
        source,
        dest: dest.clone(),
        op: LinkOp::DirectoryLink,
    };
    let applied = materialize(&entry, &ForcedProbe(true), &MaterializeOptions::default())
        .expect("an empty source is legitimate, not a missing one");

    assert_eq!(applied, LinkOp::DirectoryLink);
    assert!(dest.is_dir(), "the link must be traversable");
    assert_eq!(fs::read_dir(&dest).unwrap().count(), 0);
}

/// Row 4, file flavor: a zero-byte source file is present, so it copies.
#[test]
fn an_empty_source_file_is_legitimate_and_proceeds() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("empty.txt");
    fs::write(&source, b"").unwrap();
    let dest = tmp.path().join("dest.txt");

    let entry = Entry {
        source,
        dest: dest.clone(),
        op: LinkOp::FileCopy,
    };
    let applied = materialize(&entry, &ForcedProbe(true), &MaterializeOptions::default())
        .expect("an empty file is legitimate");

    assert_eq!(applied, LinkOp::FileCopy);
    assert_eq!(fs::read(&dest).unwrap(), b"");
}
