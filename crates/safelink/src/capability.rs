use std::path::Path;

/// Whether this process can create a real file symlink.
///
/// Injectable on purpose. GitHub-hosted Windows runners are documented to run
/// "as administrators with User Account Control (UAC) disabled", so the real
/// probe answers `true` in CI while a standard user with Developer Mode off
/// gets `false`. Tests must be able to force the answer, or the copy fallback
/// is never covered where it matters.
pub trait SymlinkProbe {
    fn can_create_file_symlink(&self) -> bool;
}

/// Probes by actually attempting a symlink in a temp directory and cleaning
/// up. Feature detection, not privilege inspection: the question is "does it
/// work here", not "which privileges are held".
pub struct RealProbe;

impl SymlinkProbe for RealProbe {
    fn can_create_file_symlink(&self) -> bool {
        let Ok(tmp) = tempfile::tempdir() else {
            return false;
        };
        let victim = tmp.path().join("probe_target.txt");
        if std::fs::write(&victim, b"probe").is_err() {
            return false;
        }
        let link = tmp.path().join("probe_link.txt");
        symlink_file(&victim, &link).is_ok()
    }
}

/// Forces a fixed answer. Test-only in spirit, public so integration tests
/// and callers that already know the environment can use it.
pub struct ForcedProbe(pub bool);

impl SymlinkProbe for ForcedProbe {
    fn can_create_file_symlink(&self) -> bool {
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
        assert!(ForcedProbe(true).can_create_file_symlink());
        assert!(!ForcedProbe(false).can_create_file_symlink());
    }

    #[test]
    fn real_probe_is_callable_and_does_not_panic() {
        // Deliberately does not assert the value: it is environment-dependent
        // and, per Global Constraint 7, CI reports `true` because the runner
        // is elevated. Asserting either way here would encode a falsehood.
        let _ = RealProbe.can_create_file_symlink();
    }

    /// On POSIX the answer IS knowable — unprivileged symlink creation always
    /// works — so assert it rather than shrugging.
    ///
    /// Without this, a `RealProbe` that ignores the filesystem and returns a
    /// hardcoded `false` passes the whole module. Verified by sabotage: it did.
    /// The value cannot be asserted on Windows (Constraint 7), which is exactly
    /// why the platform where it *can* be must carry the weight.
    #[cfg(unix)]
    #[test]
    fn real_probe_answers_true_on_posix_where_symlinks_are_unprivileged() {
        assert!(
            RealProbe.can_create_file_symlink(),
            "POSIX file symlinks need no privilege; a `false` here means the \
             probe is not actually probing"
        );
    }

    /// The probe must not leave its scratch files on disk. It runs on every
    /// `materialize` call that involves a `FileSymlink`, so a leak accumulates.
    #[test]
    fn real_probe_leaves_no_files_behind() {
        let tmp_root = std::env::temp_dir();
        let count_probe_artifacts = || -> usize {
            std::fs::read_dir(&tmp_root)
                .map(|rd| {
                    rd.filter_map(|e| e.ok())
                        .filter(|e| {
                            let n = e.file_name();
                            let n = n.to_string_lossy();
                            n.contains("probe_target") || n.contains("probe_link")
                        })
                        .count()
                })
                .unwrap_or(0)
        };

        let before = count_probe_artifacts();
        let _ = RealProbe.can_create_file_symlink();
        assert_eq!(
            count_probe_artifacts(),
            before,
            "probe leaked scratch files into the temp directory"
        );
    }
}
