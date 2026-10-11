//! The destination side. A missing *source* is one class of failure; the
//! destination has two failure modes of its own, and they need opposite
//! treatments.
//!
//! The two op kinds used to disagree about the first one, by accident
//! rather than by design: `copy_atomic` has always called
//! `create_dir_all` on the destination's parent, so `FileCopy` silently
//! built whole missing hierarchies, while `create_dir_link` never did, so
//! `DirectoryLink` failed outright on the exact same manifest shape. Nothing
//! in the code said which was correct.
//!
//! The rows, one or more tests each:
//!
//!   1. parent missing, default opts, DirectoryLink -> creates it, link works
//!   2. parent missing, default opts, FileCopy      -> creates it, copy works
//!   3. parent missing, strict opts,  DirectoryLink -> refuse, create nothing
//!   4. parent missing, strict opts,  FileCopy      -> refuse, create nothing
//!   5. parent unwritable, DirectoryLink -> PermissionDenied, names the dest
//!   6. parent unwritable, FileCopy      -> PermissionDenied, names the dest
//!   7. every failure above leaves an existing destination byte-identical
//!
//! Rows 3 and 4 are the ones that assert a *choice*. Rows 5 and 6 are not
//! configurable: permission denied is a fact about the machine, and there is
//! no flag that makes silently working around it correct.
//!
//! PLATFORM HONESTY: rows 5 and 6 are `#[cfg(unix)]`-only. Making a
//! directory unwritable on Windows means manipulating ACLs (a DACL deny-ACE
//! via `SetNamedSecurityInfo` or an `icacls` shell-out), which is a
//! substantially heavier mechanism than a `chmod` and pulls in a dependency
//! this crate does not otherwise need. The permission-denied path is
//! therefore NOT exercised on Windows, and a green Windows CI run is not
//! evidence that it works there. We explicitly state which platforms a claim
//! covers rather than letting a green check imply all of them.
//!
//! ROOT HONESTY: rows 5, 6 and 7 additionally self-skip when the process can
//! write to a 0o555 directory, i.e. when running as root — DAC write checks
//! are bypassed for UID 0, so the lock does not lock. A skipped run still
//! reports as a PASS, and the banner explaining why goes to stderr, which
//! `cargo test` captures. Before trusting these tests — in particular before
//! using them to verify a sabotage — run:
//!
//!     cargo test --all-targets -- --nocapture
//!
//! and confirm the "SKIPPED ... running as root" banner is ABSENT.

use safelink::capability::ForcedProbe;
use safelink::materialize::{MaterializeOptions, materialize};
use safelink::op::{Entry, LinkOp};
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

/// Refuse rather than auto-create. The non-default half of the flag.
fn strict() -> MaterializeOptions {
    MaterializeOptions {
        create_missing_parents: false,
    }
}

/// True if *something* occupies `path`, link or not, dangling or not.
/// `exists()` follows links and would report `false` for a dangling one.
fn anything_at(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}

/// A source directory with one identifiable file in it.
fn source_dir(tmp: &Path) -> std::path::PathBuf {
    let source = tmp.join("source_dir");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("inside.txt"), b"payload").unwrap();
    source
}

/// A source file with identifiable content.
fn source_file(tmp: &Path) -> std::path::PathBuf {
    let source = tmp.join("source.txt");
    fs::write(&source, b"payload").unwrap();
    source
}

// ---------------------------------------------------------------------------
// Rows 1 and 2: default options auto-create the hierarchy, identically.
// ---------------------------------------------------------------------------

/// Row 1. The behavior that **changed**: `create_dir_link` used to be handed a
/// destination whose parent did not exist and fail with a raw `NotFound`.
/// Under the default it now matches `FileCopy`.
///
/// The parent is nested two levels deep on purpose. A one-level parent would
/// pass against a `create_dir` implementation as well as a `create_dir_all`
/// one, so it could not tell the two apart.
#[test]
fn directory_link_creates_a_missing_parent_hierarchy_by_default() {
    let tmp = tempfile::tempdir().unwrap();
    let source = source_dir(tmp.path());
    let dest = tmp.path().join("a/b/c/dest_link");

    let entry = Entry {
        source,
        dest: dest.clone(),
        op: LinkOp::DirectoryLink,
    };
    let applied = materialize(&entry, &ForcedProbe(true), &MaterializeOptions::default())
        .expect("the default must create the missing parent hierarchy");

    assert_eq!(applied, LinkOp::DirectoryLink, "no silent downgrade");
    assert!(
        tmp.path().join("a/b/c").is_dir(),
        "the full hierarchy must exist, not just the innermost level"
    );
    // Not merely "the link object exists" — traversable, so the link is real.
    assert_eq!(
        fs::read(dest.join("inside.txt")).unwrap(),
        b"payload",
        "the created link must be traversable to the source's contents"
    );
}

/// Row 2. This already worked, via `copy_atomic`'s unconditional
/// `create_dir_all`. The test exists to LOCK IT IN across the API change:
/// the parent policy moved into `materialize`, and without this test the move
/// could have quietly made the default strict for `FileCopy` and every other
/// test in the suite would still pass.
#[test]
fn file_copy_creates_a_missing_parent_hierarchy_by_default() {
    let tmp = tempfile::tempdir().unwrap();
    let source = source_file(tmp.path());
    let dest = tmp.path().join("a/b/c/dest.txt");

    let entry = Entry {
        source,
        dest: dest.clone(),
        op: LinkOp::FileCopy,
    };
    let applied = materialize(&entry, &ForcedProbe(true), &MaterializeOptions::default())
        .expect("the default must create the missing parent hierarchy");

    assert_eq!(applied, LinkOp::FileCopy);
    assert!(tmp.path().join("a/b/c").is_dir());
    assert_eq!(fs::read(&dest).unwrap(), b"payload");
}

/// The downgraded `FileSymlink -> FileCopy` path reaches `copy_atomic`
/// through a different arm of the `match`. The parent policy is applied
/// before the `match`, so it must hold there too — asserted rather than
/// assumed, because "before the match" is an implementation detail a future
/// refactor could move into the arms.
#[test]
fn downgraded_file_symlink_creates_a_missing_parent_hierarchy_by_default() {
    let tmp = tempfile::tempdir().unwrap();
    let source = source_file(tmp.path());
    let dest = tmp.path().join("a/b/c/dest.txt");

    let entry = Entry {
        source,
        dest: dest.clone(),
        op: LinkOp::FileSymlink,
    };
    // ForcedProbe(false) reaches the copy fallback even on an elevated host.
    let applied = materialize(&entry, &ForcedProbe(false), &MaterializeOptions::default()).unwrap();

    assert_eq!(applied, LinkOp::FileCopy, "must downgrade, not fail");
    assert_eq!(fs::read(&dest).unwrap(), b"payload");
}

// ---------------------------------------------------------------------------
// Rows 3 and 4: create_missing_parents=false refuses, identically, and
// creates nothing.
// ---------------------------------------------------------------------------

/// Row 3. Refusing is not enough — it must refuse *having created nothing*.
/// A `create_dir_all` followed by a failed link would also return `Err`, and
/// an error-only assertion would call that a pass while the manifest had
/// already built a tree at a typo'd path.
#[test]
fn directory_link_refuses_a_missing_parent_when_creation_is_disabled() {
    let tmp = tempfile::tempdir().unwrap();
    let source = source_dir(tmp.path());
    let missing_parent = tmp.path().join("a/b/c");
    let dest = missing_parent.join("dest_link");

    let entry = Entry {
        source,
        dest: dest.clone(),
        op: LinkOp::DirectoryLink,
    };
    let err = materialize(&entry, &ForcedProbe(true), &strict())
        .expect_err("create_missing_parents=false must refuse a missing parent");

    assert_eq!(err.kind(), ErrorKind::NotFound);
    assert!(
        err.to_string()
            .contains(&missing_parent.display().to_string()),
        "the error must name the missing parent directory so the user knows \
         what to create. Got: {err}"
    );
    assert!(
        !anything_at(&tmp.path().join("a")),
        "SAFETY VIOLATION: the hierarchy was created despite \
         create_missing_parents=false"
    );
    assert!(!anything_at(&dest), "nothing may exist at the destination");
}

/// Row 4. The behavior that **changed** on this side: `copy_atomic` ignores
/// the flag entirely — it has no access to it — so a `FileCopy` used to build
/// the hierarchy no matter what the caller asked for.
/// Passing now requires the policy to sit in `materialize`, ahead of the
/// call, rather than inside `copy_atomic`.
#[test]
fn file_copy_refuses_a_missing_parent_when_creation_is_disabled() {
    let tmp = tempfile::tempdir().unwrap();
    let source = source_file(tmp.path());
    let missing_parent = tmp.path().join("a/b/c");
    let dest = missing_parent.join("dest.txt");

    let entry = Entry {
        source,
        dest: dest.clone(),
        op: LinkOp::FileCopy,
    };
    let err = materialize(&entry, &ForcedProbe(true), &strict())
        .expect_err("create_missing_parents=false must refuse a missing parent");

    assert_eq!(err.kind(), ErrorKind::NotFound);
    assert!(
        err.to_string()
            .contains(&missing_parent.display().to_string()),
        "the error must name the missing parent directory. Got: {err}"
    );
    assert!(
        !anything_at(&tmp.path().join("a")),
        "SAFETY VIOLATION: copy_atomic's unconditional create_dir_all ran \
         despite create_missing_parents=false"
    );
    assert!(!anything_at(&dest));
    // No stray temp file either: copy_atomic must not have been reached at
    // all, so its `.dest.txt.<nonce>.tmp` sibling can never have existed.
    let strays: Vec<_> = fs::read_dir(tmp.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
        .collect();
    assert!(strays.is_empty(), "temp file left behind: {strays:?}");
}

/// The flag must key on the parent being *absent*, not on the destination
/// being absent. A strict run against an existing parent is completely
/// ordinary and must succeed — otherwise `create_missing_parents: false`
/// would be unusable for its actual purpose.
#[test]
fn strict_options_still_succeed_when_the_parent_already_exists() {
    let tmp = tempfile::tempdir().unwrap();
    let source = source_file(tmp.path());
    let parent = tmp.path().join("already_here");
    fs::create_dir(&parent).unwrap();
    let dest = parent.join("dest.txt");

    let entry = Entry {
        source,
        dest: dest.clone(),
        op: LinkOp::FileCopy,
    };
    let applied = materialize(&entry, &ForcedProbe(true), &strict())
        .expect("an existing parent is not a missing one");

    assert_eq!(applied, LinkOp::FileCopy);
    assert_eq!(fs::read(&dest).unwrap(), b"payload");
}

// ---------------------------------------------------------------------------
// Rows 5, 6 and 7: permission denied. Unix only — see the module header.
// ---------------------------------------------------------------------------

/// Restores a path's original mode on drop.
///
/// A directory left at 0o555 cannot have its entries unlinked, so `TempDir`'s
/// own cleanup fails and the failure surfaces as an unrelated, confusing
/// error — or silently leaks the directory. Drop runs on the panic path too,
/// so an assertion failure mid-test still leaves a deletable tempdir.
#[cfg(unix)]
struct RestoreMode {
    path: std::path::PathBuf,
    mode: u32,
}

#[cfg(unix)]
impl Drop for RestoreMode {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&self.path, fs::Permissions::from_mode(self.mode));
    }
}

/// Make `path` read-and-execute but not write, remembering the old mode.
///
/// Returns `None` when the caller can write to the directory anyway, which
/// means the process is effectively root — DAC write checks are bypassed for
/// UID 0, so 0o555 simply does not deny anything. Containerized CI commonly
/// runs as root, and a permission test that cannot fail there is worse than
/// no test: it reports a pass for a guarantee it never exercised. Callers
/// skip loudly instead.
#[cfg(unix)]
fn deny_writes(path: &Path) -> Option<RestoreMode> {
    use std::os::unix::fs::PermissionsExt;

    let mode = fs::metadata(path).unwrap().permissions().mode();
    fs::set_permissions(path, fs::Permissions::from_mode(0o555)).unwrap();
    let guard = RestoreMode {
        path: path.to_path_buf(),
        mode,
    };

    // Probe rather than infer: the question is "can this process write here",
    // and only an attempted write answers it.
    let probe = path.join(".write_probe");
    match fs::write(&probe, b"") {
        Err(_) => Some(guard),
        Ok(()) => {
            let _ = fs::remove_file(&probe);
            eprintln!(
                "\n\
                 ======================================================================\n\
                 SKIPPED: this process can write to a 0o555 directory, so it is\n\
                 running as root. The permission-denied path is NOT exercised by\n\
                 this run. A green result here is NOT evidence that it works.\n\
                 Re-run as an unprivileged user to actually test it.\n\
                 ======================================================================\n"
            );
            None
        }
    }
}

/// Row 5. `PermissionDenied` survives as the kind, and the message names the
/// destination.
///
/// The raw Linux error is the bare string "Permission denied (os error 13)" —
/// it does not say which path. A manifest run touches many destinations, so
/// an unnamed error tells the user nothing actionable. The
/// missing-source error already names its path; this is the same contract on
/// the destination side.
#[cfg(unix)]
#[test]
fn directory_link_refuses_an_unwritable_parent_and_names_the_destination() {
    let tmp = tempfile::tempdir().unwrap();
    let source = source_dir(tmp.path());

    let parent = tmp.path().join("locked");
    fs::create_dir(&parent).unwrap();
    let dest = parent.join("dest_link");

    let Some(_guard) = deny_writes(&parent) else {
        return;
    };

    let entry = Entry {
        source,
        dest: dest.clone(),
        op: LinkOp::DirectoryLink,
    };
    // Default options on purpose: the parent EXISTS, so create_missing_parents
    // is irrelevant here. Permission denied is refused regardless of the flag,
    // and using the permissive setting proves the flag cannot excuse it.
    let err = materialize(&entry, &ForcedProbe(true), &MaterializeOptions::default())
        .expect_err("an unwritable destination parent must be refused");

    assert_eq!(err.kind(), ErrorKind::PermissionDenied);
    assert!(
        err.to_string().contains(&dest.display().to_string()),
        "the error must name the destination path; the raw OS error does not. \
         Got: {err}"
    );
    assert!(
        !anything_at(&dest),
        "nothing may have been created at the destination"
    );
}

/// Row 6. Same contract for the copy path, which reaches the failure through
/// `copy_atomic` rather than through `symlink`/`junction::create` as row 5
/// does. Both must arrive at the same annotated error.
///
/// Exactly which call inside `copy_atomic` reports EACCES is deliberately not
/// asserted: `create_dir_all` on the already-existing locked parent is
/// expected to short-circuit on `is_dir()` and return `Ok`, leaving `fs::copy`
/// into the sibling temp file to be the one that fails — but that is an
/// unverified reading of std's internals, and the contract this test defends
/// (kind plus a message naming the destination) holds either way.
#[cfg(unix)]
#[test]
fn file_copy_refuses_an_unwritable_parent_and_names_the_destination() {
    let tmp = tempfile::tempdir().unwrap();
    let source = source_file(tmp.path());

    let parent = tmp.path().join("locked");
    fs::create_dir(&parent).unwrap();
    let dest = parent.join("dest.txt");

    let Some(_guard) = deny_writes(&parent) else {
        return;
    };

    let entry = Entry {
        source,
        dest: dest.clone(),
        op: LinkOp::FileCopy,
    };
    let err = materialize(&entry, &ForcedProbe(true), &MaterializeOptions::default())
        .expect_err("an unwritable destination parent must be refused");

    assert_eq!(err.kind(), ErrorKind::PermissionDenied);
    assert!(
        err.to_string().contains(&dest.display().to_string()),
        "the error must name the destination path. Got: {err}"
    );
    assert!(!anything_at(&dest));
}

/// The annotated message must keep the original OS error rather than replace
/// it. Without this, a rewrite that names the path but drops the errno looks
/// identical to rows 5 and 6 — and the errno is what distinguishes EACCES
/// from EPERM and EROFS when someone is actually debugging a failure.
#[cfg(unix)]
#[test]
fn the_permission_error_preserves_the_underlying_os_error() {
    let tmp = tempfile::tempdir().unwrap();
    let source = source_file(tmp.path());

    let parent = tmp.path().join("locked");
    fs::create_dir(&parent).unwrap();
    let dest = parent.join("dest.txt");

    let Some(_guard) = deny_writes(&parent) else {
        return;
    };

    let entry = Entry {
        source,
        dest,
        op: LinkOp::FileCopy,
    };
    let err = materialize(&entry, &ForcedProbe(true), &MaterializeOptions::default()).unwrap_err();

    let raw = std::io::Error::from_raw_os_error(13).to_string();
    assert!(
        err.to_string().contains(&raw),
        "the original OS error {raw:?} must survive into the new message so \
         the errno is still available for debugging. Got: {err}"
    );
}

/// Row 7. The destination-untouched guarantee, mirroring the missing-source
/// tests and `materialize.rs`'s
/// `directory_link_failure_propagates_as_an_error_not_a_silent_downgrade`.
///
/// Real, irreplaceable content sits at the destination; the parent is then
/// locked. The copy must fail *and* leave the existing bytes alone — not
/// truncate them, not replace them with a partial write, not delete them.
/// This is the case a non-atomic implementation would corrupt: it opens the
/// destination for writing first and discovers the failure second.
#[cfg(unix)]
#[test]
fn a_failed_materialize_leaves_existing_destination_data_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let source = source_file(tmp.path());

    let parent = tmp.path().join("locked");
    fs::create_dir(&parent).unwrap();
    let dest = parent.join("dest.txt");
    fs::write(&dest, b"irreplaceable").unwrap();

    let Some(_guard) = deny_writes(&parent) else {
        return;
    };

    let entry = Entry {
        source,
        dest: dest.clone(),
        op: LinkOp::FileCopy,
    };
    let err = materialize(&entry, &ForcedProbe(true), &MaterializeOptions::default())
        .expect_err("an unwritable destination parent must be refused");
    assert_eq!(err.kind(), ErrorKind::PermissionDenied);

    assert_eq!(
        fs::read(&dest).unwrap(),
        b"irreplaceable",
        "SAFETY VIOLATION: real data at the destination was modified by a \
         materialize() call that failed"
    );
    // And no temp-file debris in the locked directory.
    let strays: Vec<_> = fs::read_dir(&parent)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
        .collect();
    assert!(strays.is_empty(), "temp file left behind: {strays:?}");
}

/// Row 7, strict-flag flavor, and the one case that needs no special
/// permissions — so unlike the rest of row 7 it runs on Windows too.
///
/// `create_missing_parents: false` must leave an existing destination alone
/// when it refuses. The refusal happens before the `match` on the op, so
/// nothing should reach the destination at all; this pins that down rather
/// than trusting the ordering.
#[test]
fn a_refused_missing_parent_leaves_a_sibling_destination_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let source = source_file(tmp.path());

    // Real data at a path that shares a prefix with the refused destination.
    let existing = tmp.path().join("existing.txt");
    fs::write(&existing, b"irreplaceable").unwrap();

    let entry = Entry {
        source,
        dest: tmp.path().join("a/b/c/dest.txt"),
        op: LinkOp::FileCopy,
    };
    let err = materialize(&entry, &ForcedProbe(true), &strict()).expect_err("must refuse");
    assert_eq!(err.kind(), ErrorKind::NotFound);

    assert_eq!(
        fs::read(&existing).unwrap(),
        b"irreplaceable",
        "a refused materialize must not modify anything on disk"
    );
    assert!(
        !anything_at(&tmp.path().join("a")),
        "a refused materialize must not create anything on disk"
    );
}
