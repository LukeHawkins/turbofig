# Releasing turbofig

This file covers the maintainer-only steps to cut a release. It does not
cover contributing; see `CONTRIBUTING.md` for that.

Releases are built by `dist` (cargo-dist) and published as GitHub Releases
plus a Homebrew formula in a separate tap repo. Config lives in
`dist-workspace.toml` and `.github/workflows/release.yml`.

## One-time setup

Do this once, before the first release.

1. Create the public repo `LukeHawkins/homebrew-tap`. Make it empty, with a
   README. `dist` pushes the formula file into it on every release.
2. Create a fine-grained GitHub PAT scoped to the `homebrew-tap` repo only,
   with Contents read and write permission. No other repo and no other
   permission.
3. Save the PAT as a secret named `HOMEBREW_TAP_TOKEN` on the `turbofig`
   repo. This is the exact name `dist` expects; do not rename it.
4. Enable private vulnerability reporting on the `turbofig` repo (Settings →
   Security).

## Each release

1. Run `bun scripts/bump-version.ts x.y.z` with the new version. This updates
   the workspace version, both `package.json` files, `Cargo.lock`, and moves
   the `CHANGELOG.md` Unreleased entries under a dated heading.
2. Check the `CHANGELOG.md` entries read well. Fix wording if needed.
3. Commit the version bump.
4. Tag the commit: `git tag vx.y.z`.
5. Push the commit, then push the tag: `git push`, then `git push --tags`.
6. Watch the `Release` workflow run in GitHub Actions. It builds both macOS
   targets, checks each binary has an embedded plugin, creates the GitHub
   Release, and pushes the updated formula to `LukeHawkins/homebrew-tap`.
7. Verify the release:
   ```bash
   brew update && brew install LukeHawkins/tap/turbofig
   # or, if already installed:
   brew upgrade turbofig
   turbofig --version
   ```

## If a release fails halfway

- A failed `build-local-artifacts` or `build-global-artifacts` job leaves no
  GitHub Release behind; `dist` only creates the release once all builds
  succeed. Fix the failure, then push a new commit and re-tag (delete the
  failed tag locally and on the remote first, or bump to the next patch
  version and tag again). Prefer bumping to the next patch version: it avoids
  a force-push of a tag that CI, or another contributor, may have already
  fetched.
- A failed `publish-homebrew-formula` job after the GitHub Release already
  exists leaves the release live but the tap formula stale. Check the job
  log for the exact `dist`/`brew` error, fix it (usually a token scope or
  expiry issue on `HOMEBREW_TAP_TOKEN`), and re-run that job from the Actions
  UI. Do not re-tag for this: the release artifacts are already correct.
- If the embedded-plugin check step fails, the plugin did not build before
  `cargo build`, or the built binary has no embedded plugin. Check the
  `build-setup.yml` step ran and that `plugin/dist` exists before the Rust
  build step. Do not publish a binary that fails this check.

## Unsigned binaries

Turbofig ships unsigned, unnotarized macOS binaries. This is unverified
against current Homebrew and dist documentation, so treat it as a working
assumption, not a confirmed fact: Homebrew-installed command-line binaries
do not usually trigger the Gatekeeper quarantine prompt that a downloaded
`.app` or a direct browser download gets, because `brew install` does not
set the `com.apple.quarantine` extended attribute the way Finder/Safari do.
Notarization is not set up for this project. If a user reports a Gatekeeper
block, check this assumption first before troubleshooting elsewhere.
