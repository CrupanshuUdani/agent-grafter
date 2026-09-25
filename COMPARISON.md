# Comparison

Two tools cover adjacent ground: `chezmoi` (general-purpose dotfiles) and
`agentsync` (AI-agent-config-specific). Both are solid, more mature than
this project, and better than agent-grafter along at least one real axis.
This page tries to say where.

## chezmoi

[chezmoi](https://github.com/twpayne/chezmoi) is the most mature dotfiles
manager available — far more mature than agent-grafter, with years of
production use, a large plugin/template system, and broad platform support.

**Where chezmoi is better:** general-purpose dotfiles management well beyond
config files — templating, secrets integration, script hooks, and a large
existing user base. If your problem is "manage my dotfiles," chezmoi is the
safer default choice.

**Where the two differ on Windows directory links, specifically:** chezmoi
is one-directional by design — "a single source of truth, i.e. a single
branch in a single git repo," which updates the home directory to match the
source. `chezmoi re-add` pulls machine-side changes back on request, and
`exact_` directories sync adds and deletes statefully, so it isn't naively
one-shot — but there is no continuously-shared object between machines the
way a `link` entry here is.

More specifically: chezmoi deliberately does not follow symlinks to
*directories* at all, to avoid infinite-loop risk, so it sidesteps rather
than solves live directory sharing — dotfiles are deployed as regular
copied files/directories instead. Creating a real Windows symlink still
requires Developer Mode or `SeCreateSymbolicLinkPrivilege` even when chezmoi
does use one. There has been a long-standing, unresolved feature request
asking chezmoi to support NTFS junctions specifically because they don't
need admin rights
([twpayne/chezmoi#1005](https://github.com/twpayne/chezmoi/issues/1005)),
open with no associated branches or PRs.

**Not chezmoi's job, and not a criticism:** chezmoi has no notion of "one
file, two machines, both writing" at all — dotfiles don't semantically
conflict the way live agent state can, so there was never a reason for it
to grow one.

## agentsync

`agentsync` is the closest competitor — also a Rust CLI, genuinely active,
and specifically about AI-agent configs:
it syncs agent configs and MCP servers across Claude/Copilot/Cursor/Gemini
from a `.agents/` source of truth, and explicitly favors "symlinks over
copies."

**Where agentsync is better:** it supports more agent harnesses today, with
built-in format conversion between them. agent-grafter is
declare-don't-discover and harness-agnostic — it materializes whatever
paths you name, for any harness, but does no per-harness format translation
of its own.

**Where the two differ on Windows:** agentsync relies on native symlinks,
which — per the research above — require Developer Mode or elevation on
Windows. It documents this as a prerequisite and points Windows users at a
WSL setup guide rather than using junctions. That is the fallback this
project exists to avoid.

## Where agent-grafter differs from both

Not "syncs AI agent config files" — that category has real competitors now.
The narrower claim: explicit junction/symlink/copy semantics with runtime
capability detection instead of a Developer-Mode or WSL prerequisite, and a
declared `link` vs. `copy` distinction so authored config (`copy`, "safe for
entries a shared volume might not always be mounted") and live state
(`link`, "one copy, read and written by every target") are handled
differently on purpose, not incidentally.

## Sources

Every claim above about `chezmoi` or `agentsync` is drawn from this
project's own research notes (competitor survey and verification, not
reproduced here); it is not this file's job to re-derive them. If you find
one of these claims stale — both projects move — please open an issue.
