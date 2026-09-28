# stillvalid — agent context

This file applies to all coding agents. `CLAUDE.md` imports it.

Read this first, then `docs/DESIGN.md`, `docs/DECISIONS.md`, `docs/ROADMAP.md`.

## What this project is

`stillvalid` checks every open GitHub issue and PR against the current code and outputs a **verdict + confidence + evidence** per item (e.g. "likely fixed — PR #2977 added the check at `src/compaction.rs:88`"). It ships as a Rust CLI and a GitHub Action, has no server, and publishes results as labels, a pinned summary issue, and a static GitHub Pages dashboard.

## Current state (2026-09-28)

- v0.1.0 released. `basic` mode is complete; `pro-ai` asks a model on any OpenAI-compatible endpoint about issues still `cant_tell` (`src/check/ai.rs`, `src/llm.rs`); `free-ai` runs `basic` (GitHub Models was retired 2026-07-30).
- `scan` fetches issues/PRs (`src/fetch.rs`), clones the repo (`src/repo.rs`), runs the Tier 1 checks (`src/check/`), and writes `report.json` (`src/store.rs`, incremental via `--previous`).
- Outputs: dashboard + badge (`--html`), opt-in labels and "Backlog health" issue (`src/report/`).
- `action.yml` installs the release binary and publishes to `gh-pages`; releases are built by dist (`.github/workflows/release.yml`, see CONTRIBUTING "Releasing").
- Next work: measure `pro-ai` precision on a real model, and "Validate" in `docs/ROADMAP.md`.

## Non-negotiables

- Every verdict carries evidence (commit, PR, or `file:line`). No evidence → `cant_tell`.
- Read-only by default. Never close, lock, or edit issues. Labels/comments only when the config enables them.
- Heuristics before LLM calls. Keep `basic` mode fully functional with no network AI.
- Validate LLM citations: a cited `file:line` must exist at the scanned `head_sha`.
- `report.json` shape follows `schema/report.example.json`; bump `schema_version` on breaking changes.
- Cross-platform: macOS, Linux, Windows. Don't depend on a system `git` feature absent on Windows without a fallback.

## Layout

```
src/main.rs                 CLI entry (clap)
docs/DESIGN.md              architecture, verdicts, heuristics, outputs, cost
docs/DECISIONS.md           decision log
docs/ROADMAP.md             MVP task list
schema/report.example.json  output contract
examples/                   workflow + config examples
action.yml                  GitHub Action
```

Planned modules: `fetch` (GitHub API), `index` (code + references), `check` (heuristics, llm), `store` (report.json, incremental), `report` (html, labels, summary, badge).

## Commands

```sh
cargo build
cargo run -- scan owner/repo --mode basic
cargo fmt && cargo clippy -- -D warnings && cargo test
```

## Conventions

- Rust stable, edition 2021. Errors: `anyhow` in the binary, `thiserror` in library modules.
- Async with `tokio`. GitHub via `octocrab`.
- Keep the dashboard a single embedded HTML template (`include_str!`), no JS build step.
- Update `docs/DECISIONS.md` when making a design decision; tick boxes in `docs/ROADMAP.md` when finishing tasks.
