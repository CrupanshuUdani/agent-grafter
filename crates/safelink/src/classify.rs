use std::io;
use std::path::Path;

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
    // Order matters. Check the junction first: a junction's mode bits are
    // not a reliable discriminator, so `is_symlink()` must never be the
    // first question asked on Windows. See Global Constraint 5.
    if junction::exists(path)? {
        return Ok(LinkClass::DirLink);
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
