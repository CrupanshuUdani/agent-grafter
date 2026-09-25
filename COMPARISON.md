# Comparison

Two tools cover ground next to this one: [chezmoi](https://github.com/twpayne/chezmoi),
a general dotfiles manager, and `agentsync`, which is built for AI-agent
config. Both are more mature than agent-grafter, and each is the better
choice for some people. This page tries to say who.

## At a glance

| | agent-grafter | chezmoi | agentsync |
|---|---|---|---|
| Built for | Any set of agent files you declare | Dotfiles in general | AI-agent configs and MCP servers |
| Live directory links on Windows without admin or Developer Mode | Yes, via NTFS junctions | No ([#1005](https://github.com/twpayne/chezmoi/issues/1005), open) | No. Its docs ask for Developer Mode or elevation, or point to WSL |
| One live copy shared by several places (memory, state) | Yes (`link` entries) | No. Directories are deployed as copies | Yes, with symlinks |
| Converts config between harness formats | No | No | Yes |
| Harnesses it knows about | None. Targets are labels you choose | Not applicable | Claude, Copilot, Cursor, Gemini |
| Templating, secrets integration, script hooks | No | Yes | Not verified |
| Maturity | Alpha | Years of production use, large user base | Active: 66 stable releases, v1.49.1 on 2026-08-11 |
| Language | Rust | Go | Rust |

"Not verified" means this project's research didn't check it, not that the
tool lacks it.

## Pick chezmoi if

You want to manage dotfiles in general. chezmoi has templating, secrets
integration and script hooks, and years of real use behind it. agent-grafter
has none of that.

chezmoi syncs in one direction: the home directory is updated to match one
source repo. `chezmoi re-add` pulls machine-side edits back when you ask,
and `exact_` directories track adds and deletes. What it doesn't have is an
object that several places read and write at the same moment. It
deliberately doesn't follow symlinks to directories (to avoid loops), so it
deploys copies instead. On Windows, a real symlink still needs Developer
Mode or `SeCreateSymbolicLinkPrivilege`, and the request to support
admin-free junctions ([#1005](https://github.com/twpayne/chezmoi/issues/1005))
is open with no linked branches or PRs.

None of this is a flaw in chezmoi. Dotfiles don't conflict the way live
agent state can, so it never needed a shared-state feature.

## Pick agentsync if

You use several of the harnesses it supports and want it to translate one
config into each of their formats. agentsync keeps a `.agents/` source of
truth, syncs MCP server definitions too, and prefers symlinks to copies.
agent-grafter does no format translation: it puts the files you name where
you name them, and nothing more.

On Windows, agentsync uses native symlinks. Its documentation lists
Developer Mode or elevation as a prerequisite and points Windows users to a
WSL setup guide. If you control your Windows machine and have Developer Mode
on, that isn't a problem.

## Pick agent-grafter if

- You're on Windows without admin rights (a locked-down work laptop, say)
  and need a live shared directory. Junctions work there; symlinks don't.
- You run more than one OS against the same agent files: dual-boot, or a
  shared drive. Config gets copied, so it keeps working if the drive isn't
  mounted, and memory gets linked, so every OS writes to the same place.
- You want the tool to be safe to run over files you care about. It backs
  up anything it overwrites (keeping the last 10 backups), previews every
  change with `--dry-run`, refuses to follow a link when removing it, and
  refuses outright when a source is missing instead of treating that as
  "delete the destination".
- Your harness isn't on anyone's supported list. agent-grafter has no list;
  a target is just a path.

See [EXAMPLES.md](EXAMPLES.md) for these as concrete setups.

## Sources

The claims about chezmoi and agentsync come from this project's competitor
research (September 2026). Both projects move quickly. If a claim here is out
of date, please open an issue.
