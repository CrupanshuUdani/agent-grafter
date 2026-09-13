//! Black-box tests for the declared-pairs manifest format: `source`, `copy`,
//! `link`, and `[targets.*]`. These exercise `agent_grafter::manifest::parse`
//! directly on raw TOML strings — the safety surface (three refusal
//! categories) does not need a file on disk to prove itself.

use agent_grafter::manifest::parse;

#[test]
fn refuses_to_link_an_entire_config_directory() {
    let e = parse("link = [\".\"]").unwrap_err();
    assert!(e.to_string().contains("whole config directory"), "got: {e}");
}

#[test]
fn refuses_an_empty_entry_as_the_whole_config_directory() {
    let e = parse("link = [\"\"]").unwrap_err();
    assert!(e.to_string().contains("whole config directory"), "got: {e}");
}

#[test]
fn refuses_an_entry_literally_equal_to_source() {
    let e = parse("source = \"sub\"\nlink = [\"sub\"]").unwrap_err();
    assert!(e.to_string().contains("whole config directory"), "got: {e}");
}

#[test]
fn refuses_entries_that_normalize_back_to_the_whole_source_root() {
    // These aren't literally "." or "", but every one of them resolves to
    // the source root once ".." components are walked — the same hole a
    // whole-directory link would open.
    for whole_dir in ["./", "./.", "sub/..", "sub/../."] {
        let e = parse(&format!("link = [\"{whole_dir}\"]")).unwrap_err();
        assert!(
            e.to_string().contains("whole config directory"),
            "accepted {whole_dir} without refusing it as the whole config directory, got: {e}"
        );
    }
}

#[test]
fn refuses_a_path_that_escapes_the_source_root() {
    for bad in ["../elsewhere", "/etc/passwd", "sub/../../out"] {
        assert!(
            parse(&format!("link = [\"{bad}\"]")).is_err(),
            "accepted {bad}"
        );
    }
}

#[test]
fn refuses_a_copy_path_that_escapes_the_source_root() {
    for bad in ["../elsewhere", "/etc/passwd", "sub/../../out"] {
        assert!(
            parse(&format!("copy = [\"{bad}\"]")).is_err(),
            "accepted {bad}"
        );
    }
}

#[test]
fn refuses_known_credential_filenames_outright() {
    // Not a heuristic on content — an explicit deny-list of known-sensitive
    // names, so a typo in a manifest cannot publish a token.
    for bad in [".credentials.json", ".env", "id_rsa"] {
        assert!(
            parse(&format!("copy = [\"{bad}\"]")).is_err(),
            "accepted {bad}"
        );
    }
}

#[test]
fn refuses_known_credential_filenames_in_link_too() {
    for bad in [".credentials.json", ".env", "id_rsa"] {
        assert!(
            parse(&format!("link = [\"{bad}\"]")).is_err(),
            "accepted {bad}"
        );
    }
}

#[test]
fn refuses_a_credential_filename_nested_in_a_subdirectory() {
    let e = parse("copy = [\"sub/.env\"]").unwrap_err();
    assert!(e.to_string().contains(".env"), "got: {e}");
}

#[test]
fn accepts_a_well_formed_manifest_with_targets() {
    let m = parse(
        r#"
source = "."

copy = ["AGENTS.md", "modes/"]
link = ["memory/"]

[targets.aria]
dest = "/home/user/.aria"

[targets.hermes]
dest = "/home/user/.hermes"
"#,
    )
    .expect("well-formed manifest should parse");

    assert_eq!(m.copy, vec!["AGENTS.md".to_string(), "modes/".to_string()]);
    assert_eq!(m.link, vec!["memory/".to_string()]);
    assert_eq!(m.targets.len(), 2);
    assert_eq!(
        m.targets["aria"].dest,
        std::path::PathBuf::from("/home/user/.aria")
    );
}

#[test]
fn defaults_source_to_current_directory_when_omitted() {
    let m = parse("link = [\"memory\"]").expect("should parse");
    assert_eq!(m.source, std::path::PathBuf::from("."));
}

#[test]
fn expands_a_leading_tilde_in_a_targets_dest() {
    // SAFETY (test-only): std::env::set_var/remove_var are documented as
    // unsafe in edition 2024 because they are process-global and racy under
    // concurrent access from other threads. This test runs in-process
    // (unlike the black-box tests in tests/cli.rs, which spawn the binary),
    // so it must not run concurrently with another test that touches HOME.
    // Cargo runs tests in a single test binary on a thread pool by default;
    // to keep this test self-contained without depending on serialization
    // across the crate's test suite, it saves and restores HOME immediately
    // around the single `parse` call.
    let home_key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    let previous = std::env::var_os(home_key);
    unsafe {
        std::env::set_var(home_key, "/home/testuser");
    }

    let result = parse("link = [\"memory\"]\n\n[targets.t]\ndest = \"~/.aria\"\n");

    match previous {
        Some(v) => unsafe { std::env::set_var(home_key, v) },
        None => unsafe { std::env::remove_var(home_key) },
    }

    let m = result.expect("should parse");
    assert_eq!(
        m.targets["t"].dest,
        std::path::PathBuf::from("/home/testuser/.aria")
    );
}
