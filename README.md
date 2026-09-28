# stillvalid

**Is that issue still valid?** `stillvalid` checks every open GitHub issue and pull request against the current code and tells you which ones are already fixed, still real, duplicated, or dead — with evidence.

> **Status: early release (v0.1.0).** `basic` mode is complete; `pro-ai` (bring your own model) is new and not yet measured against a real model. See [docs/ROADMAP.md](docs/ROADMAP.md).

**Live example:** [this repo's dashboard](https://lodibrahim.github.io/stillvalid/stillvalid/) [![stillvalid](https://img.shields.io/endpoint?url=https://lodibrahim.github.io/stillvalid/stillvalid/badge.json)](https://lodibrahim.github.io/stillvalid/stillvalid/), updated nightly by [`.github/workflows/stillvalid.yml`](.github/workflows/stillvalid.yml). It goes live once GitHub Pages is enabled for this repo.

## Why

Large open-source projects carry thousands of open issues and PRs. Many were fixed long ago by a PR that never linked them; many PRs were superseded by later changes on `main`. Existing stale bots close things by *age*, not by whether the problem still exists. Nobody — maintainers, contributors or users — can see the real backlog.

`stillvalid` answers one question per item: **is this still true against today's code?**

## What you get

For every open issue and PR, a **verdict + confidence + evidence** (a commit, a PR, or a `file:line`):

| Issues | Pull requests |
|---|---|
| Likely fixed · Still valid · Duplicate · Needs info · Can't tell | Still applies · Superseded · Conflicts · Abandoned · Ready, unreviewed |

Results show up where people already look:

- **Labels** on each issue (`stillvalid: likely-fixed`), so GitHub's own issue list becomes the dashboard
- A **pinned "Backlog health" issue**, rewritten each run
- A static **dashboard on GitHub Pages** anyone can open, no login
- A **README badge**: `stillvalid · 96 likely fixed`
- Optional: a **GitHub Projects** board with Verdict / Confidence / Evidence fields

It is **read-only by default** and **never closes anything**. Maintainers decide.

## Install (planned)

Single native binary for macOS, Linux and Windows, no runtime needed:

```sh
brew install stillvalid                 # macOS / Linux
winget install stillvalid               # Windows
cargo install stillvalid                # from source
npm install -g stillvalid               # any OS with Node.js; downloads the release binary
npx stillvalid scan owner/repo          # run once without installing
curl -sSf https://…/install.sh | sh     # any Unix
```

```sh
stillvalid scan owner/repo --mode basic --html site
```

Release archives carry GitHub build attestations; verify a download with `gh attestation verify <file> --repo lodibrahim/stillvalid`.

## Use in a repo

Copy [examples/workflow.yml](examples/workflow.yml) to `.github/workflows/stillvalid.yml`:

```yaml
permissions:
  contents: write        # publish the report to the gh-pages branch
  issues: read           # set to `write` to enable labels / summary-issue
  pull-requests: read
jobs:
  scan:
    runs-on: ubuntu-latest
    steps:
      - uses: lodibrahim/stillvalid@v1
        with:
          mode: basic
```

Each run installs the released binary (`version`, default `latest`), scans, and commits `report.json`, `index.html` and `badge.json` to `stillvalid/` on the `gh-pages` branch (created on the first run; no commit when nothing changed). The next run reads that `report.json` as its previous report. Inputs: `mode`, `branch`, `version`, `publish-pages` (default `true`), `labels` and `summary-issue` (default `false`, need `issues: write`), and for `pro-ai`: `ai-model`, `api-key`, `ai-base-url`, `max-llm-calls`.

To see the dashboard, turn on Pages once: **Settings → Pages → Build and deployment → Deploy from a branch**, branch `gh-pages`, folder `/ (root)`. It is then at `https://<owner>.github.io/<repo>/stillvalid/`. The link in the "Backlog health" issue assumes that address, so it won't match a custom Pages domain.

## Labels and the summary issue

Off by default; the scan writes nothing to GitHub unless asked. Both need a token with `issues: write` (without one, or on a 403, the scan prints a warning and still writes its report).

```sh
stillvalid scan owner/repo --labels --summary-issue --dashboard-url https://owner.github.io/repo/
stillvalid scan owner/repo --labels --summary-issue --dry-run   # print what would change
```

- `--labels` puts one `stillvalid: <verdict>` label on each issue and PR (none for "can't tell"), creates missing labels, and swaps it when the verdict changes. Other labels are never touched. Saved filter: `is:open label:"stillvalid: likely-fixed"`.
- `--summary-issue` keeps one pinned "Backlog health" issue up to date: totals per verdict and the top likely-fixed issues with evidence. If a maintainer closes it, it stays closed.

## Modes

| Mode | Needs | What it can tell you |
|---|---|---|
| `basic` (default) | Nothing | Linked PR merged, referenced files deleted, PR conflicts, PR change already on main |
| `pro-ai` | A model on any OpenAI-compatible endpoint: your API key (OpenAI, Anthropic, OpenRouter, ...) or a local server (Ollama, llama.cpp) | `basic` + "does this issue still hold in the code?" (`still_valid` / `likely_fixed`, each citing `file:line`) |
| `free-ai` | — | Ran on GitHub Models, which GitHub [retired on 2026-07-30](https://github.blog/changelog/2026-07-30-github-models-is-now-retired/); now runs `basic` |

```sh
STILLVALID_API_KEY=sk-... stillvalid scan owner/repo --mode pro-ai --ai-model gpt-4.1-mini
stillvalid scan owner/repo --mode pro-ai --ai-base-url http://localhost:11434/v1 --ai-model qwen2.5-coder:7b   # Ollama, no key
```

`pro-ai` asks the model only about issues the heuristics leave at "can't tell", shows it the matching code at the scanned commit, and keeps an answer only if every line it cites is in that code; model verdicts are never `high` confidence (`likely_fixed` is always `low`). At most `--max-llm-calls` (default 200) calls per run, most-reacted issues first; on a rate limit it stops and the next run with `--previous` continues where it stopped. `--ai-debug` (or `STILLVALID_AI_DEBUG=1`) prints each raw model answer to stderr.

## Docs

- [docs/DESIGN.md](docs/DESIGN.md) — full design: architecture, verdicts, heuristics, outputs, cost
- [docs/DECISIONS.md](docs/DECISIONS.md) — decisions made so far and why
- [docs/ROADMAP.md](docs/ROADMAP.md) — MVP scope and task list
- [schema/report.example.json](schema/report.example.json) — the `report.json` contract

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this work, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
