# Roadmap

## MVP (v0.1)

A CLI + Action that runs `basic` and `free-ai` modes and publishes a static dashboard.

**In scope:** issue + PR verdicts with evidence, `report.json`, one HTML dashboard, labels, pinned summary, incremental re-checks.
**Out of scope for now:** sandbox reproduction, per-issue comments, Projects board, non-GitHub trackers.

### Setup
- [x] Create repo, license, design docs
- [x] CLI skeleton (`stillvalid scan`) that compiles
- [ ] Claim `stillvalid` on crates.io and npm (placeholder 0.0.1)
- [ ] Claim GitHub org `stillvalid`; check `stillvalid.dev`; quick trademark search
- [x] CI: `cargo fmt --check`, `cargo clippy`, `cargo test` on push
- [x] `cargo dist init` (macOS x86_64/aarch64, Linux x86_64, Windows x86_64; shell + PowerShell installers; Homebrew tap)
- [x] npm package on each release (dist npm installer, trusted publishing)
- [x] crates.io publish on each release (trusted publishing)

### Core
- [x] Fetcher: open issues/PRs via REST (`octocrab`), paginated, `GITHUB_TOKEN` / `gh auth token`
- [x] Fetcher: timeline cross-references (linked PRs/commits) via GraphQL
- [x] Clone/fetch target repo; dev-branch aware
- [x] Indexer: extract paths, symbols, error strings, stack frames from issue text
- [x] `report.json` write per [schema/report.example.json](../schema/report.example.json)
- [x] `report.json` read (for `--previous`)
- [x] Incremental: skip items whose body hash and related-file SHAs are unchanged

### Tier 1 heuristics (`basic`)
- [x] Issue referenced by merged PR / commit on dev branch → `likely_fixed`
- [x] Referenced files/symbols deleted or rewritten → `likely_fixed` (medium)
- [x] Missing repro/version/code match → `needs_info`
- [x] PR: `git merge-tree` conflicts → `conflicts`
- [x] PR: diff already on main → `superseded`
- [x] PR: inactivity + checks → `abandoned` / `ready_unreviewed`

### Tier 2 (`free-ai`)
- [ ] GitHub Models client (OpenAI-compatible endpoint, `models: read`)
- [ ] Retrieval of relevant code snippets
- [ ] Structured JSON prompt; validate cited `file:line` exist; drop bad answers to `cant_tell`
- [ ] Rate-limit aware, resumable across runs

### Outputs
- [x] Dashboard HTML (embedded template) — summary tiles, verdict breakdown bar, Issues/PRs tabs, verdict filters, table with evidence
- [x] Labels (opt-in via `issues: write`)
- [x] Pinned "Backlog health" issue
- [x] Badge endpoint JSON

### Action
- [x] `action.yml` downloads release binary, runs scan, pushes to `gh-pages`
- [x] Example workflow in `examples/`

### Validate
- [x] Dogfood on this repo
- [ ] Pilot on one mid-size OSS repo; hand-check 50 verdicts; publish precision
- [ ] Measure first-scan cost and runtime
- [ ] Pitch results to that project's maintainers

## Later
- `pro-ai` providers (Anthropic, OpenAI, OpenAI-compatible/Ollama), model escalation
- Duplicate detection with CPU embeddings
- GitHub Projects fields, sticky comments
- winget / Scoop
- Tier 3 sandbox reproduction
- GitLab / Gitea support
