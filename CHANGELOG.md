# Changelog

## 0.1.0 - 2026-09-28

First release. `basic` mode only: heuristics, no AI.

- `stillvalid scan owner/repo` checks every open issue and PR on the development branch and writes `report.json`, each verdict with a confidence and evidence (a commit, a PR, or a `file:line`).
- Issues: `likely_fixed` when a merged PR or a commit on the branch says it fixes the issue, or (medium) when the files, symbols, or error strings the issue names are gone; `needs_info` for bug reports with no repro, version, or code match.
- Pull requests: `superseded` (change already on the branch), `conflicts` (local `git merge-tree`), `abandoned` (author idle, checks failing or conflicting), `ready_unreviewed` (green, not draft, no review).
- Incremental runs with `--previous report.json`.
- `--html <dir>` writes a static dashboard (`index.html`) and a shields.io badge (`badge.json`).
- Opt-in writes: `--labels` (one `stillvalid: <verdict>` label per item) and `--summary-issue` (a pinned "Backlog health" issue); `--dry-run` prints what would change.
- GitHub Action `lodibrahim/stillvalid@v1`: installs the release, scans, and publishes the report to `gh-pages`.
- `--mode free-ai` / `pro-ai` are accepted but not built yet; they run `basic`.
- Prebuilt binaries for macOS (x86_64, aarch64), Linux x86_64, and Windows x86_64, with shell and PowerShell installers.
- npm package `stillvalid` (`npm install -g stillvalid` or `npx stillvalid`), which downloads the release binary; published with npm provenance.
- Release archives have GitHub artifact attestations: `gh attestation verify <file> --repo lodibrahim/stillvalid`.
