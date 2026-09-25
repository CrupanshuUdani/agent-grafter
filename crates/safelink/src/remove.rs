use std::io;
use std::path::Path;

use crate::classify::{LinkClass, classify};

/// Remove a link **without traversing it**.
///
/// Refuses anything that is not a link, so a mis-declared or
/// attacker-substituted path cannot turn an uninstall into a recursive
/// delete of real data.
///
/// This function must never call `std::fs::remove_dir_all`. It delegates to
/// `remove_dir_link`, whose per-platform primitive unlinks the link object
/// itself without descending into the target.
pub fn remove_link(path: &Path) -> io::Result<()> {
    match classify(path)? {
        LinkClass::NotALink => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "refusing to remove {}: it is not a link, and this function \
                 never deletes real files or directories",
                path.display()
            ),
        )),
        LinkClass::DirLink => remove_dir_link(path),
        LinkClass::FileLink => std::fs::remove_file(path),
    }
}

/// Remove a directory-link object. **The two arms are not interchangeable.**
///
/// Windows: a junction *is* a directory carrying a reparse point, so
/// `remove_dir` is correct and unlinks it without descending.
///
/// POSIX: a directory symlink is *not* a directory. `remove_dir` fails with
/// `ENOTDIR` — the symlink must be unlinked with `remove_file`. Writing one
/// portable `remove_dir` call here compiles fine and fails only at runtime,
/// on POSIX only. (Verified empirically: it was the one bug this plan's code
/// had when first executed.)
///
/// Neither arm may ever use `remove_dir_all`.
#[cfg(windows)]
fn remove_dir_link(path: &Path) -> io::Result<()> {
    std::fs::remove_dir(path)
}

#[cfg(unix)]
fn remove_dir_link(path: &Path) -> io::Result<()> {
    std::fs::remove_file(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::create::create_dir_link;
    use std::fs;
    use std::io::ErrorKind;

    #[test]
    fn refuses_to_remove_a_plain_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join("real_dir");
        fs::create_dir(&d).unwrap();

        let err = remove_link(&d).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
        assert!(d.exists(), "must not have removed a real directory");
    }

    #[test]
    fn refuses_to_remove_a_plain_file() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("real.txt");
        fs::write(&f, b"x").unwrap();

        let err = remove_link(&f).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
        assert!(f.exists(), "must not have removed a real file");
    }

    #[test]
    fn removes_a_directory_link() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target");
        fs::create_dir(&target).unwrap();
        let link = tmp.path().join("link");
        create_dir_link(&target, &link).unwrap();

        remove_link(&link).unwrap();
        assert!(!link.exists());
    }
}
