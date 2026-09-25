# FAQ

### Why junctions instead of symlinks?

Because a directory symlink on Windows needs Developer Mode or
`SeCreateSymbolicLinkPrivilege`, which in practice means running as
administrator, and a directory junction needs neither. A `link` entry that names a directory therefore
becomes an NTFS junction on Windows and a symlink on POSIX: same effect
(reading through it sees the live source), different mechanism, chosen so
it works without asking you to change your account or run as admin.

### Do I need administrator rights?

No. Junction creation, and the file-copy fallback used
everywhere Windows has no admin-free link primitive, both work as a
standard user with Developer Mode off. The one thing this project cannot
yet promise is that this has been *verified by a human running it that
way*. See the platform matrix in [`README.md`](README.md#platform-support)
and [`SECURITY.md`](SECURITY.md) for exactly what's proven by CI versus
what still needs a real non-admin run.

### What happens if my shared drive is unmounted?

`apply` refuses the affected entries rather than guessing: *"the declared
source … does not exist; nothing was written. If it lives on a removable or
network volume, mount it and re-run"*. Nothing is deleted at the
destination just because the source is temporarily unreachable
(`crates/agent-grafter/src/apply.rs`). The same holds for pruning stale
copies: an absent or unreadable source directory yields zero prune
candidates, never a partial guess. An unmounted volume must never be read
as "the source stopped declaring these files."

### Can this delete my files?

`apply` can overwrite a destination it's about to write to, but only after
backing the old content up into `.agrafter-backups/<timestamp>/` under the
target root first. `unlink` removes a link (which never touches what it
points at) or a plain file it has first proven is byte-identical to its
still-existing source; anything else is refused. Neither ever deletes
*through* a link, and pruning inside a declared `copy` directory only ever
touches files inside that directory's own destination subtree, never a
target root at large. See "What this tool will never do" in
[`README.md`](README.md) for the specifics and source references.

### Why not chezmoi? Why not agentsync?

Both are real, more mature-in-places tools. chezmoi is the most mature
dotfiles manager around but does not solve admin-free live directory
sharing on Windows (there's a long-standing open feature request for
exactly this: [twpayne/chezmoi#1005](https://github.com/twpayne/chezmoi/issues/1005)).
`agentsync` is a strong, active, Rust-native competitor with broader
harness support today, but its Windows story relies on native symlinks and
points Windows users at a WSL setup guide rather than junctions. See
[`COMPARISON.md`](COMPARISON.md) for the honest, longer version, including
where each of them is better than this project.

### Why is config copied but state linked?

Because they have different failure modes. Authored config (an `AGENTS.md`,
a mode file, a skill) is something you want present even if the shared
volume it usually lives on isn't mounted right now: `copy` gives every
target its own durable file. Live state (a `memory/` directory multiple
targets read *and write*) is only useful if there's exactly one of it:
`link` makes every target look at the same physical files instead of
maintaining divergent copies nobody reconciles.

### Does it work with Cursor / Copilot / Hermes?

Yes, in the sense that matters: this tool never scans for a particular
agent's install, and never guesses a harness's config path. You declare
`[targets.*]` with whatever `dest` your harness of choice actually expects,
and `copy`/`link` entries with whatever source files you want there:
"predictable and correct even for harnesses this tool has never heard of"
(`crates/agent-grafter/src/manifest.rs`). There is no built-in
Cursor/Copilot/Hermes-specific format conversion, unlike some competitors.
You're declaring plain file placement, not translating between config
schemas.
