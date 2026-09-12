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
    // verbatim prefix -- see Task 11 and `paths::verbatim`. `link` does not
    // need the same treatment here: `junction::create` builds the junction's
    // reparse data from `target`, and `link` is the (typically short)
    // manifest destination being created, not traversed past its length.
    let target = crate::paths::verbatim(target)?;
    // junction::create takes (target, junction) — target first.
    // On failure it may leave the empty `link` directory behind (it creates
    // the directory before setting the reparse point), so clean it up. Use
    // `remove_dir`, never `remove_dir_all`: if anything other than an empty
    // directory exists at `link`, something worse has gone wrong and we must
    // not silently delete it.
    junction::create(&target, link).map_err(|e| {
        if link.is_dir() {
            let _ = std::fs::remove_dir(link);
        }
        e
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
}
