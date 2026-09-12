use std::path::Path;

/// Whether a file symlink can be created **at a given location**.
///
/// The path argument is load-bearing. Symlink support is a property of the
/// *filesystem*, not of the process or the platform: exFAT, FAT32 and UDF have
/// no symlink mechanism at all, and SMB/CIFS support depends on the mount's
/// `mfsymlinks` option and the server's Unix Extensions, so two mounts of the
/// same server can disagree. Probing somewhere other than the destination
/// answers a question nobody asked.
///
/// Injectable on purpose. GitHub-hosted Windows runners are documented to run
/// "as administrators with User Account Control (UAC) disabled", so the real
/// probe answers `true` in CI while a standard user with Developer Mode off
/// gets `false`. Tests must be able to force the answer, or the copy fallback
/// is never covered where it matters.
pub trait SymlinkProbe {
    /// `near` is the intended destination. Implementations must test the
    /// filesystem that would actually hold the link, not a temp directory.
    fn can_create_file_symlink(&self, near: &Path) -> bool;
}

/// Probes by actually attempting a symlink next to `near` and cleaning up.
///
/// Feature detection, not privilege inspection: the question is "does it work
/// *here*", not "which privileges are held" and not "what is this filesystem
/// called". Never branch on a filesystem name — `statfs` magic numbers are not
/// a capability contract, and CIFS proves identical types can differ.
pub struct RealProbe;

impl SymlinkProbe for RealProbe {
    fn can_create_file_symlink(&self, near: &Path) -> bool {
        // Probe in the directory that would hold the link. A sibling of the
        // destination is on the destination's filesystem by construction --
        // the same reasoning `copy_atomic` applies to its temp file.
        let Some(dir) = nearest_existing_dir(near) else {
            // Nothing on this path exists yet and we ran out of ancestors.
            // Refusing is the safe answer: claiming `true` is what caused the
            // exFAT failure this probe exists to prevent.
            return false;
        };

        // A unique name so concurrent probes cannot collide. `.tmp`-suffixed
        // and dot-prefixed to match the crate's existing scratch convention.
        let Ok(tmp_file) = tempfile::Builder::new()
            .prefix(".safelink_probe.")
            .suffix(".tmp")
            .tempfile_in(&dir)
        else {
            // Cannot even write here. Not a symlink verdict, but the caller
            // cannot use a symlink either way, and the write failure will
            // surface with a real error when the op runs.
            return false;
        };
        let target = tmp_file.path().to_path_buf();

        let link = dir.join(format!(
            ".safelink_probe_link.{}.tmp",
            target
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "x".into())
        ));

        let ok = symlink_file(&target, &link).is_ok();
        // Clean up unconditionally, including on the failure path: a failed
        // symlink can still leave a dangling entry on some filesystems.
        let _ = std::fs::remove_file(&link);
        // `tmp_file` removes its own target on drop.
        ok
    }
}

/// The closest existing directory at or above `near`.
///
/// A destination whose parent does not exist yet is not evidence of a
/// filesystem limitation -- walk up to something real and ask there. Returns
/// `None` only if nothing in the ancestry exists, which on a mounted
/// filesystem means the path is unusable anyway.
fn nearest_existing_dir(near: &Path) -> Option<std::path::PathBuf> {
    let start = if near.is_dir() { near } else { near.parent()? };
    let mut candidate = start;
    loop {
        if candidate.is_dir() {
            return Some(candidate.to_path_buf());
        }
        candidate = candidate.parent()?;
        if candidate.as_os_str().is_empty() {
            // Relative path exhausted: the current directory is the answer.
            return Some(std::path::PathBuf::from("."));
        }
    }
}

/// Forces a fixed answer, ignoring the path. Test-only in spirit, public so
/// integration tests and callers that already know the environment can use it.
pub struct ForcedProbe(pub bool);

impl SymlinkProbe for ForcedProbe {
    fn can_create_file_symlink(&self, _near: &Path) -> bool {
        self.0
    }
}

#[cfg(windows)]
fn symlink_file(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(target, link)
}

#[cfg(unix)]
fn symlink_file(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forced_probe_reports_what_it_was_given() {
        let p = Path::new("/nonexistent/anywhere");
        assert!(ForcedProbe(true).can_create_file_symlink(p));
        assert!(!ForcedProbe(false).can_create_file_symlink(p));
    }

    #[test]
    fn real_probe_is_callable_and_does_not_panic() {
        // Deliberately does not assert the value: it is environment-dependent
        // and, per Global Constraint 7, CI reports `true` because the runner
        // is elevated. Asserting either way here would encode a falsehood.
        let tmp = tempfile::tempdir().unwrap();
        let _ = RealProbe.can_create_file_symlink(&tmp.path().join("dest"));
    }

    /// On a **symlink-capable** filesystem the answer is knowable, so assert it.
    ///
    /// Scoped to the filesystem, not to POSIX. The previous version of this
    /// test asserted `true` for all of POSIX on the grounds that "POSIX file
    /// symlinks need no privilege" -- true of POSIX *filesystems*, false of
    /// POSIX *hosts*: an exFAT volume mounted on Linux is a POSIX host whose
    /// filesystem refuses symlinks outright (EPERM). That assumption is the
    /// Task 13 bug, so this test must not restate it.
    ///
    /// Without this, a `RealProbe` that ignores the filesystem and returns a
    /// hardcoded `false` passes the whole module. Verified by sabotage: it did.
    #[cfg(unix)]
    #[test]
    fn real_probe_answers_true_on_a_symlink_capable_filesystem() {
        let tmp = tempfile::tempdir().unwrap();
        // Confirm the premise instead of assuming it: if this tempdir somehow
        // cannot hold a symlink, the assertion below would be wrong to make.
        let canary = tmp.path().join("canary");
        if std::os::unix::fs::symlink("t", &canary).is_err() {
            eprintln!(
                "\nSKIPPED: the test tempdir cannot hold symlinks, so this \
                 assertion does not apply here.\n"
            );
            return;
        }
        std::fs::remove_file(&canary).unwrap();

        assert!(
            RealProbe.can_create_file_symlink(&tmp.path().join("dest.txt")),
            "this filesystem demonstrably holds symlinks, so a `false` here \
             means the probe is not actually probing"
        );
    }

    /// A destination whose parent does not exist yet must not be reported as
    /// symlink-incapable -- that is a missing directory, not a filesystem
    /// limitation. Task 10b may create the parent moments later.
    #[cfg(unix)]
    #[test]
    fn real_probe_walks_up_to_the_nearest_existing_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let canary = tmp.path().join("canary");
        if std::os::unix::fs::symlink("t", &canary).is_err() {
            eprintln!("\nSKIPPED: tempdir cannot hold symlinks.\n");
            return;
        }
        std::fs::remove_file(&canary).unwrap();

        let deep = tmp.path().join("does/not/exist/yet/dest.txt");
        assert!(
            RealProbe.can_create_file_symlink(&deep),
            "a not-yet-created parent must not be mistaken for a filesystem \
             that cannot hold symlinks"
        );
    }

    /// The probe must not leave scratch files behind, in the destination's
    /// directory this time -- it runs on every `materialize` call involving a
    /// `FileSymlink`, so a leak accumulates next to the user's real data.
    #[test]
    fn real_probe_leaves_no_files_behind() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("dest.txt");

        let count = || -> usize {
            std::fs::read_dir(tmp.path())
                .map(|rd| rd.filter_map(|e| e.ok()).count())
                .unwrap_or(0)
        };

        let before = count();
        let _ = RealProbe.can_create_file_symlink(&dest);
        assert_eq!(
            count(),
            before,
            "probe leaked scratch files into the destination's directory"
        );
    }
}
