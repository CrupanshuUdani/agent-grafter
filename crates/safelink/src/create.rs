use std::io;
use std::path::Path;

/// Create a directory link at `link` pointing to `target`.
///
/// Windows: an NTFS junction — admin-free, works across local volumes.
/// POSIX: a directory symlink.
///
/// Deliberately does NOT use a Windows directory *symlink*: that would
/// require Developer Mode or elevation, which is the prerequisite this
/// project exists to remove.
#[cfg(windows)]
pub fn create_dir_link(target: &Path, link: &Path) -> io::Result<()> {
    // A target past MAX_PATH (260 chars) fails with a generic error that
    // reads like a permissions problem unless it carries the `\\?\`
    // verbatim prefix -- see `paths::verbatim`. `link` does not
    // need the same treatment here: `junction::create` builds the junction's
    // reparse data from `target`, and `link` is the (typically short)
    // manifest destination being created, not traversed past its length.
    let target = crate::paths::verbatim(target)?;
    // junction::create takes (target, junction) — target first.
    // It calls `create_dir` to make `link` a plain directory and only then sets
    // the reparse point, so a failure after `create_dir` may leave our own empty
    // directory behind; this cleanup removes it. It must remove nothing else:
    // - `AlreadyExists` means something was already at `link` when this call
    //   tried to create it (for example a user's file or directory, or another
    //   call's junction, finished or still being built), so this call created
    //   nothing and must remove nothing.
    // - For any other error, cleanup runs only if a directory is at `link`, on
    //   the assumption that it is ours. That rests on two expectations, not
    //   guarantees: `create_dir` reports an existing path as `AlreadyExists`,
    //   and `get_full_path`, the one step before it, does not fail on the
    //   valid, existing targets this code passes. If either broke, this could
    //   remove an empty directory this call did not create.
    // - `classify` returning `DirLink` skips a finished junction, since
    //   `remove_dir` unlinks a junction's entry whatever its target holds.
    // `remove_dir`, never `remove_dir_all`: if anything other than an empty
    // directory is at `link`, something worse has gone wrong and we must not
    // silently delete it.
    junction::create(&target, link).inspect_err(|e| {
        if e.kind() != io::ErrorKind::AlreadyExists
            && link.is_dir()
            && !matches!(
                crate::classify::classify(link),
                Ok(crate::classify::LinkClass::DirLink)
            )
        {
            let _ = std::fs::remove_dir(link);
        }
    })
}

#[cfg(unix)]
pub fn create_dir_link(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn creates_a_traversable_directory_link() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("inside.txt"), b"hello").unwrap();

        let link = tmp.path().join("link");
        create_dir_link(&target, &link).unwrap();

        // Reading through the link must see the target's contents.
        assert_eq!(fs::read(link.join("inside.txt")).unwrap(), b"hello");
    }

    /// A failed `create_dir_link` must never remove a directory it did not
    /// create. An empty directory already at `link` is exactly what another
    /// call's junction looks like mid-creation on Windows (`junction::create`
    /// makes a plain directory first and sets the reparse point after), and it
    /// is also what a user's own empty directory looks like.
    #[test]
    fn a_failed_create_never_removes_a_directory_it_did_not_create() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target");
        fs::create_dir(&target).unwrap();
        let link = tmp.path().join("link");
        fs::create_dir(&link).unwrap();

        assert!(
            create_dir_link(&target, &link).is_err(),
            "creating a link over an existing directory must fail"
        );
        assert!(
            link.is_dir(),
            "a failed create_dir_link removed a directory it did not create"
        );
        assert_eq!(
            crate::classify::classify(&link).unwrap(),
            crate::classify::LinkClass::NotALink,
            "the pre-existing directory must be left exactly as it was, not replaced"
        );
    }
}
