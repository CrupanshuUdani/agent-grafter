//! Enforces a critical safety rule as a test, not just as a code comment.
//!
//! Why this exists, and why the flagship safety test is not enough:
//!
//! On POSIX, `remove_dir_all` against a *symlink* does not recurse into the
//! target — the kernel refuses to traverse a symlink for directory removal.
//! So a build that violates the constraint outright still passes
//! `tests/safety.rs` on Linux and macOS. Verified by deliberately sabotaging
//! `remove_dir_link` to call `remove_dir_all`: the flagship test stayed green.
//!
//! On Windows the same violation is genuinely destructive, because a junction
//! *is* a directory and `remove_dir_all` will descend through it into the
//! user's source of truth. That is the case the project exists to prevent, and
//! it is the one case the host running these tests cannot observe.
//!
//! A source-level assertion is therefore the only check that fails on every
//! platform. It is crude on purpose: the constraint is absolute, so a blunt
//! instrument is the correct shape.

/// The removal path must never contain `remove_dir_all`, on any platform,
/// in any `#[cfg]` arm — including arms that are not compiled on this host.
#[test]
fn removal_sources_never_reference_remove_dir_all() {
    // Comments and doc-comments legitimately name the forbidden call, so
    // strip them before checking rather than banning the word outright.
    const FORBIDDEN: &str = "remove_dir_all";

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
        "`{FORBIDDEN}` appears in executable code.\n\
         It deletes through a link on Windows and would destroy the user's \
         source of truth.\n\
         Use remove_dir (Windows dir link) or remove_file (POSIX dir link, \
         file link) instead.\n\
         Offending lines:\n  {}",
        offenders.join("\n  ")
    );
}
