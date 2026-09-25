# Troubleshooting

Windows traps this project has actually run into, as symptoms you'd
recognize rather than root causes you'd have to already know.

## "mklink appeared to succeed but no junction exists"

If you're testing junction creation by hand from Git Bash or another MSYS
shell, `mklink /J target link` can rewrite the `/J` switch before it ever
reaches Windows, exit 0, and leave nothing created — a silent failure that
looks like success. That's an artifact of invoking `cmd.exe` through an
MSYS environment, not a Windows limitation.

agent-grafter itself never shells out to `mklink` or `cmd.exe` at all — it
creates junctions through the Win32 reparse-point API directly (via the
`junction` crate), so this specific trap doesn't apply to `apply`. It's
worth knowing anyway if you're inspecting or debugging a junction by hand
from a Git Bash prompt: reach for a native `cmd.exe`/PowerShell session, or
`agent-grafter doctor`, rather than trusting an MSYS-invoked `mklink`.

## A long-path failure that looks like a permissions error

Windows rejects paths longer than `MAX_PATH` (260 characters) with an error
that reads like a permissions problem, not a length one — there's no
mention of the actual limit in the message you get back. If `apply` fails
on a deeply nested destination with what looks like an access-denied error,
suspect path length before permissions.

agent-grafter routes junction targets through Rust's `canonicalize`, which
returns a `\\?\`-prefixed "verbatim" path on Windows and bypasses the limit,
so this shouldn't surface in normal use — but if you see a Windows failure
that doesn't otherwise make sense, count the characters in the path first
(`crates/safelink/src/paths.rs`).

## Why `rm -rf` on a junction is dangerous, and what to use instead

On POSIX, `rm -rf` (and `remove_dir_all`) against a symlink does not
recurse into whatever it points at — the kernel refuses to traverse a
symlink for directory removal, so this is safe there. **A Windows NTFS
junction is different: it *is* a real directory as far as recursive removal
is concerned, and `rm -rf`/`remove_dir_all` will descend straight through
it into whatever it points at** — which, for a `link` entry, is your
source of truth. This asymmetry is exactly why this project's own removal
code is structurally forbidden from calling `remove_dir_all` anywhere, on
any platform, enforced by a source-scanning test rather than left as a
convention.

Use `agent-grafter unlink` to remove what `apply` created — it only ever
unlinks the link object itself or removes a plain file it has first proven
is byte-identical to its source, and refuses everything else. If you must
remove a junction by hand on Windows, use `rmdir` (or Explorer's delete on
the link icon), never a recursive POSIX-style delete from Git Bash or WSL
against it.

## NTFS does not preserve the executable bit

NTFS has no concept of a POSIX executable permission bit. A script copied
through a shared NTFS mount (a dual-boot drive, an SMB share) can lose its
`+x` on the Linux/macOS side even though nothing in the copy path did
anything wrong — the bit simply has nowhere to live on the filesystem in
between. If a `copy`-materialized script stops being executable after a
round trip through a shared volume, re-`chmod +x` it; this isn't something
agent-grafter can restore on your behalf, because the information was
already gone before it read the file.

## NTFS case-insensitivity and path identity

NTFS (and default-configured APFS) treats `Foo` and `foo` as the same file;
Linux filesystems don't. Two things follow:

- Two manifest entries that differ only by case can collide on Windows or
  macOS while looking like two unrelated entries on Linux. Don't rely on
  case to distinguish declared entries.
- `unlink`'s safety check (deciding whether a destination is a separate
  object from its source, or the source itself reached through a link)
  canonicalizes paths before comparing them precisely so that a
  case-insensitive filesystem can't make one object look like two —
  `Path/to/Thing` and `path/to/thing` resolve to the same canonical path
  there, and the comparison is done on that resolved form
  (`crates/agent-grafter/src/unlink.rs`).

## Something else?

Open an issue with the output of `agent-grafter doctor` and the three facts
every Windows report needs: OS, whether the process was elevated, and
whether Developer Mode was on. The issue template asks for these directly.
