//! Minimal, private manifest reader — Task S1 scaffolding ONLY.
//!
//! This is deliberately not the real manifest format. It parses just the
//! `link = [...]` and `copy = [...]` TOML arrays, plus a scaffolding-only
//! `[destinations]` table this task defines so `status` has something to
//! run `safelink::classify` against. There is no validation, no path-escape
//! or credential-filename refusal, and no `[targets.*]` destination-root
//! resolution — Task S2 replaces this module wholesale with the real,
//! hardened parser. Not exposed outside this crate.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Deserialize, Default, PartialEq, Eq)]
pub struct Manifest {
    /// Names of entries that should be materialized as links.
    #[serde(default)]
    pub link: Vec<String>,
    /// Names of entries that should be materialized as plain copies.
    /// Parsed per the controller's ruling; S1 has no test that exercises
    /// copy behavior, so `apply --dry-run` only reports these, nothing more.
    #[serde(default)]
    pub copy: Vec<String>,
    /// Scaffolding only: maps a `link` entry's name to the destination path
    /// `status` should classify. Real destination-root resolution
    /// (`[targets.*]`) is S2's job; this is the minimal stand-in that makes
    /// this task's own drift test meaningful.
    #[serde(default)]
    pub destinations: HashMap<String, PathBuf>,
}

#[derive(Debug)]
pub enum ReadError {
    Io(std::io::Error),
    Parse(toml::de::Error),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::Io(e) => write!(f, "could not read manifest: {e}"),
            ReadError::Parse(e) => write!(f, "could not parse manifest: {e}"),
        }
    }
}

/// Read and parse the manifest at `path`.
///
/// Any failure — missing file or unparseable TOML — is reported uniformly
/// as `ReadError`; the caller maps that to exit code 2 ("invalid usage or
/// unparseable manifest").
pub fn read(path: &Path) -> Result<Manifest, ReadError> {
    let text = std::fs::read_to_string(path).map_err(ReadError::Io)?;
    toml::from_str(&text).map_err(ReadError::Parse)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_link_array() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-grafter.toml");
        std::fs::write(&path, "link = [\"memory\"]").unwrap();

        let manifest = read(&path).unwrap();

        assert_eq!(manifest.link, vec!["memory".to_string()]);
        assert!(manifest.copy.is_empty());
        assert!(manifest.destinations.is_empty());
    }

    #[test]
    fn missing_manifest_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.toml");

        assert!(matches!(read(&path), Err(ReadError::Io(_))));
    }

    #[test]
    fn unparseable_manifest_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-grafter.toml");
        std::fs::write(&path, "not valid toml {{{").unwrap();

        assert!(matches!(read(&path), Err(ReadError::Parse(_))));
    }
}
