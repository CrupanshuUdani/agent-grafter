//! Task 11: path-shape hardening.
//!
//! On Windows a target path past MAX_PATH (260 chars) fails with a generic
//! error that reads like a permissions problem -- exactly the wrong-cause
//! red herring this project exists to remove. The other cases here (spaces,
//! unicode, trailing separators) are portable and already exercised on every
//! platform; the long-path one is Windows-only by construction (POSIX has no
//! equivalent limit) and its absence here is not evidence either way -- see
//! Global Constraint 6. Only the `windows-latest` CI job proves it.

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
