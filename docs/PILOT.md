# Pilot: basic mode on lazygit

A hand check of `basic` mode verdicts on one public repo, to publish how often they are right.

| | |
|---|---|
| Date | 2026-09-28 |
| Repo | [jesseduffield/lazygit](https://github.com/jesseduffield/lazygit), branch `master` |
| Scanned commit | `cfbbf18656c2b246013f64b29eaed5a47e3085e3` |
| Tool | stillvalid 0.1.0 at `ae7feb4` (main after #22), `--mode basic`, release build |
| Command | `stillvalid scan jesseduffield/lazygit --mode basic --out lazygit.json --html lazygit-html` |
| Runtime | 2 min 24 s wall clock (cached clone; merge checks 8.7 s, code checks 23 s) |

lazygit's [CONTRIBUTING.md](https://github.com/jesseduffield/lazygit/blob/master/CONTRIBUTING.md) says the project does not accept pull requests but leaves them open for visibility. That shapes the PR results below.

## Counts

| Kind | Open | Verdicts |
|---|---|---|
| Issues | 864 | 6 `likely_fixed` (medium), 2 `needs_info` (low), 856 `cant_tell` |
| PRs | 194 | 74 `abandoned` (46 high, 28 medium), 32 `conflicts` (high), 0 `superseded`, 0 `ready_unreviewed`, 88 `cant_tell` |

114 items got a verdict other than `cant_tell`.

## Method

- **Sample (50):** every issue verdict (6 `likely_fixed`, 2 `needs_info`), 21 of 32 `conflicts`, 21 of 74 `abandoned` (13 high, 8 medium, in proportion). PRs were picked with `shuf --random-source` on a fixed seed file.
- **Checked against:** the issue or PR thread (`gh issue view`, `gh pr view`), GitHub's `mergeable` state, the latest commit and check status, and the cited commit or file in the clone at the scanned commit.
- **Grades:** *right* when the verdict and its evidence hold; *wrong* when they don't; *debatable* when the facts are true but a maintainer could reasonably disagree with the label. Debatable is not counted as right.
  - `conflicts` is right when GitHub reports the PR as `CONFLICTING`.
  - `abandoned` is right when the author went quiet past the threshold with the next step on their side (changes requested, author said they would follow up). It is debatable when the PR is waiting on the maintainer (author's last post answers questions or asks for review, approved but not merged, never reviewed), and wrong if the author is still active.
  - `likely_fixed` is right when the cited change actually fixes what the issue reports.
  - `needs_info` is right when the issue lacks what a maintainer needs to act on it.
- **Misses:** 10 random `cant_tell` issues, each read to see whether a person with the same data (thread, timeline, code) could have given a verdict.

## Precision

| Verdict | Checked | Right | Debatable | Wrong | Precision (right / checked) |
|---|---|---|---|---|---|
| `conflicts` | 21 | 21 | 0 | 0 | **100%** |
| `abandoned` (high) | 13 | 3 | 10 | 0 | 23% |
| `abandoned` (medium) | 8 | 1 | 7 | 0 | 13% |
| `likely_fixed` | 6 | 2 | 0 | 4 | 33% |
| `needs_info` | 2 | 2 | 0 | 0 | 100% |
| **All** | **50** | **29** | **17** | **4** | **58%** |

In short:

- **PR `conflicts` is reliable** (21 of 21).
- **PR `abandoned` is never wrong on the facts** (0 of 21 had an active author), but in 17 of 21 the PR is waiting on the maintainer, not the author. On a project that doesn't take PRs that is expected; the label still says "the author left", which is not what happened.
- **Issue `likely_fixed` is weak** (2 of 6). All four misses come from the code check (`src/check/code.rs`), none from linked PRs or commits.

## Per item

### Issues

| # | Verdict | Conf. | Grade | Reason |
|---|---|---|---|---|
| [5125](https://github.com/jesseduffield/lazygit/issues/5125) | likely_fixed | medium | wrong | Evidence is `SSH_AUTH_SOCK` gone from `vendor/…/go-git/…/ssh/common.go` (go-git dependency removed, 587a8bb). The error in the issue comes from git's own SSH commit signing during a rebase, which that change doesn't touch |
| [2211](https://github.com/jesseduffield/lazygit/issues/2211) | likely_fixed | medium | right | `StartTicking` no longer exists at the scanned commit; the goroutine leak was fixed in #2345 and the function later removed as unused. The cited commit (95c237f, gocui copied in) is not the one that removed it, but the conclusion holds |
| [2045](https://github.com/jesseduffield/lazygit/issues/2045) | likely_fixed | medium | wrong | Evidence is `editCommand` / `openCommand` removed from `docs/Config.md` (old config names). `CommitEditorCmdObj` (`pkg/commands/git_commands/commit.go:142`) still runs plain `git commit`, so git's editor is still used over lazygit's setting |
| [1746](https://github.com/jesseduffield/lazygit/issues/1746) | likely_fixed | medium | wrong | Evidence is "89% of `main.go` changed"; the file only appears in the crash trace. Nothing shows Termux's "terminal type unsupported" is handled |
| [1146](https://github.com/jesseduffield/lazygit/issues/1146) | likely_fixed | medium | wrong | Evidence is "95% of `pkg/i18n/english.go` changed"; the issue only links a UI string there. GPG signing still drops to a subprocess by design (`git.overrideGpg` is the workaround the maintainer named) |
| [818](https://github.com/jesseduffield/lazygit/issues/818) | likely_fixed | medium | right | ca31e52 "store popup version in state not config so that we never need to write to the user config" is exactly the fix for a read-only config file |
| [4476](https://github.com/jesseduffield/lazygit/issues/4476) | needs_info | low | right | Slowdown in an IDE terminal with no version, OS or repro; a maintainer asked for memory numbers and a public repro, no reply |
| [4048](https://github.com/jesseduffield/lazygit/issues/4048) | needs_info | low | right | One-line panic report; a maintainer asked to fill in the template, no reply |

### PRs: conflicts

All 21 are `CONFLICTING` on GitHub, so all are right; some rows add context from the thread.

| # | Verdict | Conf. | Grade | Reason |
|---|---|---|---|---|
| [5488](https://github.com/jesseduffield/lazygit/pull/5488) | conflicts | high | right | GitHub: CONFLICTING |
| [5338](https://github.com/jesseduffield/lazygit/pull/5338) | conflicts | high | right | GitHub: CONFLICTING |
| [5692](https://github.com/jesseduffield/lazygit/pull/5692) | conflicts | high | right | GitHub: CONFLICTING |
| [5636](https://github.com/jesseduffield/lazygit/pull/5636) | conflicts | high | right | GitHub: CONFLICTING (maintainer's own PR) |
| [5992](https://github.com/jesseduffield/lazygit/pull/5992) | conflicts | high | right | GitHub: CONFLICTING |
| [5798](https://github.com/jesseduffield/lazygit/pull/5798) | conflicts | high | right | GitHub: CONFLICTING (maintainer's own PR) |
| [5187](https://github.com/jesseduffield/lazygit/pull/5187) | conflicts | high | right | GitHub: CONFLICTING |
| [5732](https://github.com/jesseduffield/lazygit/pull/5732) | conflicts | high | right | GitHub: CONFLICTING; a maintainer's draft prototype, pushed to the day before, so the conflict is expected and not a problem |
| [5594](https://github.com/jesseduffield/lazygit/pull/5594) | conflicts | high | right | GitHub: CONFLICTING |
| [5611](https://github.com/jesseduffield/lazygit/pull/5611) | conflicts | high | right | GitHub: CONFLICTING |
| [5663](https://github.com/jesseduffield/lazygit/pull/5663) | conflicts | high | right | GitHub: CONFLICTING |
| [5499](https://github.com/jesseduffield/lazygit/pull/5499) | conflicts | high | right | GitHub: CONFLICTING |
| [5631](https://github.com/jesseduffield/lazygit/pull/5631) | conflicts | high | right | GitHub: CONFLICTING; the maintainer pointed to #5748, which covers it, so `superseded` would say more |
| [5468](https://github.com/jesseduffield/lazygit/pull/5468) | conflicts | high | right | GitHub: CONFLICTING |
| [5714](https://github.com/jesseduffield/lazygit/pull/5714) | conflicts | high | right | GitHub: CONFLICTING |
| [5598](https://github.com/jesseduffield/lazygit/pull/5598) | conflicts | high | right | GitHub: CONFLICTING |
| [5887](https://github.com/jesseduffield/lazygit/pull/5887) | conflicts | high | right | GitHub: CONFLICTING |
| [5475](https://github.com/jesseduffield/lazygit/pull/5475) | conflicts | high | right | GitHub: CONFLICTING (maintainer's own draft) |
| [5478](https://github.com/jesseduffield/lazygit/pull/5478) | conflicts | high | right | GitHub: CONFLICTING |
| [5293](https://github.com/jesseduffield/lazygit/pull/5293) | conflicts | high | right | GitHub: CONFLICTING |
| [6048](https://github.com/jesseduffield/lazygit/pull/6048) | conflicts | high | right | GitHub: CONFLICTING |

### PRs: abandoned

| # | Verdict | Conf. | Grade | Reason |
|---|---|---|---|---|
| [4629](https://github.com/jesseduffield/lazygit/pull/4629) | abandoned | high | right | Maintainer asked for a rework on 2025-07-09, author said they would; nothing since |
| [2709](https://github.com/jesseduffield/lazygit/pull/2709) | abandoned | high | debatable | Draft; the author's last post answers the maintainers' questions and nobody replied |
| [4536](https://github.com/jesseduffield/lazygit/pull/4536) | abandoned | high | debatable | Stalled on a design disagreement; last word is the author's |
| [2665](https://github.com/jesseduffield/lazygit/pull/2665) | abandoned | high | right | Changes requested on 2025-01-03, no response from the author |
| [4264](https://github.com/jesseduffield/lazygit/pull/4264) | abandoned | high | debatable | Author fixed the requested change and said "ready for merge"; no maintainer reply |
| [3775](https://github.com/jesseduffield/lazygit/pull/3775) | abandoned | high | debatable | Approved by a maintainer on 2024-08-17 and never merged |
| [4390](https://github.com/jesseduffield/lazygit/pull/4390) | abandoned | high | debatable | Never reviewed; the author's own notes list open problems |
| [4058](https://github.com/jesseduffield/lazygit/pull/4058) | abandoned | high | debatable | Never reviewed or commented on |
| [3881](https://github.com/jesseduffield/lazygit/pull/3881) | abandoned | high | debatable | Author made the requested changes and asked for review; no reply |
| [4447](https://github.com/jesseduffield/lazygit/pull/4447) | abandoned | high | debatable | Never reviewed or commented on |
| [4762](https://github.com/jesseduffield/lazygit/pull/4762) | abandoned | high | debatable | Never reviewed or commented on |
| [3383](https://github.com/jesseduffield/lazygit/pull/3383) | abandoned | high | right | Author said on 2024-07-10 they would add tests "this weekend"; last push 2024-07-14, nothing since |
| [4032](https://github.com/jesseduffield/lazygit/pull/4032) | abandoned | high | debatable | Never reviewed or commented on |
| [5076](https://github.com/jesseduffield/lazygit/pull/5076) | abandoned | medium | debatable | Maintainer is undecided on the behavior and asked others for opinions |
| [5333](https://github.com/jesseduffield/lazygit/pull/5333) | abandoned | medium | debatable | Never reviewed or commented on |
| [5297](https://github.com/jesseduffield/lazygit/pull/5297) | abandoned | medium | right | Maintainer disagreed with the approach; the author accepted ("your decision") and stopped |
| [5369](https://github.com/jesseduffield/lazygit/pull/5369) | abandoned | medium | debatable | Never reviewed or commented on |
| [5361](https://github.com/jesseduffield/lazygit/pull/5361) | abandoned | medium | debatable | Never reviewed or commented on |
| [5300](https://github.com/jesseduffield/lazygit/pull/5300) | abandoned | medium | debatable | No maintainer response |
| [5193](https://github.com/jesseduffield/lazygit/pull/5193) | abandoned | medium | debatable | Author proposed a compromise; no maintainer reply (checks also failing) |
| [5359](https://github.com/jesseduffield/lazygit/pull/5359) | abandoned | medium | debatable | Never reviewed or commented on |

## What was wrong and why

All four wrong verdicts are issue `likely_fixed` from the code check. Each one is a pattern, not a one-off:

1. **Vendored code counts as project code** (#5125). A name from the issue disappeared from `vendor/`, because a dependency was dropped. Changes under `vendor/` (and similar third-party trees) say nothing about whether the project's own behavior changed.
2. **Docs files count as code** (#2045). Config keys were renamed in `docs/Config.md`; the behavior the issue reports is unchanged in the Go code.
3. **"Most of the file changed" is not a fix signal for large central files** (#1746, #1146). `main.go` appears only in a crash trace and `english.go` only via a link to one UI string. Both files churn with ordinary development, so a high share of changed lines happens whether or not the issue was fixed.

These are listed for follow-up fixes; this pilot changes no code.

The `abandoned` debatables are a labeling question rather than a bug: the rule looks only at the author's silence, so a PR waiting on the maintainer (never reviewed, approved but not merged, or the author asked for review last) gets the same label as one the author walked away from.

Minor: for #2211 the cited commit is the one that copied gocui into the repo, not the one that removed `StartTicking`; the conclusion is still right.

## Misses (10 `cant_tell` issues)

| # | Could a person give a verdict from the same data? |
|---|---|
| [3070](https://github.com/jesseduffield/lazygit/issues/3070) | No. Discussion about using Crowdin; a maintainer says a Crowdin project exists, but the thread stays open on purpose |
| [3557](https://github.com/jesseduffield/lazygit/issues/3557) | No clear verdict. Hang with interactive hooks; nothing links a fix |
| [4863](https://github.com/jesseduffield/lazygit/issues/4863) | No. Feature request, still open for discussion |
| [4279](https://github.com/jesseduffield/lazygit/issues/4279) | No. Feature request; the maintainer gave a workaround (not a stillvalid verdict) |
| [5721](https://github.com/jesseduffield/lazygit/issues/5721) | No. Feature request; a config workaround exists |
| [1080](https://github.com/jesseduffield/lazygit/issues/1080) | No. Keybinding crash from 2020, not reproduced by maintainers |
| [5974](https://github.com/jesseduffield/lazygit/issues/5974) | No. A maintainer's call for testers, not a bug |
| [5724](https://github.com/jesseduffield/lazygit/issues/5724) | No. Referenced by commits on a fork branch, none on `master`; `cant_tell` is correct |
| [5516](https://github.com/jesseduffield/lazygit/issues/5516) | **Yes: miss.** The reporter confirmed on 2026-05-11 that PR #5596 (merged 2026-05-10) fixes it. The PR doesn't reference the issue; only a comment in the issue names the PR |
| [3108](https://github.com/jesseduffield/lazygit/issues/3108) | No. Feature request, still wanted |

1 of 10 is a miss: a fix confirmed in the issue's comments ("can you test #N?" / "it solves the problem") is not read as evidence. Most `cant_tell` issues on lazygit are feature requests and discussions, which `basic` mode doesn't try to judge.
