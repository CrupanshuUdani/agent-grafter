# Releasing agent-grafter

1. Update `CHANGELOG.md`: move the `[Unreleased]` entries into a new
   `## [X.Y.Z] - YYYY-MM-DD` section below an empty `## [Unreleased]`
   heading, and update the link definitions at the bottom of the file:

   ```markdown
   [Unreleased]: https://github.com/CrupanshuUdani/agent-grafter/compare/vX.Y.Z...HEAD
   [X.Y.Z]: https://github.com/CrupanshuUdani/agent-grafter/compare/vPREVIOUS...vX.Y.Z
   ```

   For the first release, use
   `[0.1.0]: https://github.com/CrupanshuUdani/agent-grafter/releases/tag/v0.1.0`.
   Keep these definitions at the bottom of the file: `release-notes.sh`
   stops a section at the first line shaped like `[X]: ...`.

   Bump the version in both crates' `Cargo.toml` (`agent-grafter` and
   `safelink`) and the `safelink` dependency requirement in
   `crates/agent-grafter/Cargo.toml`, keeping all three in lockstep, and
   refresh `Cargo.lock` since release builds use `--locked`. Open a PR
   with these changes, wait for CI, and merge it.
2. Run `gh workflow run release.yml` on `main` (or trigger it from the
   Actions tab). Only a run on a `v*` tag ref creates the draft release:
   the tag push, a re-run of it, or a dispatch with `--ref vX.Y.Z`. A
   `workflow_dispatch` run on `main` builds a dry run:

   ```sh
   gh workflow run release.yml --ref main
   gh run list --workflow release.yml --branch main --event workflow_dispatch --limit 1   # note the run ID
   gh run watch <run-id>
   gh run download <run-id> -n release-dry-run -D /tmp/release-dry-run
   ```

   Dry-run archives are named `agent-grafter-vX.Y.Z-dev.<sha>-<target>`.
   Check the archives and `SHA256SUMS`. The dry run also runs
   `check-version.sh` and, once the version is bumped, previews the
   `## [X.Y.Z]` section, so `notes.md` in the artifact should show exactly
   the notes the release will carry.
3. Tag the commit the dry run built, and push the tag.

   ```sh
   git switch main && git pull --ff-only
   sha="$(gh run view <run-id> --json headSha -q .headSha)"
   git tag -a vX.Y.Z "$sha" -m vX.Y.Z && git push origin vX.Y.Z
   ```

   The tag push runs `release.yml` again, this time creating a draft
   GitHub Release with all the assets. A draft is not public, and
   `releases/latest` ignores it.
4. Run Gate B on the draft. It is required for a first release or any
   change that could affect Windows; otherwise go straight to step 7.
   Open an issue for it if one doesn't exist (title pattern "Gate B:
   verify vX.Y.Z Windows binaries"). Download the draft's Windows zips:

   ```sh
   gh release download vX.Y.Z -p '*windows-msvc.zip' -D /tmp/gateb-vX.Y.Z
   ```

   Run the Gate B checklist below and post the full output on that issue.
5. If Gate B fails, nothing is public yet, so delete the draft and its
   tag, fix the cause in a PR, and start again from step 2:

   ```sh
   gh release delete vX.Y.Z --yes --cleanup-tag && git tag -d vX.Y.Z
   ```

   A tag run that fails before its "Create the GitHub Release as a draft"
   step created nothing. A transient failure can be re-run from the
   Actions tab, since the re-run still runs on the tag. Otherwise delete
   the tag, fix the cause in a PR, and start again from step 2:

   ```sh
   git push --delete origin vX.Y.Z && git tag -d vX.Y.Z
   ```
6. Once Gate B passes, open a docs PR that records the result where the
   repo states Windows support (the standard-user row of the README
   platform table), links the Gate B issue, and closes it with
   `Closes #N`. Merge it. The release archives carry the README as of the
   tagged commit, so they won't show this result; the release notes link
   the Gate B issue instead (next step).
7. Edit the draft's notes to add a line linking the Gate B issue (when
   Gate B ran), either on the release page or with
   `gh release edit vX.Y.Z --notes-file notes.md`. The flag replaces the
   whole body, so start from the current one with
   `gh release view vX.Y.Z --json body -q .body > notes.md`. Check the
   page: six archives, `SHA256SUMS`, and `THIRD-PARTY-LICENSES` should all
   be attached. Then publish the release:

   ```sh
   gh release edit vX.Y.Z --draft=false
   ```
8. Optionally publish to crates.io, but only once the repository is
   public. Run
   `gh workflow run publish-crates.yml -f tag=vX.Y.Z -f confirm=publish`.
   This is a separate, manual step: pushing a tag never publishes to
   crates.io on its own, and the workflow refuses to publish while the
   GitHub Release is still a draft. It publishes `safelink` first, then
   `agent-grafter`, since `agent-grafter` depends on the published
   `safelink` version. The token is the `CARGO_REGISTRY_TOKEN` secret of
   the `crates-io` environment, which only `main` can deploy to, so
   dispatch the workflow on `main`. The token needs the `publish-new`
   scope for the first publish of each crate and `publish-update` after
   that. The workflow packages and verifies both crates before publishing
   either, and if `agent-grafter` fails after `safelink` is published,
   re-running the workflow skips `safelink` and resumes.

## Gate B: Windows standard-user check

Junction creation and the file-link-to-copy fallback need at least one
human run on a real, non-elevated Windows account, because CI's Windows
runners run elevated and can't observe that path.

On a Windows machine, signed in as a standard user (not an administrator)
with Developer Mode off, extract the draft release's Windows zip matching
the machine's architecture (`x86_64-pc-windows-msvc` or
`aarch64-pc-windows-msvc`). In an empty directory, create
`src\memory\note.md`, `src\notes.md`, an empty `dest\` directory, and this
`agent-grafter.toml`:

```toml
source = "src"
link = ["memory", "notes.md"]

[targets.gateb]
dest = "dest"
```

Then, in order:

- Run `agent-grafter --version`. Expect `agent-grafter X.Y.Z`.
- Run `agent-grafter init --print`. Expect the commented template.
- Run `agent-grafter apply`. Expect a `wrote: memory` line, and a
  `wrote: notes.md` line that includes
  `(as a copy: this system can't create a file symlink here, so edits at
  this path won't reach the source)`. In PowerShell,
  `(Get-Item dest\memory).LinkType` should print `Junction`.
- Run `agent-grafter status`. Expect `in sync:` for both entries and exit
  code 0.
- Run `agent-grafter unlink`. Expect `removed link: memory` and
  `removed: notes.md`, then confirm `src\memory\note.md` and `src\notes.md`
  are still intact.

Post the full output on the release's Gate B issue.
