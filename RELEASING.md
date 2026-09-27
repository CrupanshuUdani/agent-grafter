# Releasing agent-grafter

1. Update `CHANGELOG.md`: move the `[Unreleased]` entries into a new
   `## [X.Y.Z] - YYYY-MM-DD` section, and add the compare links at the
   bottom of the file. Leave the `## [Unreleased]` heading in place with
   its (now empty) category headings, such as `### Added`, rather than
   removing them too: `scripts/release-notes.sh` treats a completely
   empty `[Unreleased]` section as an error, and the next step relies on
   it. Bump the version in both crates' `Cargo.toml` (`agent-grafter` and
   `safelink`) and the `safelink` dependency requirement in
   `crates/agent-grafter/Cargo.toml`, keeping all three in lockstep, and
   refresh `Cargo.lock` since release builds use `--locked`. Open a PR
   with these changes, wait for CI, and merge it.
2. Run `gh workflow run release.yml` on `main` (or trigger it from the
   Actions tab). Only a run triggered by a `v*` tag push publishes; a
   `workflow_dispatch` run on `main` builds a dry run: download its
   `release-dry-run` artifact and check the archives, `SHA256SUMS`, and
   release notes before tagging. For a first release, or any change that
   could affect Windows, also run the Gate B checklist below and comment
   its output on the merged PR from step 1.
3. Tag the merge commit and push the tag:
   `git tag -a vX.Y.Z -m vX.Y.Z && git push origin vX.Y.Z`. The tag push
   runs `release.yml` again, this time publishing the GitHub Release.
4. Check the release page: six archives, `SHA256SUMS`, and
   `THIRD-PARTY-LICENSES` should all be attached.
5. Optionally publish to crates.io:
   `gh workflow run publish-crates.yml -f tag=vX.Y.Z -f confirm=publish`.
   This is a separate, manual step: pushing a tag never publishes to
   crates.io on its own. It publishes `safelink` first, then
   `agent-grafter`, since `agent-grafter` depends on the published
   `safelink` version. It needs the `CARGO_REGISTRY_TOKEN` repository
   secret to be set.

## Gate B: Windows standard-user check

Junction creation and the file-link-to-copy fallback need at least one
human run on a real, non-elevated Windows account, because CI's Windows
runners run elevated and can't observe that path.

On a Windows machine, signed in as a standard user (not an administrator)
with Developer Mode off, extract the dry run's Windows zip matching the
machine's architecture (`x86_64-pc-windows-msvc` or
`aarch64-pc-windows-msvc`) and, in order:

- Run `agent-grafter --version`.
- Run `agent-grafter init --print`.
- Run `agent-grafter apply` against a manifest with one directory `link`
  entry and one file `link` entry. Expect the directory entry to become a
  junction, and the file entry's line to include
  `(as a copy: this system can't create a file symlink here, so edits at
  this path won't reach the source)`.
- Run `agent-grafter status`.
- Run `agent-grafter unlink`, then confirm the original source files are
  still intact.

Comment the full output on the merged PR from step 1.
