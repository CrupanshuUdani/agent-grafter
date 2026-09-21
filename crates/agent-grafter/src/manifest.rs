//! The declared-pairs manifest: `source`, `copy`, `link`, and `[targets.*]`.
//!
//! This is *declare-don't-discover*: the tool never scans for agent
//! installations or guesses paths — the user states exactly what goes
//! where. That keeps behavior predictable and correct even for harnesses
//! this tool has never heard of.
//!
//! Three refusals are the safety surface of this format (see `validate_entry`):
//! a `link`/`copy` entry may not name the whole source directory (a config
//! root often holds files like `.credentials.json` that a shared volume
//! cannot enforce `0600` on), may not escape the source root, and may not
//! name a known-credential filename outright.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// A parsed and validated manifest.
#[derive(Debug, PartialEq, Eq)]
pub struct Manifest {
    /// Where the declared entries live, relative to the manifest file.
    pub source: PathBuf,
    /// Entries materialized as plain copies (real behavior: Task S3).
    pub copy: Vec<String>,
    /// Entries materialized as links (real behavior: Task S3).
    pub link: Vec<String>,
    /// Named destinations `copy`/`link` entries are materialized into.
    pub targets: HashMap<String, Target>,
}

/// One `[targets.*]` destination.
#[derive(Debug, PartialEq, Eq)]
pub struct Target {
    /// Destination root, with a single leading `~` already expanded.
    pub dest: PathBuf,
}

#[derive(Debug, Deserialize)]
struct RawManifest {
    #[serde(default = "default_source")]
    source: String,
    #[serde(default)]
    copy: Vec<String>,
    #[serde(default)]
    link: Vec<String>,
    #[serde(default)]
    targets: HashMap<String, RawTarget>,
}

fn default_source() -> String {
    ".".to_string()
}

#[derive(Debug, Deserialize)]
struct RawTarget {
    dest: String,
}

/// Filenames refused outright, regardless of directory, because they are
/// known to hold credentials. Not a content heuristic — a typo in a
/// manifest must not be able to publish a token by accident.
const DENIED_CREDENTIAL_NAMES: &[&str] = &[
    ".credentials.json",
    ".env",
    "id_rsa",
    "id_ed25519",
    ".npmrc",
];

#[derive(Debug)]
pub enum ManifestError {
    /// The manifest file at this path could not be read. Carries the path
    /// alongside the underlying I/O error so `Display` can name which file
    /// was tried — without it, "No such file or directory" tells the user
    /// nothing about what to create.
    Io(PathBuf, std::io::Error),
    Parse(toml::de::Error),
    Invalid(String),
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ManifestError::Io(path, e) => {
                write!(f, "could not read manifest at {}: {e}", path.display())
            }
            ManifestError::Parse(e) => write!(f, "could not parse manifest: {e}"),
            ManifestError::Invalid(msg) => write!(f, "invalid manifest: {msg}"),
        }
    }
}

impl std::error::Error for ManifestError {}

/// Parse a manifest from an already-loaded TOML string, validating every
/// declared `copy`/`link` entry against the three refusals above.
pub fn parse(toml_str: &str) -> Result<Manifest, ManifestError> {
    let raw: RawManifest = toml::from_str(toml_str).map_err(ManifestError::Parse)?;

    for entry in raw.copy.iter().chain(raw.link.iter()) {
        validate_entry(entry, &raw.source)?;
    }

    let targets = raw
        .targets
        .into_iter()
        .map(|(name, t)| {
            (
                name,
                Target {
                    dest: expand_tilde(&t.dest),
                },
            )
        })
        .collect();

    Ok(Manifest {
        source: PathBuf::from(raw.source),
        copy: raw.copy,
        link: raw.link,
        targets,
    })
}

/// Read and parse the manifest at `path`.
///
/// Any failure — missing file, unparseable TOML, or a manifest that fails
/// validation — is reported uniformly as `ManifestError`; the caller maps
/// that to exit code 2 ("invalid usage or unparseable manifest").
pub fn read(path: &Path) -> Result<Manifest, ManifestError> {
    let text =
        std::fs::read_to_string(path).map_err(|e| ManifestError::Io(path.to_path_buf(), e))?;
    parse(&text)
}

/// Reject an entry that names the whole source directory, escapes the
/// source root, or names a known-credential filename outright.
fn validate_entry(entry: &str, source: &str) -> Result<(), ManifestError> {
    let whole_dir_err = || {
        Err(ManifestError::Invalid(format!(
            "entry {entry:?} refers to the whole config directory; \
             declare individual files or subdirectories instead"
        )))
    };

    // A literal match against the empty string, ".", or the manifest's own
    // `source` field is refused outright before any path normalization, so
    // that case reads clearly in the error regardless of how `source` is
    // itself spelled.
    if entry.is_empty() || entry == "." || entry == source {
        return whole_dir_err();
    }

    // Walk the entry's components to compute its net depth relative to the
    // source root. Any `..` that would take it above depth 0 escapes the
    // source root outright; any absolute path does too. An entry that
    // normalizes back to depth 0 (e.g. "./", "sub/..") never escapes, but
    // still names the whole source root, so it gets the same refusal as the
    // literal-string cases above.
    let mut depth: i32 = 0;
    for component in Path::new(entry).components() {
        match component {
            std::path::Component::Prefix(_) | std::path::Component::RootDir => {
                return Err(ManifestError::Invalid(format!(
                    "entry {entry:?} is an absolute path, which escapes the source root"
                )));
            }
            std::path::Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return Err(ManifestError::Invalid(format!(
                        "entry {entry:?} escapes the source root"
                    )));
                }
            }
            std::path::Component::CurDir => {}
            std::path::Component::Normal(_) => depth += 1,
        }
    }
    if depth == 0 {
        return whole_dir_err();
    }

    if let Some(name) = Path::new(entry).file_name().and_then(|n| n.to_str())
        && DENIED_CREDENTIAL_NAMES.contains(&name)
    {
        return Err(ManifestError::Invalid(format!(
            "entry {entry:?} names a known credential file ({name}) \
             and is refused outright"
        )));
    }

    Ok(())
}

/// Expand a single leading `~` in `dest` to the current user's home
/// directory. Not a general path-resolution engine: anything else (`~user`,
/// embedded `~`, relative paths) is passed through unchanged.
fn expand_tilde(dest: &str) -> PathBuf {
    if let Some(rest) = dest.strip_prefix('~')
        && let Some(home) = home_dir()
    {
        let rest = rest.trim_start_matches(['/', '\\']);
        return home.join(rest);
    }
    PathBuf::from(dest)
}

#[cfg(windows)]
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE").map(PathBuf::from)
}

#[cfg(not(windows))]
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_manifest_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.toml");

        assert!(matches!(read(&path), Err(ManifestError::Io(_, _))));
    }

    #[test]
    fn missing_manifest_error_names_the_attempted_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.toml");

        let err = read(&path).expect_err("missing manifest must error");
        let message = err.to_string();
        assert!(
            message.contains(&path.display().to_string()),
            "error message must name the attempted path, got: {message}"
        );
    }

    #[test]
    fn unparseable_manifest_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-grafter.toml");
        std::fs::write(&path, "not valid toml {{{").unwrap();

        assert!(matches!(read(&path), Err(ManifestError::Parse(_))));
    }

    #[test]
    fn read_delegates_to_parse_for_validation_failures() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-grafter.toml");
        std::fs::write(&path, "link = [\".\"]").unwrap();

        assert!(matches!(read(&path), Err(ManifestError::Invalid(_))));
    }
}
