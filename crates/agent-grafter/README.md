# agent-grafter

You write one small file listing what to share and where it should go.
agent-grafter then puts those files in place on each system. Files you write
yourself, like instructions and skills, are copied, so each system keeps
working on its own. Files the agent writes, like its memory, are linked, so
every system reads and writes the same copy. On Windows a linked
directory becomes a junction, which any user can create without admin
rights or Developer Mode. A linked single file falls back to a copy there
unless the account can create symlinks.

## Install

```sh
cargo install agent-grafter
```

Or download a prebuilt binary from
[GitHub Releases](https://github.com/CrupanshuUdani/agent-grafter/releases).

## 60-second quickstart

```sh
agent-grafter init            # write agent-grafter.toml interactively
agent-grafter apply --dry-run # preview what would be written
agent-grafter apply           # write it
agent-grafter status          # confirm the linked entries stay in sync
```

It never deletes through a link: `unlink`, or `apply` replacing a link,
removes the link object and leaves its target untouched.

## More

- [README](https://github.com/CrupanshuUdani/agent-grafter/blob/main/README.md):
  full walkthrough, commands, exit codes, and platform support.
- [EXAMPLES.md](https://github.com/CrupanshuUdani/agent-grafter/blob/main/EXAMPLES.md):
  real setups, including dual-boot and two agents sharing memory.
- [FAQ.md](https://github.com/CrupanshuUdani/agent-grafter/blob/main/FAQ.md):
  junctions vs. symlinks, admin rights, unmounted drives, and more.
- [SECURITY.md](https://github.com/CrupanshuUdani/agent-grafter/blob/main/SECURITY.md):
  threat model and how to report a vulnerability.

## License

Apache-2.0.
