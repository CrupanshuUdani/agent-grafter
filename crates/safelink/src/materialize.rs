use std::io;
use std::path::Path;

use crate::capability::SymlinkProbe;
use crate::copy::copy_atomic;
use crate::create::create_dir_link;
use crate::op::{Entry, LinkOp};

/// Caller-supplied policy for the destination side of a materialization.
///
/// This exists because the two destination-side failure modes need opposite
/// treatments, and only one of them is a policy question:
///
///   - *Missing parent hierarchy* is a policy question. Auto-creating it is
///     convenient for a fresh machine and reckless for a manifest whose
///     destination root is expected to already exist — a typo in `dest` then
///     silently builds a whole tree in the wrong place instead of reporting
///     it. So it is a flag, and the caller decides.
///   - *Permission denied* is not a policy question and is deliberately
///     absent from this struct. See `annotate_dest_error`.
///
/// An interactive prompt is not an option here: `materialize` is a pure
/// per-entry function with no terminal in its call path, run across a whole
/// declarative manifest and possibly unattended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializeOptions {
    /// Create the destination's missing parent directories before applying
    /// the op. Applies identically to every `LinkOp`.
    ///
    /// When `false`, a missing parent is refused and named, and **nothing**
    /// is created — not the parent, not the destination.
    pub create_missing_parents: bool,
}

impl Default for MaterializeOptions {
    /// `create_missing_parents: true`.
    ///
    /// Chosen to match what the code already did before this option existed,
    /// so the default carries no behavior change: `copy_atomic` has always
    /// called `create_dir_all` on the destination's parent unconditionally.
    /// `DirectoryLink` did not, which made the same manifest shape succeed
    /// for one op kind and fail for another — an accident of which function
    /// happened to call `create_dir_all`, not a designed distinction. The
    /// default resolves that split toward the existing majority behavior;
    /// `false` is what a caller opts into.
    fn default() -> Self {
        Self {
            create_missing_parents: true,
        }
    }
}

/// Materialize one entry, returning the operation **actually applied**.
///
/// The only permitted divergence from the declared op is
/// `FileSymlink -> FileCopy` when the probe reports symlinks are
/// unavailable. Everything else is honored exactly as declared: the engine
/// does not guess.
pub fn materialize(
    entry: &Entry,
    probe: &dyn SymlinkProbe,
    options: &MaterializeOptions,
) -> io::Result<LinkOp> {
    // Checked before anything looks at, classifies, or touches the
    // destination. A missing source can never justify mutating the
    // destination, so there is no ordering in which inspecting `dest` first
    // would be correct.
    //
    // The canonical source routinely lives on a removable or shared volume,
    // so absence is the common case, not an exotic one. Bailing out here
    // covers all three destination states at once, each for its own reason:
    //
    //   - dest absent:      refuse rather than leave a dangling link behind.
    //   - dest is a link:   a stale link, not a broken one. The volume may
    //                       come back; removing it destroys a working
    //                       configuration for no benefit.
    //   - dest is real data: never touch real data on the strength of a
    //                       source that merely is not mounted right now.
    //
    // `exists()` is the right predicate here despite following symlinks: the
    // question is whether the source resolves to real content, and a source
    // that is itself a dangling link resolves to nothing. Emptiness is NOT
    // part of the question — an empty source is legitimate and proceeds.
    if !entry.source.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "source does not exist: {} -- refusing to materialize. If this \
                 is a removable volume, mount it and re-run; the existing \
                 destination was left untouched.",
                entry.source.display()
            ),
        ));
    }

    // Before the `match`, deliberately: this is the one place the parent
    // policy is applied, so every op kind gets the identical decision from
    // the identical code. Putting it inside the arms — or inside
    // `copy_atomic` and `create_dir_link` — is how the two paths drifted
    // apart in the first place.
    prepare_dest_parent(&entry.dest, options)?;

    let applied = match entry.op {
        LinkOp::DirectoryLink => {
            create_dir_link(&entry.source, &entry.dest).map(|()| LinkOp::DirectoryLink)
        }
        LinkOp::FileCopy => copy_atomic(&entry.source, &entry.dest).map(|()| LinkOp::FileCopy),
        LinkOp::FileSymlink => {
            if probe.can_create_file_symlink() {
                symlink_file(&entry.source, &entry.dest).map(|()| LinkOp::FileSymlink)
            } else {
                copy_atomic(&entry.source, &entry.dest).map(|()| LinkOp::FileCopy)
            }
        }
    };

    // `map_err`, never a fallback: an error stays an error. The result is
    // still `Err` in every case it was `Err` before — only the message
    // improves. See `directory_link_failure_propagates_as_an_error_not_a_
    // silent_downgrade` for the property this must not break.
    applied.map_err(|e| annotate_dest_error(&entry.dest, e))
}

/// Apply the missing-parent policy to `dest`, before anything touches it.
///
/// Returns `Ok(())` without creating anything when the parent already
/// exists, which is the overwhelmingly common case.
fn prepare_dest_parent(dest: &Path, options: &MaterializeOptions) -> io::Result<()> {
    let Some(parent) = dest.parent() else {
        // A root path, e.g. `/`. There is no parent to create; let the op
        // itself fail on its own terms rather than inventing an error here.
        return Ok(());
    };
    // `dest.parent()` yields `""` for a bare relative file name like
    // `AGENTS.md`. That means the current directory, which exists by
    // definition — `Path::new("").exists()` is `false`, so this case has to
    // be excluded explicitly or a bare file name would be refused outright.
    if parent.as_os_str().is_empty() || parent.exists() {
        return Ok(());
    }

    if !options.create_missing_parents {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "destination parent directory does not exist: {} -- refusing to \
                 materialize {} because create_missing_parents is disabled. \
                 Create the directory yourself and re-run, or enable \
                 create_missing_parents; nothing was created.",
                parent.display(),
                dest.display()
            ),
        ));
    }

    std::fs::create_dir_all(parent).map_err(|e| annotate_dest_error(dest, e))
}

/// Name the destination in a permission failure; pass everything else
/// through untouched.
///
/// Permission denied is never negotiable and is deliberately not covered by
/// `MaterializeOptions`: a destination the user cannot write is a fact about
/// the machine, and silently working around it (by relaxing modes, or by
/// writing somewhere else) is exactly the kind of guess this engine does not
/// make. The only thing worth improving is the diagnostic — the raw OS error
/// on Linux is the bare string "Permission denied (os error 13)", which does
/// not say *which* path, and a manifest run touches many. Task 10's
/// missing-source error already names its path; this makes the destination
/// side consistent with it.
///
/// The original error's `Display` is folded into the new message rather than
/// discarded, so the OS errno survives for debugging.
fn annotate_dest_error(dest: &Path, e: io::Error) -> io::Error {
    if e.kind() != io::ErrorKind::PermissionDenied {
        return e;
    }
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!(
            "permission denied writing destination {}: {e} -- refusing to \
             materialize. Fix the permissions on the destination or its parent \
             directory and re-run; the destination was left untouched.",
            dest.display()
        ),
    )
}

#[cfg(windows)]
fn symlink_file(target: &std::path::Path, link: &std::path::Path) -> io::Result<()> {
    std::os::windows::fs::symlink_file(target, link)
}

#[cfg(unix)]
fn symlink_file(target: &std::path::Path, link: &std::path::Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::ForcedProbe;
    use std::fs;

    #[test]
    fn directory_link_entry_creates_a_traversable_link() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("src_dir");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("f.txt"), b"v").unwrap();
        let dest = tmp.path().join("dest_dir");

        let entry = Entry {
            source,
            dest: dest.clone(),
            op: LinkOp::DirectoryLink,
        };
        let applied =
            materialize(&entry, &ForcedProbe(true), &MaterializeOptions::default()).unwrap();

        assert_eq!(applied, LinkOp::DirectoryLink);
        assert_eq!(fs::read(dest.join("f.txt")).unwrap(), b"v");
    }

    #[test]
    fn file_symlink_downgrades_to_copy_when_unsupported() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("s.txt");
        fs::write(&source, b"payload").unwrap();
        let dest = tmp.path().join("d.txt");

        let entry = Entry {
            source,
            dest: dest.clone(),
            op: LinkOp::FileSymlink,
        };
        // ForcedProbe(false) reaches the fallback even on an elevated CI host.
        let applied =
            materialize(&entry, &ForcedProbe(false), &MaterializeOptions::default()).unwrap();

        assert_eq!(applied, LinkOp::FileCopy, "must downgrade, not fail");
        assert_eq!(fs::read(&dest).unwrap(), b"payload");
        // A copy, not a link: removing it must be refused by remove_link.
        assert!(crate::remove::remove_link(&dest).is_err());
    }

    #[test]
    fn file_symlink_is_used_when_supported() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("s.txt");
        fs::write(&source, b"payload").unwrap();
        let dest = tmp.path().join("d.txt");

        let entry = Entry {
            source,
            dest: dest.clone(),
            op: LinkOp::FileSymlink,
        };
        let applied =
            materialize(&entry, &ForcedProbe(true), &MaterializeOptions::default()).unwrap();

        assert_eq!(applied, LinkOp::FileSymlink);
        assert_eq!(fs::read(&dest).unwrap(), b"payload");
    }

    #[test]
    fn file_copy_entry_never_consults_the_probe() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("s.txt");
        fs::write(&source, b"payload").unwrap();
        let dest = tmp.path().join("d.txt");

        let entry = Entry {
            source,
            dest: dest.clone(),
            op: LinkOp::FileCopy,
        };
        assert_eq!(
            materialize(&entry, &ForcedProbe(true), &MaterializeOptions::default()).unwrap(),
            LinkOp::FileCopy
        );
        assert_eq!(fs::read(&dest).unwrap(), b"payload");
    }

    /// `DirectoryLink` has no downgrade path — unlike `FileSymlink`, nothing
    /// in the plan authorizes falling back to a copy when the link fails.
    ///
    /// Without this test, `materialize` could catch the `create_dir_link`
    /// error and silently substitute a copy, and every other test in this
    /// module would still pass — none of them make `create_dir_link` fail.
    /// That silent substitution is precisely the "the engine never guesses"
    /// guarantee this whole plan is built on. Verified by sabotage:
    /// swallowing the error and returning `Ok(DirectoryLink)` anyway is
    /// caught here — `Got: Ok(DirectoryLink)` — and stays uncaught by every
    /// other test in the module.
    ///
    /// (A copy-on-failure fallback, the more obvious sabotage, could not be
    /// used to demonstrate this: `entry.source` is a directory, and
    /// `copy_atomic` cannot copy a directory, so that particular fallback
    /// fails for an unrelated reason and produces a false-positive catch.
    /// The result still had to be inspected, not just its error-ness.)
    #[test]
    fn directory_link_failure_propagates_as_an_error_not_a_silent_downgrade() {
        let tmp = tempfile::tempdir().unwrap();

        // A valid source: if anything silently falls back to copy, the copy
        // itself would succeed, so only checking "materialize failed" is not
        // enough — the fallback path must never be reachable at all.
        let source = tmp.path().join("valid_source_dir");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("f.txt"), b"would be copyable").unwrap();

        // Occupy the destination with a real file so create_dir_link cannot
        // succeed: a directory link cannot be created where a file already
        // exists.
        let dest = tmp.path().join("dest");
        fs::write(&dest, b"already here").unwrap();

        let entry = Entry {
            source,
            dest: dest.clone(),
            op: LinkOp::DirectoryLink,
        };

        let result = materialize(&entry, &ForcedProbe(true), &MaterializeOptions::default());

        assert!(
            result.is_err(),
            "a failing DirectoryLink must return an error, never Ok(FileCopy) \
             or any other silently substituted operation. Got: {result:?}"
        );
        // The failure must not have touched the occupied destination — the
        // one outcome a silent copy-fallback would produce.
        assert_eq!(
            fs::read(&dest).unwrap(),
            b"already here",
            "a failed materialize must not modify an existing destination"
        );
    }
}
