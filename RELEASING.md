# Releasing agent-grafter

The git tag, the GitHub Release and both crates share one version, `X.Y.Z`.
`scripts/check-version.sh` enforces this in both workflows.

1. Open a PR that moves the `[Unreleased]` entries in `CHANGELOG.md` into a
   `## [X.Y.Z] - YYYY-MM-DD` section (update the link definitions at the
   bottom, which `release-notes.sh` relies on), bumps both crates and the
   `safelink` requirement in `crates/agent-grafter/Cargo.toml`, and refreshes
   `Cargo.lock`. Merge it.
2. Build a candidate from `main`. The dry run needs no tag:

   ```sh
   gh workflow run release.yml --ref main
   gh run watch <run-id>
   gh run download <run-id> -n release-dry-run -D <empty-dir>
   ```

3. Verify the candidate. Windows-affecting changes need a human run on a real
   standard-user account (CI's Windows runners are elevated). Record the
   result in a PR rather than in this file.
4. Tag the commit you verified, from `main`, and push the tag. The tag run
   builds a draft GitHub Release. Check it, then publish it:

   ```sh
   git tag -a vX.Y.Z -m vX.Y.Z && git push origin vX.Y.Z
   gh release edit vX.Y.Z --draft=false
   ```

   If the tag run fails before the draft exists, delete the tag and start
   again from step 2. If it left a partial draft, a re-run refuses to add a
   second one: run `gh release delete vX.Y.Z --yes` (this keeps the tag),
   then re-run.
5. Optionally publish to crates.io, once the repository is public:

   ```sh
   gh workflow run publish-crates.yml --ref main -f tag=vX.Y.Z -f confirm=publish
   ```

   It refuses a draft release, publishes `safelink` and then `agent-grafter`
   from the tagged commit, and resumes if `agent-grafter` fails after
   `safelink` went out. The token is the `CARGO_REGISTRY_TOKEN` secret of the
   `crates-io` environment, which only `main` can deploy to. crates.io
   versions can be yanked but never replaced, so a published version is never
   re-tagged: fix forward with the next version.
