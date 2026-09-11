use std::path::PathBuf;

/// How a single path is materialized at its destination.
///
/// There is deliberately no `Auto` variant. Every entry declares its
/// operation; the engine never infers one from the filesystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkOp {
    /// NTFS junction on Windows, symlink on POSIX. Directories only.
    /// Admin-free and works across local volumes on both platforms.
    DirectoryLink,
    /// Atomic copy. The mandatory fallback for individual files: there is no
    /// admin-free cross-volume single-file link on Windows.
    FileCopy,
    /// Real file symlink. Optional, capability-detected at runtime, never
    /// assumed. Requires Developer Mode or elevation on Windows.
    FileSymlink,
}

/// One declared unit of work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Canonical source of truth. Never modified, never deleted.
    pub source: PathBuf,
    /// Where the source is materialized.
    pub dest: PathBuf,
    pub op: LinkOp,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn entry_carries_an_explicit_op() {
        let e = Entry {
            source: PathBuf::from("/src/AGENTS.md"),
            dest: PathBuf::from("/home/u/.claude/AGENTS.md"),
            op: LinkOp::FileCopy,
        };
        assert_eq!(e.op, LinkOp::FileCopy);
    }

    #[test]
    fn link_ops_are_distinct() {
        assert_ne!(LinkOp::DirectoryLink, LinkOp::FileCopy);
        assert_ne!(LinkOp::FileCopy, LinkOp::FileSymlink);
    }
}
