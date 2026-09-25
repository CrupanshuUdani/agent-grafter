# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `safelink` crate: admin-free directory linking — NTFS junctions on Windows, symlinks elsewhere — with removal that never follows a link into its target.
- Declared-pairs manifest (`agent-grafter.toml`): `copy` and `link` entries from a source directory to one or more named targets; known credential filenames and whole-directory entries are refused at parse time.
- `apply`: materializes the manifest, backs up anything it replaces into `.agrafter-backups`, is idempotent, supports `--dry-run`, prunes stale destination files and sweeps old backups.
- `status` and `doctor`: read-only reporting on what is materialized and on install health.
- `unlink`: removes what `apply` created, refusing anything whose ownership cannot be proven.
- `init`: interactive manifest generator; `init --print` writes a commented template to stdout.
- `help` output via clap and a generated man page.
- `apply` says when a declared file `link` was written as a copy because this system can't create a file symlink, since edits to that copy won't reach the source.

### Security

- Writes refuse to escape the declared destination through a symlinked intermediate path component.
