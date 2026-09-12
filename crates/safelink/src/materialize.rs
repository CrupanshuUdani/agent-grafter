use std::io;

use crate::capability::SymlinkProbe;
use crate::copy::copy_atomic;
use crate::create::create_dir_link;
use crate::op::{Entry, LinkOp};

/// Materialize one entry, returning the operation **actually applied**.
///
/// The only permitted divergence from the declared op is
/// `FileSymlink -> FileCopy` when the probe reports symlinks are
/// unavailable. Everything else is honored exactly as declared: the engine
/// does not guess.
pub fn materialize(entry: &Entry, probe: &dyn SymlinkProbe) -> io::Result<LinkOp> {
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

    match entry.op {
        LinkOp::DirectoryLink => {
            create_dir_link(&entry.source, &entry.dest)?;
            Ok(LinkOp::DirectoryLink)
        }
        LinkOp::FileCopy => {
            copy_atomic(&entry.source, &entry.dest)?;
            Ok(LinkOp::FileCopy)
        }
        LinkOp::FileSymlink => {
            if probe.can_create_file_symlink() {
                symlink_file(&entry.source, &entry.dest)?;
                Ok(LinkOp::FileSymlink)
            } else {
                copy_atomic(&entry.source, &entry.dest)?;
                Ok(LinkOp::FileCopy)
            }
        }
    }
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
        let applied = materialize(&entry, &ForcedProbe(true)).unwrap();

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
        let applied = materialize(&entry, &ForcedProbe(false)).unwrap();

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
        let applied = materialize(&entry, &ForcedProbe(true)).unwrap();

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
            materialize(&entry, &ForcedProbe(true)).unwrap(),
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

        let result = materialize(&entry, &ForcedProbe(true));

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
