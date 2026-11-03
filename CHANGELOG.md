# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-11-03

First release, an alpha. agent-grafter keeps one set of AI agent config and memory in step across machines, operating systems and agent tools. You declare the files in an `agent-grafter.toml` manifest. `copy` entries are copied from a source directory to each named target. `link` entries are linked, so what the agent writes lands in the source.

The work is split across two crates. `safelink` is a library that creates and removes links without administrator rights (NTFS junctions on Windows, symlinks elsewhere) and never follows a link into its target. `agent-grafter` is the command line tool built on it. The manifest format may still change before 1.0.

If it saves you time, a star on the [GitHub repository](https://github.com/CrupanshuUdani/agent-grafter) helps other people find it.

### Added

- Prebuilt binaries for Linux (x86_64/aarch64, static musl), macOS (x86_64/Apple Silicon) and Windows (x86_64/arm64) on GitHub Releases, with SHA256SUMS and THIRD-PARTY-LICENSES.
- `safelink` crate: admin-free directory linking — NTFS junctions on Windows, symlinks elsewhere — with removal that never follows a link into its target.
- Declared-pairs manifest (`agent-grafter.toml`): `copy` and `link` entries from a source directory to one or more named targets; known credential filenames and whole-directory entries are refused at parse time.
- `apply`: materializes the manifest, backs up anything it replaces into `.agrafter-backups`, is idempotent, supports `--dry-run`, prunes stale destination files and sweeps old backups.
- `status` and `doctor`: read-only reporting on what is materialized and on install health.
- `unlink`: removes what `apply` created, refusing anything whose ownership cannot be proven.
- `init`: interactive manifest generator; `init --print` writes a commented template to stdout.
- `help` output via clap and a generated man page.
- `apply` says when a declared file `link` was written as a copy because this system can't create a file symlink, since edits to that copy won't reach the source.
- `status` counts such a copy as correctly in place rather than as drift, and takes its verdict from the same plan `apply` carries out, so `apply`, `status` and `doctor` never disagree about an entry.

### Security

- Writes refuse to escape the declared destination through a symlinked intermediate path component.

[Unreleased]: https://github.com/CrupanshuUdani/agent-grafter/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/CrupanshuUdani/agent-grafter/releases/tag/v0.1.0
