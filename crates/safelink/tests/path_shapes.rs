//! Task 11: path-shape hardening.
//!
//! On Windows a target path past MAX_PATH (260 chars) fails with a generic
//! error that reads like a permissions problem -- exactly the wrong-cause
//! red herring this project exists to remove. The other cases here (spaces,
//! unicode, trailing separators) are portable and already exercised on every
//! platform; the long-path tests are Windows-only by construction (POSIX has
//! no equivalent limit) and their absence here is not evidence either way --
//! see Global Constraint 6. Only the `windows-latest` CI job proves them,
//! including the combined long-path-with-spaces-and-unicode case below,
//! which is not implied by the two dimensions passing independently.

use safelink::classify::{LinkClass, classify};
use safelink::create::create_dir_link;
use std::path::PathBuf;
use tempfile::tempdir;

#[test]
fn handles_spaces_and_unicode_in_paths() {
    let d = tempdir().unwrap();
    for name in ["AI workspace", "ünïcødé dir", "with'quote", "a.b.c"] {
        let src = d.path().join(name);
        std::fs::create_dir(&src).unwrap();
        let dst = d.path().join(format!("link-{name}"));
        create_dir_link(&src, &dst).unwrap_or_else(|e| panic!("failed on {name:?}: {e}"));
        assert_eq!(classify(&dst).unwrap(), LinkClass::DirLink);
    }
}

#[test]
fn a_trailing_separator_does_not_change_classification() {
    let d = tempdir().unwrap();
    let src = d.path().join("src");
    std::fs::create_dir(&src).unwrap();
    let dst = d.path().join("dst");
    create_dir_link(&src, &dst).unwrap();

    let with_sep = PathBuf::from(format!("{}/", dst.display()));
    assert_eq!(
        classify(&dst).unwrap(),
        classify(&with_sep).unwrap(),
        "a trailing separator must not change what a path is classified as"
    );
}

// MAX_PATH is 260. Windows accepts longer paths only via the \\?\ prefix, and
// the failure without it does NOT look like a length problem.
#[cfg(windows)]
#[test]
fn creates_a_link_whose_target_exceeds_max_path() {
    let d = tempdir().unwrap();
    let mut deep = d.path().to_path_buf();
    for _ in 0..20 {
        deep = deep.join("a-reasonably-long-directory-component-name");
    }
    std::fs::create_dir_all(&deep).expect("create_dir_all past MAX_PATH");
    assert!(
        deep.as_os_str().len() > 260,
        "test is not exercising a long path"
    );

    let dst = d.path().join("deep-link");
    create_dir_link(&deep, &dst)
        .expect("a target past MAX_PATH must succeed via the verbatim prefix");
    assert_eq!(classify(&dst).unwrap(), LinkClass::DirLink);
}

/// The two dimensions above are tested independently everywhere else in this
/// file: `handles_spaces_and_unicode_in_paths` proves spaces/unicode work at
/// ordinary length, `creates_a_link_whose_target_exceeds_max_path` proves
/// length past MAX_PATH works with a plain-ASCII path. Neither proves the
/// combination, and there is a real way it could fail independently of both:
/// `verbatim()`'s only long-path branch is `std::fs::canonicalize`, and
/// canonicalize is exactly the kind of path-processing step that could
/// mishandle spaces or non-ASCII components even though the un-prefixed,
/// under-260 fast path never touches it. Only Windows CI is evidence here —
/// see Global Constraint 6.
#[cfg(windows)]
#[test]
fn creates_a_link_whose_target_is_both_past_max_path_and_contains_spaces_and_unicode() {
    let d = tempdir().unwrap();
    let mut deep = d.path().to_path_buf();
    for _ in 0..20 {
        deep = deep.join("a component with spaces and ünïcødé");
    }
    std::fs::create_dir_all(&deep).expect("create_dir_all past MAX_PATH with spaces/unicode");
    assert!(
        deep.as_os_str().len() > 260,
        "test is not exercising a long path"
    );

    let dst = d.path().join("deep-link with spaces and ünïcødé");
    create_dir_link(&deep, &dst).expect(
        "a target that is both past MAX_PATH and contains spaces/unicode \
         must succeed via the verbatim prefix",
    );
    assert_eq!(classify(&dst).unwrap(), LinkClass::DirLink);
}
