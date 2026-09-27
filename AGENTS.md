# stillvalid — agent context

This file applies to all coding agents. `CLAUDE.md` imports it.

Read this first, then `docs/DESIGN.md`, `docs/DECISIONS.md`, `docs/ROADMAP.md`.

## What this project is

`stillvalid` checks every open GitHub issue and PR against the current code and outputs a **verdict + confidence + evidence** per item (e.g. "likely fixed — PR #2977 added the check at `src/compaction.rs:88`"). It ships as a Rust CLI and a GitHub Action, has no server, and publishes results as labels, a pinned summary issue, and a static GitHub Pages dashboard.

## Current state (2026-09-27)

- Design complete; docs in `docs/`.
- `scan` fetches open issues/PRs (`src/fetch.rs`) and writes `report.json` (`src/store.rs`). No checks yet, so every verdict is `cant_tell`.
- `action.yml` is a draft; it assumes release binaries that don't exist yet.
- Next work: the "Setup" and "Core" sections of `docs/ROADMAP.md`.

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
action.yml                  GitHub Action (draft)
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
