# Future scope

What agent-grafter doesn't do yet, what it might do next, and what it
deliberately won't do. Nothing here is a promise or a schedule. If one of
these matters to you, open an issue and say how you'd use it; that's what
decides the order.

## Being worked on

- **Prebuilt binaries and crates.io.** GitHub Releases for Linux (static
  musl), macOS and Windows on x86_64 and aarch64, with `SHA256SUMS` and
  `THIRD-PARTY-LICENSES`. Publishing `safelink` and `agent-grafter` to
  crates.io will be a separate, manual step.
- **A confirmed run on Windows without admin rights.** CI's Windows runners
  always run elevated, so the standard-user path is covered by tests but not
  yet by a person on a real machine. The first release waits for that run.

## Likely next

- **Session transcript sync.** Memory syncs well because it's one small file
  per fact and a union of files is always correct. Session transcripts are
  different: they're large, append-only, and embed absolute paths that
  differ per OS, so they need path rewriting and probably different storage.
  [dual-os-cc-syc](https://github.com/xhkzdepartedream/dual-os-cc-syc) solves
  part of this for one tool and is the prior art to learn from.
- **Verbose tracing and an audit log.** A `--verbose` flag for debugging, and
  an optional file recording what each `apply` and `unlink` actually did
  (the `wrote`, `same`, `pruned` and `refused` lines it already prints).
- **A confirmation prompt for `unlink`.** "About to remove N entries,
  continue?" when run in a terminal, with `--yes` for scripts. Without a
  terminal it would behave exactly as today.
- **`doctor` noticing file links that became copies.** `apply` says so when
  it makes the copy. On later runs `doctor` reports that file as in sync,
  which is true, but it could also remind you that edits there stay local.
- **Signed binaries.** Releases start unsigned, so macOS Gatekeeper and
  Windows SmartScreen warn on first run. Signing (Apple Developer Program,
  Azure Trusted Signing) costs money every year and will be added if enough
  people use the tool.
- **Package managers.** A Homebrew tap, and possibly winget or Scoop.
- **Colour output** that honours `NO_COLOR` and stays plain text when piped.
- **More code-health checks in CI.** `cargo machete` for unused dependencies,
  and `cargo geiger` to track the `unsafe` blocks in the Windows-only code.

## Ideas, not yet decided

- **When to sync.** Some agent tools have session start and end hooks. One option
  is committing locally when a session ends and pulling when the next one
  starts. That belongs in your own hooks or scripts, not in this tool, unless
  a pattern turns out to be common.
- **Dual licensing as `MIT OR Apache-2.0`,** the usual Rust convention, if
  Apache-2.0 alone causes friction. Apache-2.0 can't be combined with
  GPLv2-only code.
- **A project website** with a short demo.

## Not planned, on purpose

- **Converting config between agent tools' formats.** agent-grafter puts the
  files you name where you name them, unchanged. If you want one config
  rewritten for Claude, Copilot, Cursor and Gemini, `agentsync` does that
  (see [COMPARISON.md](COMPARISON.md)).
- **Built-in knowledge of specific agent tools.** Targets are labels and
  paths you choose. Hard-coding where each tool keeps its files would go
  stale and make the tool less predictable.
- **Finding your files for you.** You declare what to share; the tool never
  scans your disk for things to include.
- **Its own sync service.** Moving bytes between machines is git's job, or a
  shared drive's. agent-grafter puts files in place once they're there.
- **Linking a whole config directory.** A config folder often holds session
  logs and local settings with tokens in them. Declaring individual files
  and folders keeps those from being shared by accident. See
  [SECURITY.md](SECURITY.md).
