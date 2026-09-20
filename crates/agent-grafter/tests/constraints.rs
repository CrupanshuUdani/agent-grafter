//! Enforces Global Constraint 3 as a test, not just as a code comment.
//!
//! This mirrors `crates/safelink/tests/constraints.rs` — read that file's
//! doc comment for the full "sabotage-tested" reasoning on why a
//! source-level string scan, not a behavioral test, is the correct check
//! here: on POSIX, `remove_dir_all` against a symlink doesn't recurse into
//! the target (the kernel refuses), so a violation can still pass every
//! behavioral test on this dev machine while being genuinely destructive on
//! Windows, where the same call *does* descend through a junction. That
//! asymmetry is exactly why `safelink` trusts a blunt source-level
//! assertion over runtime coverage, and it applies just as much to
//! `agent-grafter`'s own `src/`, which has no such gate today.
//!
//! `agent-grafter` depends on `safelink` for actual link removal, so this
//! crate's `src/` should never need to call `remove_dir_all` directly at
//! all — this test exists as a structural backstop against that changing
//! silently (as it once did, in a test file, before a human/AI reviewer
//! caught it by hand).
//!
//! Each crate's test binary gets its own `CARGO_MANIFEST_DIR` at compile
//! time, so this is a near-duplicate of `safelink`'s test rather than a
//! shared/generalized one — deliberately, since a workspace-level checker
//! would add real complexity for what is currently exactly two crates.
//!
//! Like `safelink`'s version, this scan is **not recursive**
//! (`std::fs::read_dir`, not a directory walk): it only looks at files
//! directly inside `src/`, not in subdirectories. `agent-grafter/src/` is
//! currently flat, so this is not a functional gap today — but if it ever
//! grows a subdirectory, files there would silently go unchecked.
//!
//! That "not a functional gap today" is no longer left as a claim. Task S8
//! added `src/unlink.rs`, the module that does the most deleting in this
//! crate, and "extend the scan to cover it" would otherwise be a change with
//! no observable effect — the flat layout already covered it, so nobody would
//! find out if that stopped being true. [`the_scan_covers_every_module_that_removes_things`]
//! therefore asserts the scan's own file list, by name: move any of those
//! modules into a subdirectory and the check fails loudly instead of quietly
//! scanning one file fewer.

/// The call this project never makes. Comments and doc-comments legitimately
/// name it, so they are stripped before checking rather than the word being
/// banned outright.
const FORBIDDEN: &str = "remove_dir_all";

/// The removal path must never contain `remove_dir_all`, on any platform,
/// in any `#[cfg]` arm — including arms that are not compiled on this host.
#[test]
fn removal_sources_never_reference_remove_dir_all() {
    let src_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
    let mut offenders = Vec::new();

    for entry in std::fs::read_dir(src_dir).expect("src/ must be readable") {
        let path = entry.expect("readable dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("source must be readable");

        for (n, line) in text.lines().enumerate() {
            let code = match line.find("//") {
                Some(i) => &line[..i],
                None => line,
            };
            if code.contains(FORBIDDEN) {
                offenders.push(format!(
                    "{}:{}: {}",
                    path.file_name().unwrap().to_string_lossy(),
                    n + 1,
                    line.trim()
                ));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "Global Constraint 3 violated — `{FORBIDDEN}` appears in executable code.\n\
         It deletes through a link on Windows and would destroy the user's \
         source of truth.\n\
         Use remove_dir (Windows dir link) or remove_file (POSIX dir link, \
         file link) instead.\n\
         Offending lines:\n  {}",
        offenders.join("\n  ")
    );
}

/// The scan above is only as good as the set of files it actually reads.
///
/// Every module named here contains removal logic, and none of them may
/// escape the scan by being moved into a subdirectory `std::fs::read_dir`
/// does not descend into. This assertion is the difference between a scan
/// that covers them and a scan that happens to cover them.
#[test]
fn the_scan_covers_every_module_that_removes_things() {
    // `apply` (prune and the pre-write unlink), `retention` (the backup
    // sweep), and `unlink` (the whole command).
    const MUST_BE_SCANNED: &[&str] = &["apply.rs", "retention.rs", "unlink.rs"];

    let scanned = scanned_source_files();
    for name in MUST_BE_SCANNED {
        assert!(
            scanned.iter().any(|f| f == name),
            "`{name}` is not among the files the `{FORBIDDEN}` scan reads, so that \
             scan no longer covers it.\n\
             The scan is deliberately non-recursive (`read_dir`, not a walk): a \
             module moved into a subdirectory of `src/` silently goes unchecked.\n\
             Either move it back to the top level of `src/`, or make the scan \
             recursive — and sabotage-verify whichever you choose.\n\
             Files actually scanned: {scanned:?}"
        );
    }
}

/// The exact file list [`removal_sources_never_reference_remove_dir_all`]
/// reads, derived the same way, so the two cannot drift apart.
fn scanned_source_files() -> Vec<String> {
    let src_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
    let mut names: Vec<String> = std::fs::read_dir(src_dir)
        .expect("src/ must be readable")
        .map(|entry| entry.expect("readable dir entry").path())
        .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("rs"))
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}
