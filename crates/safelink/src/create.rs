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
    // junction::create takes (target, junction) — target first.
    junction::create(target, link)
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
