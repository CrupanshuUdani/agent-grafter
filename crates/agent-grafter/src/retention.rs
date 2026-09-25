//! Backup retention: keep the most recent N backup directories, remove the
//! rest.
//!
//! Split out of `apply.rs` because nothing here needs a manifest, a target, or
//! a plan — it is a pure question about the names of the subdirectories inside
//! one `.agrafter-backups/` directory, plus the narrow removal primitive that
//! acts on the answer. Keeping it separate is what lets [`plan_retention`] be
//! tested against fabricated directory names with no wall-clock waiting at all.
//!
//! **Two rules run through everything below.**
//!
//! *Only this tool's own bookkeeping is eligible.* A subdirectory counts
//! toward the limit, and may be removed, **only** when its name is exactly a
//! `YYYYMMDDTHHMMSSZ` timestamp of the shape `apply` itself writes. Anything
//! else inside `.agrafter-backups/` — a user's stray note, a directory they
//! named themselves, a link — is not counted, not sorted, and never removed.
//! A sort-then-truncate over everything in the directory is precisely how that
//! rule gets broken by accident, so the filter happens *before* the sort.
//!
//! *Removal is hand-rolled and classify-gated.* Emptying a backup directory
//! needs recursive deletion. We never use the standard library's recursive-delete
//! helper. [`remove_backup_dir`] therefore walks the tree itself, asks
//! `safelink::classify` about every single entry before touching it, unlinks
//! plain files with `std::fs::remove_file`, and removes each directory with
//! `std::fs::remove_dir` — which fails on a non-empty directory, so a refusal
//! anywhere in the tree stops the removal instead of blowing past it.

use std::io;
use std::path::{Path, PathBuf};

use safelink::classify::{LinkClass, classify};

/// How many backup directories a run keeps, per target root.
pub const KEEP_BACKUPS: usize = 10;

/// The length of a `YYYYMMDDTHHMMSSZ` timestamp.
const TIMESTAMP_LEN: usize = 16;

/// How deep [`remove_backup_dir`] will descend before giving up.
///
/// Belt, not braces: classify-before-recurse already makes a link cycle
/// impossible, since a link is never descended into. This is here so that a
/// pathological tree cannot blow the stack, and it errors rather than
/// silently stopping half-way.
const MAX_DEPTH: usize = 64;

/// Whether `name` is exactly the timestamp form `apply` names its backup
/// directories with — `YYYYMMDDTHHMMSSZ`, produced by `apply::utc_timestamp`.
///
/// Deliberately a shape check and not a date parse: the only question that
/// matters is "did this tool write it", and a name of this exact shape sorts
/// lexically in time order whether or not it denotes a real instant.
pub fn is_backup_dir_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.len() != TIMESTAMP_LEN {
        return false;
    }
    bytes[..8].iter().all(u8::is_ascii_digit)
        && bytes[8] == b'T'
        && bytes[9..15].iter().all(u8::is_ascii_digit)
        && bytes[15] == b'Z'
}

/// Which directories under `backups_dir` should be removed to leave at most
/// `keep` of the most recent validly-named ones.
///
/// Decides only; removes nothing. The caller chooses *when* to act on this,
/// which matters: the sweep is executed before a run writes any backup of its
/// own, so it can never delete a directory the run is in the middle of filling.
///
/// `prospective` is the backup directory name this run is *about* to create,
/// when it is going to create one. It is counted toward `keep` — this run's own
/// backup is the newest of all and must survive — but it is never returned for
/// removal, since it does not exist yet and is not this sweep's to delete.
/// Passing `None` asks the plain question: what should be removed given only
/// what is already on disk.
///
/// A `backups_dir` that does not exist yet is not an error — it is the normal
/// state before the first backup is ever taken — and yields no removals.
///
/// Entries that are not plain directories named as a valid timestamp are
/// invisible here: not counted toward `keep`, never returned. That includes a
/// link whose name *looks* like a timestamp, which is left alone rather than
/// followed.
pub fn plan_retention(
    backups_dir: &Path,
    keep: usize,
    prospective: Option<&str>,
) -> io::Result<Vec<PathBuf>> {
    let read = match std::fs::read_dir(backups_dir) {
        Ok(read) => read,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };

    // Filter first, sort second. The other order is the bug this function
    // exists to not have.
    let mut names: Vec<String> = Vec::new();
    for entry in read {
        let entry = entry?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if !is_backup_dir_name(&name) {
            continue;
        }
        let path = entry.path();
        if !matches!(classify(&path), Ok(LinkClass::NotALink)) {
            continue;
        }
        if !std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir()) {
            continue;
        }
        names.push(name);
    }

    // This run's own directory counts toward the limit. When the run shares a
    // second with an earlier one the directory already exists, so it is only
    // added when genuinely new.
    let prospective = prospective.filter(|ts| is_backup_dir_name(ts));
    if let Some(ts) = prospective
        && !names.iter().any(|n| n == ts)
    {
        names.push(ts.to_string());
    }

    names.sort();
    let Some(excess) = names.len().checked_sub(keep) else {
        return Ok(Vec::new());
    };

    Ok(names
        .into_iter()
        .take(excess)
        // Never hand back the directory this run is about to write into, even
        // at `keep == 0`: it is not on disk, and removing it is not a thing
        // that could be meant.
        .filter(|name| prospective != Some(name.as_str()))
        .map(|name| backups_dir.join(name))
        .collect())
}

/// Remove one backup directory and everything under it.
///
/// Hand-rolled rather than delegating to the standard library's recursive
/// delete. That helper follows whatever it finds; we never take that risk with
/// a user's disk, even inside a directory we believe we wrote ourselves.
///
/// Every object — the root included — is put to `safelink::classify` before
/// anything happens to it. A link found anywhere in the tree stops the
/// removal with an error naming it, rather than being unlinked or descended
/// into. No link should ever be in here (only plain files are ever backed up),
/// which is exactly why finding one means something is wrong enough to stop
/// for.
///
/// Failure leaves a partially removed tree. That is the honest outcome: the
/// alternative is pressing on past something that refused, and the caller
/// reports the failure rather than hiding it.
pub fn remove_backup_dir(dir: &Path) -> io::Result<()> {
    remove_tree(dir, 0)
}

fn remove_tree(dir: &Path, depth: usize) -> io::Result<()> {
    if depth > MAX_DEPTH {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "refusing to descend past {MAX_DEPTH} levels while removing {}",
                dir.display()
            ),
        ));
    }
    if !matches!(classify(dir)?, LinkClass::NotALink) {
        return Err(link_refusal(dir));
    }

    let mut children: Vec<PathBuf> = std::fs::read_dir(dir)?
        .map(|e| e.map(|e| e.path()))
        .collect::<io::Result<_>>()?;
    // `read_dir` order is unspecified; sorted so a failure part-way through
    // leaves a reproducible state.
    children.sort();

    for child in children {
        match classify(&child)? {
            LinkClass::DirLink | LinkClass::FileLink => return Err(link_refusal(&child)),
            LinkClass::NotALink => {
                if std::fs::symlink_metadata(&child)?.is_dir() {
                    remove_tree(&child, depth + 1)?;
                } else {
                    std::fs::remove_file(&child)?;
                }
            }
        }
    }

    // Not the recursive variant: this only succeeds once the loop above has
    // emptied the directory, so anything the loop declined to remove keeps its
    // parent alive instead of being swept away with it.
    std::fs::remove_dir(dir)
}

fn link_refusal(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "refusing to remove the backup directory: {} is a link, and this \
             tool never deletes a link's target or deletes through one",
            path.display()
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_exact_timestamp_shape_is_a_backup_directory_name() {
        assert!(is_backup_dir_name("20240229T123045Z"));
        assert!(is_backup_dir_name("19700101T000000Z"));

        assert!(!is_backup_dir_name(""));
        assert!(!is_backup_dir_name("notes"));
        // Right length, wrong shape.
        assert!(!is_backup_dir_name("2024022912304512"));
        assert!(!is_backup_dir_name("20240229t123045Z"));
        assert!(!is_backup_dir_name("20240229T123045z"));
        // Right shape, wrong length.
        assert!(!is_backup_dir_name("20240229T123045Z "));
        assert!(!is_backup_dir_name("0240229T123045Z"));
        // A name that would sort *after* every real timestamp, which is the
        // position that makes a sort-then-truncate implementation look correct
        // on a casual test and be wrong in production.
        assert!(!is_backup_dir_name("zzzz-user-notes"));
    }
}
