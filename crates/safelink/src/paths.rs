use std::io;
use std::path::{Path, PathBuf};

/// Strip a trailing path separator, if any, without touching anything else.
///
/// POSIX `lstat` (which `std::fs::symlink_metadata` calls) dereferences a
/// trailing slash: `lstat("link/")` on a directory symlink follows the link
/// and reports the *target*, not the link itself, per POSIX's own
/// specification of pathname resolution. `classify` calls
/// `symlink_metadata` first, so a caller-supplied trailing separator would
/// silently flip a `DirLink` into whatever the target is -- a link
/// materializer's caller could easily produce such a path (e.g. by joining
/// a manifest-declared directory name that itself ends in `/`), so this
/// must be handled before it reaches the OS call, not documented as a
/// caller obligation. `Path`'s own `Components` iterator already discards
/// a trailing separator when re-joined, so this is exact, not an ad hoc
/// string strip.
pub fn normalize_for_classification(p: &Path) -> PathBuf {
    let mut normalized: PathBuf = p.components().collect();
    if normalized.as_os_str().is_empty() {
        // `p` was e.g. just "/" or "" -- components() legitimately yields
        // nothing to collect from a root, so fall back to the original
        // rather than handing classify an empty path.
        normalized = p.to_path_buf();
    }
    normalized
}

/// Windows rejects paths over MAX_PATH (260 characters) unless they carry
/// the `\\?\` "verbatim" prefix. The resulting error (`ERROR_PATH_NOT_FOUND`
/// et al.) reads like a generic failure with no mention of length, which
/// would send a user chasing the wrong cause -- exactly what this project
/// exists to prevent for the admin-rights case, and just as misleading here.
/// `\\?\` also disables the usual path normalisation, so the path MUST
/// already be absolute and fully canonical before prefixing; `canonicalize`
/// already returns a `\\?\`-prefixed path on Windows, so it does double
/// duty here.
#[cfg(windows)]
pub fn verbatim(p: &Path) -> io::Result<PathBuf> {
    const MAX_PATH: usize = 260;
    let s = p.as_os_str();
    if s.len() <= MAX_PATH || p.as_os_str().to_string_lossy().starts_with(r"\\?\") {
        return Ok(p.to_path_buf());
    }
    std::fs::canonicalize(p)
}

#[cfg(unix)]
pub fn verbatim(p: &Path) -> io::Result<PathBuf> {
    Ok(p.to_path_buf()) // POSIX has no equivalent limit to work around.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_a_single_trailing_separator() {
        let p = Path::new("/tmp/some/dir/");
        assert_eq!(normalize_for_classification(p), Path::new("/tmp/some/dir"));
    }

    #[test]
    fn leaves_a_path_without_a_trailing_separator_unchanged() {
        let p = Path::new("/tmp/some/dir");
        assert_eq!(normalize_for_classification(p), Path::new("/tmp/some/dir"));
    }

    #[test]
    fn verbatim_leaves_a_short_path_unchanged() {
        let p = Path::new("short/relative/path");
        assert_eq!(verbatim(p).unwrap(), p);
    }
}
