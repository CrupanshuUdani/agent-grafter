use std::io;
use std::path::Path;

use crate::paths::normalize_for_classification;

/// What a path is, for the purposes of safe removal.
///
/// Every link variant is a **traversal leaf** and a security boundary:
/// never recurse into one, never delete through one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkClass {
    /// An ordinary file or directory. Not removable by `remove_link`.
    NotALink,
    /// A junction (Windows) or directory symlink (any platform).
    DirLink,
    /// A file symlink.
    FileLink,
}

#[cfg(windows)]
pub fn classify(path: &Path) -> io::Result<LinkClass> {
    let path = &normalize_for_classification(path);
    // Order matters. Check the junction first: a junction's mode bits are
    // not a reliable discriminator, so `is_symlink()` must never be the
    // first question asked on Windows. See Global Constraint 5.
    //
    // `junction::exists` does NOT return `Ok(false)` for an ordinary file or
    // directory — it returns `Err` from the underlying FSCTL_GET_REPARSE_POINT
    // call, with raw OS error 4390 (ERROR_NOT_A_REPARSE_POINT). Confirmed by
    // reading `junction` v2.0.0's own source (`internals.rs`): `Ok(false)` is
    // reserved for a path that does not exist at all; every reparse-point
    // query on an existing-but-plain object surfaces as this specific error
    // code instead. Blindly propagating it with `?` made `classify` return
    // `Err` for every plain file and directory on Windows — caught only when
    // this code first ran on a real Windows kernel in CI, exactly the gap
    // Global Constraint 6 warns about. Only this one error code means
    // "not a junction, keep looking"; anything else (permission denied, path
    // too long, etc.) is a genuine failure and must still propagate.
    const ERROR_NOT_A_REPARSE_POINT: i32 = 4390;
    match junction::exists(path) {
        Ok(true) => return Ok(LinkClass::DirLink),
        Ok(false) => {}
        Err(e) if e.raw_os_error() == Some(ERROR_NOT_A_REPARSE_POINT) => {}
        Err(e) => return Err(e),
    }

    let md = std::fs::symlink_metadata(path)?;
    if !md.file_type().is_symlink() {
        return Ok(LinkClass::NotALink);
    }

    // A symlink that is not a junction: decide dir vs file by what it points
    // at. A broken symlink cannot be a directory link we are responsible for,
    // so classify it as a file link — `remove_file` is the correct primitive
    // for it either way.
    match std::fs::metadata(path) {
        Ok(target_md) if target_md.is_dir() => Ok(LinkClass::DirLink),
        _ => Ok(LinkClass::FileLink),
    }
}

#[cfg(unix)]
pub fn classify(path: &Path) -> io::Result<LinkClass> {
    // A trailing separator makes POSIX `lstat` dereference a directory
    // symlink and report the target, not the link -- normalize before the
    // OS ever sees it rather than document this as a caller obligation.
    // See Task 11 and `paths::normalize_for_classification`.
    let path = &normalize_for_classification(path);
    let md = std::fs::symlink_metadata(path)?;
    if !md.file_type().is_symlink() {
        return Ok(LinkClass::NotALink);
    }
    match std::fs::metadata(path) {
        Ok(target_md) if target_md.is_dir() => Ok(LinkClass::DirLink),
        _ => Ok(LinkClass::FileLink),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::create::create_dir_link;
    use std::fs;

    #[test]
    fn plain_directory_is_not_a_link() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join("plain");
        fs::create_dir(&d).unwrap();
        assert_eq!(classify(&d).unwrap(), LinkClass::NotALink);
    }

    #[test]
    fn plain_file_is_not_a_link() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("plain.txt");
        fs::write(&f, b"x").unwrap();
        assert_eq!(classify(&f).unwrap(), LinkClass::NotALink);
    }

    #[test]
    fn directory_link_is_classified_as_a_dir_link() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target");
        fs::create_dir(&target).unwrap();
        let link = tmp.path().join("link");
        create_dir_link(&target, &link).unwrap();

        assert_eq!(classify(&link).unwrap(), LinkClass::DirLink);
    }
}
