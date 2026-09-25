# Examples

Each example is a situation you might be in, the manifest that handles it,
and what running it looks like. Output shown on this page was captured from
real runs on Linux, with long paths shortened to `~`. Where a behaviour has
only been proven in CI and not by a person on a real machine, the example
says so.

A reminder of the two kinds of entry, since every example uses them:

- `copy` puts an independent duplicate in each target. Use it for things you
  write by hand and want to keep working even if the source goes away:
  instructions, rules, skills.
- `link` puts a live link in each target, so every target reads and writes
  the same files. Use it for things the agent writes: memory, notes, state.

## 1. One agent setup, two operating systems

**The situation.** You dual-boot Windows and Linux, and you use the same AI
coding agent on both. You'd like it to have the same instructions on both
sides, and to remember on Linux what it learned on Windows yesterday.

**The setup.** Keep the agent's files on the drive both systems can see (on
Linux something like `/run/media/you/Shared/agent`, on Windows `S:\agent`)
and put the manifest next to them:

```
agent/
├── agent-grafter.toml
├── AGENTS.md
├── skills/
└── memory/
```

```toml
source = "."
copy = ["AGENTS.md", "skills/"]
link = ["memory/"]

[targets.agent]
dest = "~/.claude"
```

The same manifest works on both systems. `source` is resolved relative to
the manifest file, so each OS finds the files at its own path for the shared
drive, and `~` means `HOME` on Linux and `USERPROFILE` on Windows.

Run it once on each side:

```sh
# Linux
agent-grafter apply --manifest /run/media/you/Shared/agent/agent-grafter.toml
```

```powershell
# Windows
agent-grafter apply --manifest S:\agent\agent-grafter.toml
```

On Linux, `~/.claude/memory` becomes a symlink into the shared drive. On
Windows it becomes an NTFS junction, which needs no admin rights. Both point
at the same folder, so a memory written on one boot is there on the next.

`AGENTS.md` and `skills/` are copies on each side. If the shared drive isn't
mounted one day, the agent still has its instructions, but `memory/` points
at nothing until the drive is back, so the agent can't read or save
memories in the meantime. Running `apply` then changes nothing. It refuses
every entry and exits with code 4:

```
claude: refused: AGENTS.md: the declared source /run/media/you/Shared/agent/AGENTS.md does not exist; nothing was written. If it lives on a removable or network volume, mount it and re-run.
```

If the machine already has its own `~/.claude/memory` folder with files in
it, `apply` won't replace it or merge into it. It writes everything else,
skips that one entry and exits with code 4:

```
claude: refused: memory/: a non-empty directory already exists at /home/you/.claude/memory; refusing to replace or merge into it. Merge it by hand (or move it aside) and re-run.
```

Move those files into the shared `memory/` folder, delete the local one, and
run `apply` again.

After you edit `AGENTS.md` on the shared drive, run `apply` again on each
side to update the copies.

> Windows as a standard user with Developer Mode off is covered by CI and
> unit tests but has not yet been confirmed by a person on a real machine.
> See the platform table in the [README](README.md#platform-support).

## 2. Two agents on one machine, sharing memory

**The situation.** You use two different agent tools, and you want both to
follow the same rules and share one memory folder instead of each keeping
its own.

```toml
source = "."
copy = ["AGENTS.md", "skills/"]
link = ["memory/"]

[targets.claude]
dest = "~/.claude"

[targets.other]
dest = "~/.other-agent"
```

Target names are labels you pick. agent-grafter doesn't know or care which
tool reads which directory, so check where each of your tools actually looks
for its files and use that as `dest`. Each target directory has to exist
already; `apply` won't create it, so a typo can't build a tree in the wrong
place.

Preview, then apply:

```
$ agent-grafter apply --dry-run
claude: would write: memory/
claude: would write: AGENTS.md
claude: would write: skills/review.md
other: would write: memory/
other: would write: AGENTS.md
other: would write: skills/review.md

$ agent-grafter apply
claude: wrote: memory/
claude: wrote: AGENTS.md
claude: wrote: skills/review.md
other: wrote: memory/
other: wrote: AGENTS.md
other: wrote: skills/review.md
```

Both tools now see one `memory/` folder:

```
$ ls -l ~/.claude
-rw-r--r-- 1 you you  2 Sep 25 11:52 AGENTS.md
lrwxrwxrwx 1 you you 50 Sep 25 11:52 memory -> /home/you/agent/memory/
drwxr-xr-x 2 you you 60 Sep 25 11:52 skills
```

## 3. A work laptop where you can't get admin rights

**The situation.** Your employer's Windows laptop doesn't give you
administrator rights, and Developer Mode is locked. Tools that rely on
symlinks tell you to get one of those or use WSL.

agent-grafter handles this the same way as example 1. A `link` entry that
names a directory becomes an NTFS junction, which Windows lets a standard
user create. A `link` entry that names a single file has no admin-free
equivalent on Windows, so agent-grafter checks at runtime whether it can
make a symlink and copies the file instead if it can't. It never assumes
either way.

So on a locked-down machine, prefer linking folders (`memory/`) over
individual files. Anything you declare as a single linked file will quietly
be a copy there, and changes to it won't flow back to the source.

Junctions only work between local volumes, so the source has to be on a
local drive, not a network share.

> Same caveat as example 1: this path is covered by tests, not yet by a
> human run on a real non-admin account.

## 4. A config repo you keep in git

**The situation.** You keep your agent's rules and skills in a git repo and
want to install them into the agent's directory, including removing skills
you've deleted from the repo.

```toml
source = "."
copy = ["AGENTS.md", "skills/"]

[targets.agent]
dest = "~/.claude"
```

With no `link` entries, everything is a plain copy. When you delete a file
from a copied directory in the repo, the next `apply` removes it from the
target too, after backing it up:

```
$ git rm skills/test.md && agent-grafter apply
agent: same: AGENTS.md
agent: same: skills/review.md
agent: pruned: skills/test.md (backed up to ~/.claude/.agrafter-backups/20260925T185134Z/skills/test.md)
```

If someone edits the installed copy directly, `doctor` notices and exits
with code 3 (drift found). The first few lines it prints, describing the OS,
manifest and paths, are left out here:

```
$ agent-grafter doctor
...
agent: drift: AGENTS.md is not correctly materialized; re-run `apply` to fix it.
agent: in sync: skills/review.md
agent: in sync: .agrafter-backups backup root
```

Re-running `apply` puts the repo's version back and keeps the edited one in
`.agrafter-backups/`. The last 10 backup directories are kept.

## 5. Trying it out, and taking it back out

Nothing in agent-grafter needs you to trust it on the first run.

- `agent-grafter init` asks a few questions and writes a manifest.
  `init --print` shows the template without writing anything.
- `apply --dry-run` lists every change `apply` would make, and changes
  nothing.
- `doctor` reports on every entry and on the backup folder, and changes
  nothing.
- `unlink` removes what `apply` put there. For the manifest in example 2:

```
$ agent-grafter unlink
claude: removed link: memory/
claude: removed: AGENTS.md
claude: removed: skills/review.md
other: removed link: memory/
other: removed: AGENTS.md
other: removed: skills/review.md
```

`unlink` removes a link without touching what it points at, so your
`memory/` folder is still there afterwards. It only removes a copied file if
the file still matches its source byte for byte. Anything you've changed is
left alone.

## When it's the wrong tool

- **Keeping secrets.** Well-known credential filenames (`.env`, `id_rsa` and
  a few others) are refused, but that's a typo guard, not protection. Keep
  secrets out of anything you point this at. See [SECURITY.md](SECURITY.md).
- **Translating config between tools.** agent-grafter copies and links files
  as they are. If you need one config rewritten into each tool's format,
  look at `agentsync` ([COMPARISON.md](COMPARISON.md)).
- **General dotfiles.** For templating, per-machine variations and secrets
  managers, chezmoi is the mature choice.
- **Session transcripts.** Big, append-only session logs that embed
  OS-specific paths need more than a link. That's out of scope for now.
