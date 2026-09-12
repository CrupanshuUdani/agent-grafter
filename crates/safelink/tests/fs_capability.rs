//! Task 13: symlink support is a property of the **filesystem at the
//! destination**, not of the process, the platform, or `/tmp`.
//!
//! The bug this file exists to prevent: `RealProbe` used to probe
//! `tempfile::tempdir()` -- tmpfs, which always supports symlinks -- and the
//! trait took no path at all, so it was structurally incapable of asking about
//! the destination. On exFAT the probe therefore answered `true`, `materialize`
//! took the `FileSymlink` arm, and `symlink_file` failed at the destination
//! with `EPERM`. The `FileSymlink -> FileCopy` downgrade that exists for
//! exactly this case never fired.
//!
//! EXFAT COVERAGE IS OPT-IN. The rows that need a filesystem without symlink
//! support only run when `SAFELINK_EXFAT_TEST_DIR` points at a writable mount
//! on such a filesystem. They **skip loudly** otherwise, because a green run on
//! ext4/btrfs/tmpfs is not evidence about exFAT -- every one of those supports
//! symlinks, so they cannot exercise the path at all. To set one up:
//!
//!   truncate -s 64M /tmp/exfat.img
//!   sudo mkfs.exfat -n SAFELINK /tmp/exfat.img
//!   sudo mkdir -p /mnt/exfat && sudo mount -o loop,uid=$(id -u),gid=$(id -g) \
//!        /tmp/exfat.img /mnt/exfat
//!   export SAFELINK_EXFAT_TEST_DIR=/mnt/exfat
//!
//! The probe-level tests below need no special filesystem and always run.

use safelink::capability::{ForcedProbe, RealProbe, SymlinkProbe};
use safelink::materialize::{MaterializeOptions, materialize};
use safelink::op::{Entry, LinkOp};
use std::fs;
use std::path::{Path, PathBuf};

/// A directory on a filesystem that genuinely cannot hold symlinks, or `None`.
///
/// Verifies the premise rather than trusting the env var: if the configured
/// path *can* hold a symlink it is not a link-less filesystem, and using it
/// would silently turn these tests into no-ops that still report success.
fn linkless_fs_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var("SAFELINK_EXFAT_TEST_DIR").ok()?);
    if !dir.is_dir() {
        eprintln!(
            "\nSKIPPED: SAFELINK_EXFAT_TEST_DIR={} is not a directory.\n",
            dir.display()
        );
        return None;
    }

    // Probe directly, not via the crate -- the crate is what is under test.
    let canary = dir.join(".linkless_canary");
    let _ = fs::remove_file(&canary);
    #[cfg(unix)]
    let can_link = std::os::unix::fs::symlink("target", &canary).is_ok();
    #[cfg(windows)]
    let can_link = std::os::windows::fs::symlink_file("target", &canary).is_ok();
    if can_link {
        let _ = fs::remove_file(&canary);
        eprintln!(
            "\n\
             ======================================================================\n\
             SKIPPED: {} CAN hold symlinks, so it is not a link-less\n\
             filesystem and cannot exercise the exFAT path. A pass here would\n\
             be meaningless. Point SAFELINK_EXFAT_TEST_DIR at real exFAT.\n\
             ======================================================================\n",
            dir.display()
        );
        return None;
    }
    Some(dir)
}

/// Loud skip when exFAT coverage is simply not configured.
fn skip_no_exfat() {
    eprintln!(
        "\n\
         ======================================================================\n\
         SKIPPED: SAFELINK_EXFAT_TEST_DIR is not set, so the link-less\n\
         filesystem path is NOT exercised by this run. A green result here is\n\
         NOT evidence that exFAT/FAT32 behaviour works. See this file's header.\n\
         ======================================================================\n"
    );
}

/// Unique scratch dir inside the link-less volume, so concurrent tests cannot
/// collide on exFAT (which has no per-test tempdir crate support we rely on).
fn scratch(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(format!("t13_{name}"));
    let _ = fs::remove_dir_all(&p);
    let _ = fs::remove_file(&p);
    fs::create_dir_all(&p).expect("create scratch dir on the test volume");
    p
}

// ---------------------------------------------------------------------------
// Probe-level: no special filesystem needed.
// ---------------------------------------------------------------------------

/// The trait must accept a path. If this stops compiling, the regression is
/// back: a path-less probe cannot ask about the destination.
#[test]
fn the_probe_api_takes_a_destination_path() {
    let tmp = tempfile::tempdir().unwrap();
    // Both implementations accept a path.
    let _ = RealProbe.can_create_file_symlink(&tmp.path().join("dest"));
    let _ = ForcedProbe(true).can_create_file_symlink(&tmp.path().join("dest"));
}

/// A not-yet-created parent is a missing directory, not a filesystem
/// limitation -- Task 10b may create it moments later, so answering `false`
/// would wrongly downgrade every `FileSymlink` whose parent does not exist yet.
#[cfg(unix)]
#[test]
fn a_missing_parent_does_not_read_as_a_symlink_incapable_filesystem() {
    let tmp = tempfile::tempdir().unwrap();
    let canary = tmp.path().join("canary");
    if std::os::unix::fs::symlink("t", &canary).is_err() {
        eprintln!("\nSKIPPED: tempdir cannot hold symlinks.\n");
        return;
    }
    fs::remove_file(&canary).unwrap();

    assert!(
        RealProbe.can_create_file_symlink(&tmp.path().join("a/b/c/dest.txt")),
        "walking up to the nearest existing directory is required; a missing \
         parent is not evidence about the filesystem"
    );
}

// ---------------------------------------------------------------------------
// The rows that need a link-less filesystem.
// ---------------------------------------------------------------------------

/// The core of Task 13. The probe must answer about the destination, and the
/// old `/tmp` behaviour must not creep back.
///
/// Sabotage target: make `RealProbe` ignore its `near` argument and probe
/// `tempfile::tempdir()` again. On any normal dev box `/tmp` is tmpfs, so the
/// probe flips to `true` and this test fails.
#[test]
fn the_probe_reports_false_on_a_filesystem_without_symlinks() {
    let Some(dir) = linkless_fs_dir() else {
        skip_no_exfat();
        return;
    };
    let dest = scratch(&dir, "probe").join("dest.txt");

    assert!(
        !RealProbe.can_create_file_symlink(&dest),
        "the destination's filesystem cannot hold a symlink, so the probe must \
         say so. Answering `true` here is what made materialize() fail instead \
         of downgrading."
    );
}

/// The consequence the bug actually caused: `materialize` must downgrade to a
/// copy, not fail, when the destination cannot hold a symlink.
#[test]
fn a_file_symlink_downgrades_to_a_copy_on_a_filesystem_without_symlinks() {
    let Some(dir) = linkless_fs_dir() else {
        skip_no_exfat();
        return;
    };
    let work = scratch(&dir, "downgrade");

    // Source lives on the link-less volume too -- the realistic case is a
    // removable disk holding the canonical copy.
    let source = work.join("src.txt");
    fs::write(&source, b"payload").unwrap();
    let dest = work.join("dest.txt");

    let entry = Entry {
        source,
        dest: dest.clone(),
        op: LinkOp::FileSymlink,
    };
    let applied = materialize(&entry, &RealProbe, &MaterializeOptions::default())
        .expect("must downgrade to a copy, not fail");

    assert_eq!(
        applied,
        LinkOp::FileCopy,
        "the only permitted divergence is FileSymlink -> FileCopy, and this is \
         exactly when it must happen"
    );
    assert_eq!(fs::read(&dest).unwrap(), b"payload");
    // A real copy, not a link: nothing on this filesystem can be a link.
    assert!(
        !dest.symlink_metadata().unwrap().file_type().is_symlink(),
        "the destination must be a real file"
    );
}

/// `DirectoryLink` has no downgrade by design, so it must refuse -- but with an
/// error that names the filesystem limitation instead of blaming permissions.
///
/// The raw OS error is `EPERM`, indistinguishable from a genuine ACL failure,
/// so the pre-Task-13 message told the user to "fix the permissions" on a
/// destination whose permissions were already correct. That is the
/// admin-rights red herring this project exists to eliminate.
#[test]
fn a_directory_link_refuses_and_blames_the_filesystem_not_permissions() {
    let Some(dir) = linkless_fs_dir() else {
        skip_no_exfat();
        return;
    };
    let work = scratch(&dir, "dirlink");

    // The source must be a real directory; put it somewhere that can hold one.
    let src_host = tempfile::tempdir().unwrap();
    let source = src_host.path().join("source_dir");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("inside.txt"), b"payload").unwrap();

    let dest = work.join("dir_link");
    let entry = Entry {
        source,
        dest: dest.clone(),
        op: LinkOp::DirectoryLink,
    };
    let err = materialize(&entry, &RealProbe, &MaterializeOptions::default())
        .expect_err("a directory link is impossible on this filesystem");

    assert_eq!(
        err.kind(),
        std::io::ErrorKind::Unsupported,
        "an unsupported operation must not be reported as PermissionDenied. \
         Got: {err}"
    );
    let msg = err.to_string();
    assert!(
        msg.contains("filesystem limitation"),
        "the error must say the filesystem is the cause. Got: {msg}"
    );
    assert!(
        !msg.contains("Fix the permissions"),
        "must NOT advise fixing permissions -- they are not the problem, and \
         that advice is the exact red herring this project removes. Got: {msg}"
    );
    assert!(
        msg.contains(&dest.display().to_string()),
        "the error must name the destination. Got: {msg}"
    );

    // And nothing may be left behind.
    assert!(
        dest.symlink_metadata().is_err(),
        "a refused DirectoryLink must create nothing"
    );
    // Emphatically not a recursive copy of the source.
    assert!(
        !dest.join("inside.txt").exists(),
        "SAFETY VIOLATION: DirectoryLink must never fall back to copying the \
         tree -- that turns one link into unbounded duplication"
    );
}

/// A genuine permission failure on a link-capable filesystem must still report
/// `PermissionDenied`, not get swallowed by the new Unsupported branch.
#[cfg(unix)]
#[test]
fn a_real_permission_failure_is_still_reported_as_permission_denied() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let canary = tmp.path().join("canary");
    if std::os::unix::fs::symlink("t", &canary).is_err() {
        eprintln!("\nSKIPPED: tempdir cannot hold symlinks.\n");
        return;
    }
    fs::remove_file(&canary).unwrap();

    let source = tmp.path().join("source_dir");
    fs::create_dir(&source).unwrap();

    let locked = tmp.path().join("locked");
    fs::create_dir(&locked).unwrap();
    let mode = fs::metadata(&locked).unwrap().permissions().mode();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();

    // Root bypasses DAC write checks, so this cannot be tested as root.
    let probe_write = locked.join(".w");
    if fs::write(&probe_write, b"").is_ok() {
        let _ = fs::remove_file(&probe_write);
        fs::set_permissions(&locked, fs::Permissions::from_mode(mode)).unwrap();
        eprintln!("\nSKIPPED: running as root, 0o555 does not deny writes.\n");
        return;
    }

    let entry = Entry {
        source,
        dest: locked.join("dir_link"),
        op: LinkOp::DirectoryLink,
    };
    let err = materialize(&entry, &RealProbe, &MaterializeOptions::default())
        .expect_err("an unwritable parent must be refused");

    // Restore before asserting so a failure cannot leak an undeletable dir.
    fs::set_permissions(&locked, fs::Permissions::from_mode(mode)).unwrap();

    assert_eq!(
        err.kind(),
        std::io::ErrorKind::PermissionDenied,
        "this filesystem CAN hold links, so the failure really is permissions \
         and must not be reclassified as Unsupported. Got: {err}"
    );
}
