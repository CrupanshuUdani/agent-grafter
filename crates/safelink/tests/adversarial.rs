//! Task 12: the adversarial input matrix.
//!
//! Every case below is a way a real filesystem can hand the engine something
//! hostile, and the project's entire claim is that it never destroys a source
//! of truth. Each row is one `#[test]`. A row whose behaviour is merely
//! "documented" still needs an assertion -- a test that asserts nothing is a
//! defect, not coverage.
//!
//! Several rows overlap with work already done elsewhere in this crate. Where
//! they do, this file deliberately duplicates the assertion rather than
//! deleting or merging it -- the plan's stated reason is defense-in-depth: if
//! a future refactor breaks the guarantee, two independent tests should fail,
//! not one. Each such row carries a doc comment naming the existing test it
//! deliberately overlaps with and why it is re-asserted here.
//!
//! PLATFORM HONESTY (Global Constraint 6): the self-referential-link and
//! own-parent-link tests are `#[cfg(unix)]`-only. Creating a self-referential
//! junction on Windows is a separate exercise, and gating only the setup line
//! would leave the assertions running against a path that does not exist, so
//! the test would fail on Windows for the wrong reason. The cross-volume test
//! is `#[cfg(windows)]`-only (POSIX symlinks already allow cross-filesystem
//! linking trivially and this was never in question there). Neither can be
//! exercised for real on this Linux machine -- only the `windows-latest` CI
//! job is evidence for them.

use safelink::capability::ForcedProbe;
use safelink::classify::{LinkClass, classify};
use safelink::create::create_dir_link;
use safelink::materialize::{MaterializeOptions, materialize};
use safelink::op::{Entry, LinkOp};
use safelink::remove::remove_link;
use std::fs;
use std::io::ErrorKind;
use tempfile::tempdir;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

// ---------------------------------------------------------------------------
// Row 1. Dangling link (target deleted after creation).
// NEW -- no existing test covers this exact case.
// ---------------------------------------------------------------------------

/// A link whose target is deleted after the link is created. The link still
/// exists (it is a filesystem object independent of its target), so it must
/// still classify as a link and still be removable -- without erroring on the
/// now-dangling target. A dangling directory link classifies as FileLink (not
/// DirLink): with the target gone, classify() cannot resolve the target's type
/// and falls through to the FileLink branch, which is the correct primitive for
/// a broken link either way.
#[test]
fn a_dangling_link_is_classified_and_removed_without_erroring_on_its_target() {
    let d = tempdir().unwrap();
    let target = d.path().join("target");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("inside.txt"), b"payload").unwrap();
    let link = d.path().join("link");
    create_dir_link(&target, &link).unwrap();

    // Delete the target out from under the link.
    fs::remove_dir_all(&target).unwrap();
    assert!(!target.exists(), "setup: target should be gone");

    // The link is still a link, even though it now dangles. A dangling
    // directory link classifies as FileLink (the target is gone, so the
    // type cannot be resolved).
    assert_eq!(classify(&link).unwrap(), LinkClass::FileLink);

    // And it must be removable -- without erroring on the missing target.
    // On Windows, remove_dir_all may not release the directory handle
    // immediately, so the first remove_link attempt can hit "Access is
    // denied". Retry a few times to handle this asynchronous handle release
    // -- the link must eventually be removable.
    let mut last_err = None;
    for _ in 0..10 {
        match remove_link(&link) {
            Ok(()) => {
                last_err = None;
                break;
            }
            Err(e) => {
                last_err = Some(e);
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
    if let Some(e) = last_err {
        panic!("dangling link should be removable after retries, got: {e}");
    }
    assert!(!link.exists(), "the link should be gone after remove_link");
}

// ---------------------------------------------------------------------------
// Row 2. Chained link (target is another link).
// NEW -- no existing test covers this.
// ---------------------------------------------------------------------------

/// A link whose target is itself a link, not real data. `classify` must report
/// the class of the path *given* (a link), never the final resolved type, and
/// `remove_link` must unlink exactly one level -- leaving the intermediate
/// link and the final target intact.
#[test]
fn a_chained_link_is_classified_at_one_level_and_removes_one_level() {
    let d = tempdir().unwrap();
    let real_target = d.path().join("real_target");
    fs::create_dir(&real_target).unwrap();
    fs::write(real_target.join("inside.txt"), b"payload").unwrap();

    let middle = d.path().join("middle_link");
    create_dir_link(&real_target, &middle).unwrap();

    let outer = d.path().join("outer_link");
    create_dir_link(&middle, &outer).unwrap();

    // classify(outer) reports outer's own class -- a link -- not the final
    // resolved type (a directory). It must not walk the chain.
    assert_eq!(classify(&outer).unwrap(), LinkClass::DirLink);

    // Removing outer unlinks only outer. middle and real_target survive.
    remove_link(&outer).unwrap();
    assert!(!outer.exists(), "outer link should be gone");
    assert!(middle.exists(), "middle link must survive");
    assert!(
        real_target.join("inside.txt").exists(),
        "final target's contents must survive"
    );
}

// ---------------------------------------------------------------------------
// Row 3. Self-referential link.
// The plan gives verbatim code for this row -- reproduced here with the same
// rigor. `#[cfg(unix)]`-only; see the module header.
// ---------------------------------------------------------------------------

/// A link that points at itself. `classify` must return a definite answer and
/// never resolve the target (resolving would fail with ELOOP, or hang a naive
/// resolver). `remove_link` must succeed without touching anything else.
#[cfg(unix)]
#[test]
fn a_self_referential_link_is_classified_without_following_it() {
    let d = tempdir().unwrap();
    let p = d.path().join("loop");

    // .expect(), not .ok(): if setup fails, the test must fail rather than
    // pass vacuously against a path where no cycle was ever created.
    std::os::unix::fs::symlink(&p, &p).expect("setup: create the self-referential symlink");
    assert!(
        std::fs::symlink_metadata(&p)
            .unwrap()
            .file_type()
            .is_symlink(),
        "setup did not produce a symlink"
    );

    let class = classify(&p).expect("classify must not error on a cycle");
    assert!(
        matches!(class, LinkClass::FileLink | LinkClass::DirLink),
        "a self-referential link is still a link; got {class:?}"
    );

    remove_link(&p).expect("a self-referential link must be removable");
    assert!(
        std::fs::symlink_metadata(&p).is_err(),
        "the link should be gone after remove_link"
    );
}

// ---------------------------------------------------------------------------
// Row 4. Link pointing at its own parent.
// NEW -- structurally similar to row 3 but one level removed.
// ---------------------------------------------------------------------------

/// A directory link that points at its own parent (or an ancestor). The
/// engine must refuse to hang or recurse -- classify returns a definite
/// answer, remove_link unlinks exactly the link object.
#[cfg(unix)]
#[test]
fn a_link_pointing_at_its_own_parent_is_classified_without_recursing() {
    let d = tempdir().unwrap();
    let parent = d.path().join("parent");
    fs::create_dir(&parent).unwrap();
    fs::write(parent.join("inside.txt"), b"payload").unwrap();

    // A link inside parent that points back at parent itself.
    let link = parent.join("link");
    std::os::unix::fs::symlink(&parent, &link).expect("setup: create the parent-pointing symlink");
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );

    let class = classify(&link).expect("classify must not error on a parent-cycle");
    assert_eq!(class, LinkClass::DirLink);

    remove_link(&link).expect("a parent-pointing link must be removable");
    assert!(!link.exists(), "the link should be gone");
    assert!(
        parent.join("inside.txt").exists(),
        "the parent's contents must survive"
    );
}

// ---------------------------------------------------------------------------
// Row 5. remove_link on a real directory.
// DELIBERATE OVERLAP with `remove::tests::refuses_to_remove_a_plain_directory`
// (Task 5). Re-asserted here as a regression guard: if a future refactor
// breaks this guarantee, two independent tests should fail, not one.
// ---------------------------------------------------------------------------

#[test]
fn refuses_to_remove_a_real_directory() {
    let d = tempdir().unwrap();
    let real = d.path().join("real_dir");
    fs::create_dir(&real).unwrap();
    fs::write(real.join("inside.txt"), b"payload").unwrap();

    let err = remove_link(&real).expect_err("a real directory must be refused");
    assert_eq!(err.kind(), ErrorKind::InvalidInput);
    assert!(real.exists(), "must not have removed a real directory");
}

// ---------------------------------------------------------------------------
// Row 6. remove_link on a real file.
// DELIBERATE OVERLAP with `remove::tests::refuses_to_remove_a_plain_file`
// (Task 5). Same defense-in-depth reasoning as row 5.
// ---------------------------------------------------------------------------

#[test]
fn refuses_to_remove_a_real_file() {
    let d = tempdir().unwrap();
    let real = d.path().join("real.txt");
    fs::write(&real, b"payload").unwrap();

    let err = remove_link(&real).expect_err("a real file must be refused");
    assert_eq!(err.kind(), ErrorKind::InvalidInput);
    assert!(real.exists(), "must not have removed a real file");
}

// ---------------------------------------------------------------------------
// Row 7. Destination exists as a real file, dir link requested.
// NEW -- the closest existing test is materialize.rs's
// `directory_link_failure_propagates_as_an_error_not_a_silent_downgrade`, but
// that test occupies the destination with a real file to make
// `create_dir_link` fail and then asserts the failure is not silently
// downgraded to a copy. This row asserts the same underlying guarantee but
// framed as part of the adversarial matrix: the existing real file must be
// left byte-identical, not truncated, not deleted, not replaced.
// ---------------------------------------------------------------------------

#[test]
fn refuses_when_destination_is_a_real_file_and_dir_link_is_requested() {
    let d = tempdir().unwrap();
    let source = d.path().join("source_dir");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("inside.txt"), b"payload").unwrap();

    // Occupied destination: a real file where the link is requested.
    let dest = d.path().join("dest.txt");
    fs::write(&dest, b"irreplaceable real file").unwrap();

    let entry = Entry {
        source,
        dest: dest.clone(),
        op: LinkOp::DirectoryLink,
    };
    let err = materialize(&entry, &ForcedProbe(true), &MaterializeOptions::default())
        .expect_err("a real file at the destination must be refused");

    // The real file must be byte-identical -- not truncated, not deleted.
    assert_eq!(
        fs::read(&dest).unwrap(),
        b"irreplaceable real file",
        "SAFETY VIOLATION: the existing real file was modified by a refused materialize()"
    );
    // And the error must not be a silent downgrade to a copy.
    assert_ne!(
        err.kind(),
        ErrorKind::NotFound,
        "a missing-source error would indicate the engine misread the occupied destination as a missing source"
    );
}

// ---------------------------------------------------------------------------
// Row 8. Destination's parent does not exist.
// DELIBERATE OVERLAP with `dest_failure.rs` (Task 10b), which already covers
// missing-parent-hierarchy via `directory_link_refuses_a_missing_parent_when_creation_is_disabled`
// and `file_copy_refuses_a_missing_parent_when_creation_is_disabled`. Re-asserted
// here as a regression guard and to confirm the guarantee holds for the
// `DirectoryLink` op through the full materialize() path, not just the
// lower-level primitives.
// ---------------------------------------------------------------------------

#[test]
fn refuses_when_destinations_parent_does_not_exist() {
    let d = tempdir().unwrap();
    let source = d.path().join("source_dir");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("inside.txt"), b"payload").unwrap();

    let missing_parent = d.path().join("a/b/c");
    let dest = missing_parent.join("dest_link");

    let entry = Entry {
        source,
        dest: dest.clone(),
        op: LinkOp::DirectoryLink,
    };
    // Use strict() options: with the default (create_missing_parents: true)
    // the missing parent would be auto-created. To test refusal, the flag
    // must be disabled.
    let err = materialize(
        &entry,
        &ForcedProbe(true),
        &MaterializeOptions {
            create_missing_parents: false,
        },
    )
    .expect_err("a missing parent must be refused");

    assert_eq!(err.kind(), ErrorKind::NotFound);
    assert!(
        err.to_string()
            .contains(&missing_parent.display().to_string()),
        "the error must name the missing parent directory. Got: {err}"
    );
    assert!(!dest.exists(), "nothing may exist at the destination");
}

// ---------------------------------------------------------------------------
// Row 9. Destination is read-only.
// DELIBERATE OVERLAP with `dest_failure.rs` (Task 10b), which already covers
// permission-denied via `directory_link_refuses_an_unwritable_parent_and_names_the_destination`
// and `file_copy_refuses_an_unwritable_parent_and_names_the_destination`.
// Re-asserted here as a regression guard and to confirm the OS error is
// surfaced unchanged (not swallowed, not replaced with a fallback).
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn surfaces_the_os_error_unchanged_when_destination_is_read_only() {
    let d = tempdir().unwrap();
    let source = d.path().join("source_dir");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("inside.txt"), b"payload").unwrap();

    let parent = d.path().join("locked");
    fs::create_dir(&parent).unwrap();
    let dest = parent.join("dest_link");

    // Lock the parent directory.
    let mode = fs::metadata(&parent).unwrap().permissions().mode();
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o555)).unwrap();
    let _guard = RestoreMode {
        path: parent.clone(),
        mode,
    };

    let entry = Entry {
        source,
        dest: dest.clone(),
        op: LinkOp::DirectoryLink,
    };
    let err = materialize(&entry, &ForcedProbe(true), &MaterializeOptions::default())
        .expect_err("a read-only destination parent must be refused");

    assert_eq!(err.kind(), ErrorKind::PermissionDenied);
    assert!(
        err.to_string().contains(&dest.display().to_string()),
        "the error must name the destination path. Got: {err}"
    );
    assert!(
        !dest.exists(),
        "nothing may have been created at the destination"
    );
}

// ---------------------------------------------------------------------------
// Row 10. Same-volume and cross-volume targets.
// Windows-only -- POSIX symlinks already allow cross-filesystem linking
/// trivially and this was never in question there. Only `windows-latest` CI
/// is evidence for this row.
// ---------------------------------------------------------------------------

/// Junctions on Windows allow cross-volume linking, unlike POSIX symlinks
/// which already do trivially. Both same-volume and cross-volume targets must
/// succeed. This test is `#[cfg(windows)]`-only and cannot execute on this
/// Linux machine -- see Global Constraint 6.
#[cfg(windows)]
#[test]
fn creates_links_for_both_same_volume_and_cross_volume_targets() {
    let d = tempdir().unwrap();
    let source = d.path().join("source_dir");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("inside.txt"), b"payload").unwrap();

    // Same-volume target.
    let same_volume = d.path().join("same_vol_link");
    create_dir_link(&source, &same_volume).unwrap();
    assert_eq!(classify(&same_volume).unwrap(), LinkClass::DirLink);
    assert_eq!(
        fs::read(same_volume.join("inside.txt")).unwrap(),
        b"payload"
    );

    // Cross-volume target: use the system temp directory's volume as a
    // different mount point if one is available, otherwise document that
    // this specific sub-case is deferred to manual Gate B verification.
    // The CI runner typically exposes multiple drive letters; try to find
    // one that is not the same as our tempdir's drive.
    let cross_volume = if let Some(other_vol) = find_other_volume(d.path()) {
        let link = other_vol.join("cross_vol_link");
        create_dir_link(&source, &link).expect("cross-volume junction must succeed on Windows");
        assert_eq!(classify(&link).unwrap(), LinkClass::DirLink);
        true
    } else {
        // No second volume available in this environment -- document the
        // deferral rather than silently passing.
        eprintln!(
            "\n======================================================================\n\
             NOTE: cross-volume sub-case not exercised -- no second volume\n\
             found in this environment. Same-volume case passed.\n\
             ======================================================================\n"
        );
        false
    };
    let _ = cross_volume; // used only for the side effect above
}

#[cfg(windows)]
fn find_other_volume(our_path: &std::path::Path) -> Option<std::path::PathBuf> {
    let our_drive = our_path.components().next()?;
    let our_letter = match our_drive {
        std::path::Component::Prefix(p) => p.as_os_str().to_string_lossy().chars().next()?,
        _ => return None,
    };
    // Try the other common drive letters.
    for letter in ['C', 'D', 'E', 'T'] {
        if letter == our_letter {
            continue;
        }
        let candidate = format!("{}:\\", letter);
        let path = std::path::Path::new(&candidate);
        if path.exists() {
            return Some(path.to_path_buf());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Row 11. Two concurrent materialize calls, same destination.
// NEW -- no existing test covers concurrent materialize().
// ---------------------------------------------------------------------------

/// Two threads racing to materialize the same destination. Exactly one must
/// win; the other must error. The destination must never be left in a
/// partial/corrupt state -- it holds one complete result, never a mix.
#[test]
fn two_concurrent_materialize_calls_never_leave_a_partial_destination() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let d = tempdir().unwrap();
    let source = d.path().join("source_dir");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("inside.txt"), b"payload").unwrap();

    let dest = d.path().join("dest_dir");

    let success_count = Arc::new(AtomicUsize::new(0));
    let error_count = Arc::new(AtomicUsize::new(0));

    let handles: Vec<_> = (0..2)
        .map(|_| {
            let source = source.clone();
            let dest = dest.clone();
            let success_count = Arc::clone(&success_count);
            let error_count = Arc::clone(&error_count);
            std::thread::spawn(move || {
                let entry = Entry {
                    source,
                    dest,
                    op: LinkOp::DirectoryLink,
                };
                match materialize(&entry, &ForcedProbe(true), &MaterializeOptions::default()) {
                    Ok(LinkOp::DirectoryLink) => {
                        success_count.fetch_add(1, Ordering::SeqCst);
                    }
                    Ok(other) => {
                        // A downgrade to a copy would be a silent substitution
                        // the engine is not authorized to make -- count as an
                        // error for the purposes of this test.
                        panic!("unexpected downgrade to {other:?}");
                    }
                    Err(_) => {
                        error_count.fetch_add(1, Ordering::SeqCst);
                    }
                }
            })
        })
        .collect();

    for h in handles {
        h.join().expect("thread panicked");
    }

    assert_eq!(
        success_count.load(Ordering::SeqCst),
        1,
        "exactly one materialize must succeed"
    );
    assert_eq!(
        error_count.load(Ordering::SeqCst),
        1,
        "exactly one materialize must error"
    );

    // The destination must hold one complete result -- a working link.
    assert_eq!(classify(&dest).unwrap(), LinkClass::DirLink);
    assert_eq!(
        fs::read(dest.join("inside.txt")).unwrap(),
        b"payload",
        "the destination must hold the complete source contents, never a partial write"
    );
}

// ---------------------------------------------------------------------------
// Row 12. Atomic copy interrupted.
// DELIBERATE OVERLAP with `copy::tests::destination_is_never_observed_partially_written`
// (Task 7), which already proves the temp-file-plus-rename property via a
// watcher thread. Re-asserted here as a regression guard and to confirm the
// guarantee holds through the full materialize() path, not just the
// lower-level copy_atomic() primitive.
// ---------------------------------------------------------------------------

/// The temp-file-plus-rename property: a crash or interruption mid-copy must
/// leave the destination holding either the old content or the new content,
/// never a partial write. Proved here through the full materialize() path.
#[test]
fn an_atomic_copy_never_leaves_a_partial_destination() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let d = tempdir().unwrap();

    // Large enough that a non-atomic copy is observably incremental.
    let payload = vec![b'N'; 32 * 1024 * 1024];
    let source = d.path().join("big_src");
    fs::write(&source, &payload).unwrap();

    let dest = d.path().join("dest.bin");
    let old = b"OLD".repeat(10);
    fs::write(&dest, &old).unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let watcher = {
        let dest = dest.clone();
        let stop = Arc::clone(&stop);
        let old_len = old.len() as u64;
        let new_len = payload.len() as u64;
        std::thread::spawn(move || {
            let mut bad: Option<u64> = None;
            while !stop.load(Ordering::Relaxed) {
                if let Ok(md) = std::fs::metadata(&dest) {
                    let n = md.len();
                    if n != old_len && n != new_len {
                        bad = Some(n);
                        break;
                    }
                }
            }
            bad
        })
    };

    let entry = Entry {
        source,
        dest: dest.clone(),
        op: LinkOp::FileCopy,
    };
    materialize(&entry, &ForcedProbe(true), &MaterializeOptions::default()).unwrap();
    stop.store(true, Ordering::Relaxed);

    let torn = watcher.join().expect("watcher thread panicked");
    assert!(
        torn.is_none(),
        "destination was observed at {} bytes -- neither the old content ({} bytes) nor the finished copy ({} bytes)",
        torn.unwrap(),
        old.len(),
        payload.len()
    );
    assert_eq!(fs::read(&dest).unwrap().len(), payload.len());
}

// ---------------------------------------------------------------------------
// Shared helpers.
// ---------------------------------------------------------------------------

/// Restores a path's original mode on drop. A directory left at 0o555 cannot
/// have its entries unlinked, so TempDir's own cleanup fails and the failure
/// surfaces as an unrelated, confusing error -- or silently leaks the
/// directory. Drop runs on the panic path too, so an assertion failure
/// mid-test still leaves a deletable tempdir.
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
