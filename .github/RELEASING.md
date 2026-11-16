# Drafting a release

The `Draft release` workflow builds the macOS arm64 app and creates an unpublished
GitHub Release when you push a stable `vX.Y.Z` tag. The tagged commit must be
reachable from `origin/main`; it does not have to be the latest commit. Tags with
leading zeroes, prerelease suffixes, or build metadata are currently rejected.
Lightweight and annotated tags both work.

1. Bump the version together in `Cargo.toml`, `package.json`,
   `apps/desktop/package.json`, `apps/desktop/src-tauri/tauri.conf.json`, and
   `pyproject.toml`, then merge that commit to `main`.
2. Once CI is green, tag the intended commit and push the tag:

   ```sh
   git fetch origin main
   git tag -a v0.2.0 <commit-on-main> -m "Cadpad 0.2.0"
   git push origin v0.2.0
   ```

3. Wait for `Draft release` to finish. Open the draft under GitHub Releases,
   review its generated notes and attached app ZIP, then publish it manually.

The workflow uses the built-in `GITHUB_TOKEN` with `contents: write`; no new
secret is required. Push the tag with your own credentials: tags created using
another workflow's `GITHUB_TOKEN` do not trigger this workflow.

Rerunning a tag build replaces assets on an existing draft while preserving its
title and notes. It refuses to modify an already published release. Manual
`workflow_dispatch` runs build the supplied existing tag, apply the same checks,
and upload workflow artifacts only; they do not create a release draft.

The app remains ad-hoc signed. Developer ID signing, notarization, and updater
manifests are not yet part of this flow.
