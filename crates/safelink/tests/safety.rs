//! The safety property this project exists to guarantee.
//!
//! Runs on every platform: it exercises a junction on Windows and a
//! directory symlink on POSIX, via the same public API.

use safelink::create::create_dir_link;
use safelink::remove::remove_link;
use std::fs;

#[test]
fn removing_a_dir_link_never_deletes_the_targets_contents() {
    let tmp = tempfile::tempdir().unwrap();

    // The "source of truth" that must survive, with nested content so a
    // recursive delete would be unambiguous if it happened.
    let source = tmp.path().join("source_of_truth");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("canary.txt"), b"must survive").unwrap();
    let nested = source.join("nested");
    fs::create_dir(&nested).unwrap();
    fs::write(nested.join("deep.txt"), b"must also survive").unwrap();

    // Materialize it somewhere else, then tear the link down.
    let link = tmp.path().join("live_link");
    create_dir_link(&source, &link).unwrap();
    assert_eq!(
        fs::read(link.join("canary.txt")).unwrap(),
        b"must survive",
        "precondition: the link must actually be traversable"
    );

    remove_link(&link).unwrap();

    assert!(!link.exists(), "the link itself should be gone");
    assert!(
        source.join("canary.txt").exists(),
        "SAFETY VIOLATION: removal deleted through the link"
    );
    assert!(
        nested.join("deep.txt").exists(),
        "SAFETY VIOLATION: removal recursed through the link"
    );
    assert_eq!(
        fs::read(source.join("canary.txt")).unwrap(),
        b"must survive"
    );
}

#[test]
fn removal_refuses_a_real_directory_even_when_it_looks_like_a_target() {
    let tmp = tempfile::tempdir().unwrap();
    let real = tmp.path().join("not_a_link");
    fs::create_dir(&real).unwrap();
    fs::write(real.join("data.txt"), b"precious").unwrap();

    assert!(remove_link(&real).is_err());
    assert!(real.join("data.txt").exists());
}
