# stillvalid — Design

Last updated: 2026-09-28

## 1. Problem

Large open-source projects have thousands of open issues and PRs, and nobody can tell which are still true against today's code.

- Many open issues were already fixed, often by a PR that never linked them.
- Many open PRs are superseded by later changes on `main`, or conflict and were abandoned.
- Stale bots close items by **age** ("no activity in 60 days"), not by whether the problem still exists.
- Triage bots label **new** issues but never re-check the backlog.

Result: maintainers can't see the real backlog, contributors pick up dead issues, users file duplicates.

### Prior art (Sept 2026)

Nothing does "re-validate the whole backlog against current code, with evidence, publicly":

- **Metabase Repro-Bot** — reproduces issues and writes failing tests; Metabase-specific.
- **Agno Coda** — daily triage against the codebase, reports to Slack; not a standalone OSS tool.
- **microsoft/IssueLens** — Copilot-SDK triage agent: labels, duplicates, priority; not backlog re-validation.
- **actions/stale** and similar — age-based only.

## 2. Principles

1. **Verdict with evidence, never a bare guess.** Every verdict cites a commit, PR, or `file:line`, plus a confidence level.
2. **Read-only by default.** Suggest labels/comments; never close anything automatically.
3. **Cheap first.** Deterministic heuristics before any LLM call; small model before big model.
4. **Incremental.** Re-check an item only when it or its related code changed.
5. **No server.** Runs as a CLI or a GitHub Action; output is static files.
6. **Public.** The view is for everyone — maintainers, contributors, users.
7. **Branch-aware.** Check the branch where development actually happens (issues auto-close only when a fix reaches the *default* branch; long-lived dev branches make "open" misleading).

## 3. Verdicts

### Issues

| Verdict | Meaning | Example evidence |
|---|---|---|
| `likely_fixed` | Code or history shows it was addressed | Commit abc123 changed the function named in the issue |
| `still_valid` (`pro-ai` only) | Described behavior is still possible in current code | `src/pool.rs:142` still skips release on timeout |
| `duplicate` (planned) | Same problem as another issue | Same stack trace as #3288 |
| `needs_info` | Not enough to judge | No repro steps, version, or matching code |
| `cant_tell` | Some signal, no conclusion | Left for a human |

### Pull requests

| Verdict | Meaning | Example evidence |
|---|---|---|
| `still_applies` (planned) | Rebases cleanly, still relevant | Clean merge-tree against main |
| `superseded` | Its change already exists on main | Identical diff landed in 91b0f3e |
| `conflicts` | Touches files rewritten since | 12 of 19 files changed on main |
| `abandoned` | No author activity, failing checks | Inactive 9 months |
| `ready_unreviewed` | Green, no review | No review for 3 weeks |

Confidence: `high` / `medium` / `low`.

## 4. Architecture

One pipeline, five components, no server:

```
GitHub API ─► Fetcher ─► Indexer ─► Checker ─► Store ─► Reporter
                                       ▲          │
                                       └──────────┘  next run re-checks only what changed
```

| Component | Responsibility |
|---|---|
| **Fetcher** | Pull open issues, PRs, timelines, linked PRs/commits via GitHub REST/GraphQL; clone repo (shallow + needed history) |
| **Indexer** | Map code: files, symbols (tree-sitter), git history per file; extract references from issue text (paths, symbols, stack frames, error strings) |
| **Checker** | Tiered checks (below) producing a verdict + evidence |
| **Store** | `report.json` — previous verdicts, input hashes, file fingerprints; lives on `gh-pages` (Action) or local dir (CLI) |
| **Reporter** | Dashboard HTML, labels, pinned summary issue, badge JSON, optional Projects fields, optional comments |

### Checker tiers

**Tier 1 — heuristics (free, `basic` mode):**
- Issue referenced by a merged PR / commit message (`fixes #N`, `#N`, URL) on the dev branch
- Files/functions named in the issue were deleted or heavily rewritten since the issue was filed
- Stack-frame / error-string from the issue no longer exists in the code
- PR: `git merge-tree` against main → conflicts; diff already present on main → superseded
- PR: author inactivity + check status → abandoned / ready_unreviewed
- Duplicate candidates via title/body similarity (optional small CPU embedding model)

**Tier 2 — retrieval + LLM (`pro-ai`, built; `src/check/ai.rs`, `src/llm.rs`):**
- Only open issues still `cant_tell` after Tier 1; PRs stay heuristic-only
- Retrieve relevant code at `head_sha`, no embeddings: files the issue names (+10), `git grep` hits of its symbols and error strings (+5 each), identifiers and `--flag` names from its text (+3), and title words (+1, any case); source files only unless named (git grep reads only those). Top 4 files, at most 1 of them a test file unless named, ±30 lines around hits, at most 120 lines per file and ~24k characters in all; the issue text is cut at 6k characters. These files (with blob SHAs) are the item's `related_files`
- Ask: "Given this issue and this code, is the described behavior still possible? Cite lines." Structured JSON (`response_format` `json_schema`, falling back to `json_object`): verdict (`likely_fixed` / `still_valid` / `cant_tell`), confidence, reason, citations `[{path, line}]`
- Keep the answer only if it cites at least one line and every cited line is inside the excerpts shown (so it exists at `head_sha`); otherwise `cant_tell`. Evidence = the cited `file:line`s, the reason as note, tier `llm`. Confidence: `likely_fixed` always `low`, `still_valid` at most `medium`
- Order: most reactions, then most comments, then newest; at most `--max-llm-calls` (200) calls. A `429` with `retry-after` ≤ 60 s is waited out once; a `502`/`503`/`504` is retried twice (after 5 s, then 20 s); an answer cut off at the output limit counts as a failed call; a longer or repeated one, a refused key, `x-ratelimit-remaining-requests: 0`, or 5 failures in a row stop calls. The scan never fails on AI errors; unreached issues stay `cant_tell` (tier `none`), and the next run reuses this run's answers and continues with them
- Later: small/cheap model screens; stronger model only for uncertain items

**Tier 3 — reproduction (later, optional):** run repro steps in a sandbox, write a failing test.

### Incremental runs

Store per item: hash of issue body/comments, list of related files + their blob SHAs. Next run re-checks an item only if its hash changed or any related file's SHA changed. First scan is the only expensive one.

## 5. Modes

| Mode | Needs | Uses |
|---|---|---|
| `basic` (default, recommended) | nothing | Tier 1 only |
| `pro-ai` (experimental) | an OpenAI-compatible endpoint (`--ai-base-url`, `--ai-model`, key in `STILLVALID_API_KEY` if needed) | Tier 1 + Tier 2 with any provider (OpenAI, Anthropic's compatibility endpoint, OpenRouter, local Ollama / llama.cpp) |
| `free-ai` | nothing | Was Tier 1 + GitHub Models via `GITHUB_TOKEN`. GitHub retired GitHub Models on 2026-07-30, so it prints a warning and runs `basic` |

Self-hosted runners can point `pro-ai` at a local model for zero per-call cost; that is the free option now.

`pro-ai` is experimental and not promoted: the pitch is PR triage plus fixed-but-forgotten issues, which `basic` covers (see [DECISIONS.md](DECISIONS.md), 2026-09-28; precision in [PILOT.md](PILOT.md)).

## 6. Outputs (where people see it)

Closest to the repo first:

1. **Labels** — `stillvalid: likely-fixed`, `stillvalid: still-valid`, … README links to saved filters (`is:open label:"stillvalid: likely-fixed"`). Needs `issues: write`.
2. **Pinned summary issue** — "Backlog health", rewritten each run with totals + top likely-fixed items.
3. **GitHub Projects (v2) board** — custom fields Verdict / Confidence / Evidence (opt-in).
4. **Sticky comment per issue** — one comment, edited in place (opt-in; some maintainers dislike bot comments).
5. **Dashboard on GitHub Pages** — static HTML reading `report.json`; filters by verdict; every row links to the real item. Read-only permissions suffice.
6. **Badge** — shields.io endpoint JSON.

Audience: maintainers (real backlog), contributors (filter "still valid, unassigned"), users (is my bug already fixed?).

## 7. Distribution

- **Core + CLI in Rust.** Single binary per platform.
- **Release tooling: cargo-dist** — builds macOS (x86_64, aarch64), Linux, Windows on each tag; generates shell/PowerShell installers, Homebrew formula, GitHub Release. Later: winget, Scoop, npm wrapper (`npx stillvalid`).
- **GitHub Action** = thin `action.yml` that downloads the Linux release binary and runs `stillvalid scan` with workflow inputs, then publishes to `gh-pages`.
- Dashboard HTML template is embedded in the binary (`include_str!`), so the CLI can write it locally too.

Suggested crates: `clap` (CLI), `octocrab` (GitHub API), `tokio`, `reqwest`, `serde`/`serde_json`, `git2` or shelling out to `git`, `tree-sitter` (symbols), `minijinja` (HTML template).

## 8. Configuration — `.stillvalid.yml`

```yaml
mode: basic              # basic | free-ai | pro-ai
branch: main             # branch where development happens
skip_labels: [wontfix, discussion]
outputs:
  labels: true
  pinned_summary: true
  dashboard: true
  comments: false
  projects: false
limits:
  max_llm_calls_per_run: 200
  max_cost_usd_per_run: 1.00   # pro-ai only
pulls:                   # also: scan --abandoned-after-days / --unreviewed-after-days
  abandoned_after_days: 180    # no author activity + failing checks or conflicts
  unreviewed_after_days: 21    # green, not draft, open this long with no review
provider:                # pro-ai only
  kind: anthropic        # anthropic | openai | openai-compatible
  model: claude-haiku-4-5
  escalate_model: claude-sonnet-5
```

## 9. Cost

The repo running it pays, never the tool author.

- Heuristics settle many items with no AI call.
- Incremental: nightly runs touch only changed items.
- Cheap model screens, strong model escalates.
- `pro-ai` caps calls per run (`--max-llm-calls`, default 200); about 7k input tokens and at most 4096 output tokens per call, thinking included (`reasoning_effort: low`). A local model costs nothing per call.
- There is no free hosted tier: GitHub Models, the planned `free-ai` backend, was retired on 2026-07-30.
- Large projects: sponsorship funds or AI-vendor OSS credits.

Open question: measure real first-scan cost on a ~3,000-issue repo during the pilot.

## 10. Risks

| Risk | Mitigation |
|---|---|
| Wrong "likely fixed" verdicts erode trust | Evidence required; citation validation; read-only; publish precision from pilot |
| GitHub API rate limits on big repos | GraphQL batching, incremental runs, resumable scans |
| AI provider limits or shutdowns (GitHub Models was retired 2026-07-30) | `basic` mode always works; `pro-ai` takes any OpenAI-compatible endpoint, including a local model |
| Maintainers dislike bots | Labels/dashboard default; comments opt-in |
| Name clash | `stillvalid` free on crates.io, npm, GitHub org, Homebrew as of 2026-09-27; trademark search still to do |
