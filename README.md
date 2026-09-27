# stillvalid

**Is that issue still valid?** `stillvalid` checks every open GitHub issue and pull request against the current code and tells you which ones are already fixed, still real, duplicated, or dead — with evidence.

> **Status: pre-alpha / design stage.** The CLI skeleton compiles but does not scan yet. See [docs/ROADMAP.md](docs/ROADMAP.md).

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
curl -sSf https://…/install.sh | sh     # any Unix
```

```sh
stillvalid scan owner/repo --mode basic --html site
```

## Use in a repo (planned)

```yaml
# .github/workflows/stillvalid.yml
on:
  schedule: [{ cron: "0 3 * * *" }]
  workflow_dispatch:
permissions:
  issues: write          # labels + pinned summary (use `read` for dashboard only)
  pull-requests: read
  contents: write        # publish report to gh-pages
  models: read           # free-ai mode (GitHub Models)
jobs:
  scan:
    runs-on: ubuntu-latest
    steps:
      - uses: lodibrahim/stillvalid@v1
        with:
          mode: free-ai
          branch: main
```

## Modes

| Mode | Needs | What it can tell you |
|---|---|---|
| `basic` | Nothing | Linked PR merged, referenced files deleted, PR conflicts, PR change already on main |
| `free-ai` (default) | Nothing — GitHub Models via the built-in token | `basic` + "does this bug still exist in the code?" |
| `pro-ai` | Your own LLM API key | Same, more accurate, larger scale |

## Docs

- [docs/DESIGN.md](docs/DESIGN.md) — full design: architecture, verdicts, heuristics, outputs, cost
- [docs/DECISIONS.md](docs/DECISIONS.md) — decisions made so far and why
- [docs/ROADMAP.md](docs/ROADMAP.md) — MVP scope and task list
- [schema/report.example.json](schema/report.example.json) — the `report.json` contract

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this work, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
