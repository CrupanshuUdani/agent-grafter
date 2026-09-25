# Security

## Reporting a vulnerability

Please use GitHub's private vulnerability reporting: **Security → Report a
vulnerability** on this repository. Do not open a public issue for a
security problem.

This repository is currently private. Private vulnerability reporting will
be enabled the moment it's made public; until then, if you've somehow found
this and have something to report, reach the maintainer through GitHub
directly.

## Threat model

agent-grafter materializes files you declared, onto paths you declared. It
does not execute anything, does not fetch anything over the network, and
does not scan your filesystem for things to include. The main things worth
being explicit about:

- **The manifest author is trusted.** A manifest is a declaration of intent
  from whoever wrote it — this tool applies it faithfully, including a
  destructive-looking `[targets.*]` if that's what's declared. It is not a
  sandbox against a hostile manifest, and isn't designed to be: if you
  wouldn't run a shell script from a source, don't run its
  `agent-grafter.toml` either.
- **The destination volume is assumed to be one you control.** Junctions
  and symlinks don't add an access-control boundary of their own — anyone
  who can already read the destination path can read through a link to
  whatever it points at, exactly as if they'd read the source directly.
  This tool doesn't change who can read what; it changes *where* the same
  bytes are reachable from.
- **A shared NTFS mount cannot enforce `0600`, or any POSIX permission bit,
  on files that pass through it.** If your source and destination live on
  different filesystems (a dual-boot shared drive, an SMB share), whatever
  permission bits the source file had are not guaranteed to survive the
  round trip — see [`TROUBLESHOOTING.md`](TROUBLESHOOTING.md) for the
  executable-bit case specifically. Do not rely on this tool, or on the
  filesystem it's writing through, to keep a sensitive file's permissions
  narrow. Keep genuinely sensitive files out of the manifest entirely.
- **Known credential filenames are refused outright, as a backstop, not a
  content scanner.** A manifest entry named `.credentials.json`, `.env`,
  `id_rsa`, `id_ed25519`, or `.npmrc` is rejected at parse time regardless
  of what directory it's in (`crates/agent-grafter/src/manifest.rs`). This
  catches an accidental declaration of an obviously-named secret; it does
  not inspect file contents, and it does not stop you from declaring some
  other file that happens to contain a secret under a filename this list
  doesn't recognize. The list is a typo backstop, not a security boundary.
- **A manifest entry cannot name the whole source directory.** This exists
  for the same reason: a config root often holds files a shared volume
  can't protect (session transcripts, local settings with embedded tokens),
  and declaring the entire directory would materialize all of them
  wherever the manifest says, including anything added to that directory
  later without anyone re-reading the manifest.

None of the above is a substitute for keeping credentials out of any
directory you point this tool at. It reduces a few specific ways a manifest
could leak something by accident; it does not make an unencrypted secret on
a shared volume safe.
