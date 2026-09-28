//! stillvalid — is that issue still valid?
//!
//! Scanning fetches open issues and PRs and writes report.json. Issues that a merged PR or a
//! commit on the branch says it fixes, or whose named code is gone, get `likely_fixed`; vague bug
//! reports get `needs_info`; open PRs get the activity rules (`abandoned`, `ready_unreviewed`) and a
//! local `git merge-tree` against the branch (`conflicts`, `superseded`); everything else is
//! `cant_tell`. With `--mode pro-ai`, a model then checks the remaining issues against retrieved
//! code (see `check::ai`). See docs/DESIGN.md and docs/ROADMAP.md.

use anyhow::{Context, Result};
use chrono::{SubsecRound, Utc};
use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
use std::time::Instant;
use stillvalid::check::pulls::PullThresholds;
use stillvalid::check::{ai, code, info, merge};
use stillvalid::report::github;
use stillvalid::{fetch, incremental, llm, repo, report, store};

#[derive(Parser)]
#[command(
    name = "stillvalid",
    version,
    about = "Check whether open GitHub issues and PRs are still valid against the current code"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scan a repository's open issues and pull requests
    Scan {
        /// Repository as owner/repo
        repo: String,

        /// How much checking to do
        #[arg(long, value_enum, default_value_t = Mode::Basic)]
        mode: Mode,

        /// Branch where development happens (defaults to the repo's default branch)
        #[arg(long)]
        branch: Option<String>,

        /// Write report.json here
        #[arg(long, default_value = "report.json")]
        out: PathBuf,

        /// Also write the dashboard (index.html) and shields.io badge (badge.json) into this directory
        #[arg(long)]
        html: Option<PathBuf>,

        /// Previous report.json for incremental runs
        #[arg(long)]
        previous: Option<PathBuf>,

        /// Use this existing checkout instead of cloning (must contain the branch's head commit)
        #[arg(long)]
        repo_path: Option<PathBuf>,

        /// Days without author activity before a failing or conflicting PR is `abandoned`
        #[arg(long, default_value_t = PullThresholds::default().abandoned_after_days)]
        abandoned_after_days: u32,

        /// Days open without a review before a green PR is `ready_unreviewed`
        #[arg(long, default_value_t = PullThresholds::default().unreviewed_after_days)]
        unreviewed_after_days: u32,

        /// Put a `stillvalid: <verdict>` label on each issue and PR (needs issues: write)
        #[arg(long)]
        labels: bool,

        /// Create or update the "Backlog health" summary issue (needs issues: write)
        #[arg(long)]
        summary_issue: bool,

        /// Dashboard URL to link from the summary issue
        #[arg(long)]
        dashboard_url: Option<String>,

        /// With --labels / --summary-issue: print what would change on GitHub, write nothing
        #[arg(long)]
        dry_run: bool,

        /// pro-ai: OpenAI-compatible API root (key from STILLVALID_API_KEY, if the server needs one)
        #[arg(long, default_value = "https://api.openai.com/v1")]
        ai_base_url: String,

        /// pro-ai: model to ask, e.g. gpt-4.1-mini
        #[arg(long)]
        ai_model: Option<String>,

        /// pro-ai: most model calls per run; the next run continues where this one stopped
        #[arg(long, default_value_t = 200)]
        max_llm_calls: usize,

        /// pro-ai: print each raw model answer to stderr (it includes the issue's code)
        #[arg(long, env = "STILLVALID_AI_DEBUG", value_parser = clap::builder::FalseyValueParser::new())]
        ai_debug: bool,
    },
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum Mode {
    /// Heuristics only, no AI
    Basic,
    /// Was GitHub Models, which GitHub retired on 2026-07-30; runs basic
    FreeAi,
    /// Heuristics + a model on any OpenAI-compatible endpoint (your key, or a local server)
    ProAi,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Scan {
            repo,
            mode,
            branch,
            out,
            html,
            previous,
            repo_path,
            abandoned_after_days,
            unreviewed_after_days,
            labels,
            summary_issue,
            dashboard_url,
            dry_run,
            ai_base_url,
            ai_model,
            max_llm_calls,
            ai_debug,
        } => {
            let (owner, name) = fetch::split_repo(&repo)?;
            let (mode, client) = match mode {
                Mode::Basic => (mode, None),
                Mode::FreeAi => {
                    eprintln!(
                        "stillvalid: GitHub Models was retired on 2026-07-30, so --mode free-ai has no model to call; running basic (use --mode pro-ai with your own key or a local model)"
                    );
                    (Mode::Basic, None)
                }
                Mode::ProAi => {
                    let model = ai_model.context("--mode pro-ai needs --ai-model")?;
                    let key = std::env::var("STILLVALID_API_KEY")
                        .ok()
                        .map(|k| k.trim().to_string())
                        .filter(|k| !k.is_empty());
                    (mode, Some(llm::Client::new(&ai_base_url, key, &model)?))
                }
            };
            // Read it before the long fetch so a bad file fails fast; a missing one means a first run.
            let previous = match previous {
                Some(path) if !path.exists() => {
                    eprintln!(
                        "stillvalid: no previous report at {}; checking everything",
                        path.display()
                    );
                    None
                }
                Some(path) => Some(store::read_report(&path)?),
                None => None,
            };

            let mut gh = octocrab::Octocrab::builder();
            let token = github_token();
            match &token {
                Some(token) => gh = gh.personal_token(token.clone()),
                None => eprintln!(
                    "stillvalid: no GITHUB_TOKEN or `gh auth token`; using unauthenticated API (60 requests/hour) and skipping issue timelines and PR activity"
                ),
            }
            let gh = gh.build()?;
            let fetcher = fetch::Fetcher::new(gh.clone());
            let mut snapshot = fetcher
                .fetch(&repo, branch.as_deref(), token.is_some())
                .await
                .with_context(|| format!("fetching {repo}"))?;
            github::drop_summary(&mut snapshot);

            let started = Instant::now();
            let local = match &repo_path {
                Some(dir) => repo::Repo::open_existing(dir, &snapshot.head_sha),
                None => repo::Repo::clone_or_update(
                    &format!("https://github.com/{owner}/{name}.git"),
                    &repo::cache_path(owner, name)?,
                    &snapshot.branch,
                    &snapshot.head_sha,
                    token.as_deref(),
                ),
            }
            .with_context(|| format!("preparing a local copy of {repo}"))?;
            eprintln!(
                "stillvalid: {} at {} ({:.1}s)",
                local.path.display(),
                local.head_sha,
                started.elapsed().as_secs_f64(),
            );

            let started = Instant::now();
            let checks =
                merge::check_all(&local, &snapshot, token.as_deref()).unwrap_or_else(|e| {
                    eprintln!("stillvalid: skipping merge checks: {e}");
                    merge::MergeChecks::default()
                });
            eprintln!(
                "stillvalid: merge-checked PRs against {} ({:.1}s)",
                snapshot.branch,
                started.elapsed().as_secs_f64(),
            );
            if let Some(err) = &checks.prefetch_failed {
                eprintln!("stillvalid: could not batch-fetch blobs for merge checks, fetching them one by one: {err}");
            }
            if let Some((number, err)) = checks.failed.first() {
                eprintln!(
                    "stillvalid: could not merge-check {} PRs (first: #{number}: {err})",
                    checks.failed.len()
                );
            }

            let started = Instant::now();
            let code = code::check_all(&local, &snapshot, token.as_deref());
            eprintln!(
                "stillvalid: checked the code issues name ({:.1}s)",
                started.elapsed().as_secs_f64(),
            );
            if let Some(err) = &code.prefetch_failed {
                eprintln!("stillvalid: could not batch-fetch blobs for code checks, fetching them one by one: {err}");
            }
            if let Some((number, err)) = code.failed.first() {
                eprintln!(
                    "stillvalid: could not code-check {} issues (first: #{number}: {err})",
                    code.failed.len()
                );
            }

            let now = Utc::now().trunc_subsecs(0);
            // Needs fetched references, which need a token.
            let info = match token {
                Some(_) => info::check_all(&fetcher, &snapshot, &code.findings, now).await,
                None => info::InfoChecks::default(),
            };
            if let Some((number, err)) = info.failed.first() {
                eprintln!(
                    "stillvalid: could not read comments on {} issues (first: #{number}: {err})",
                    info.failed.len()
                );
            }

            let mode = mode.to_possible_value().expect("no skipped variants");
            let thresholds = PullThresholds {
                abandoned_after_days,
                unreviewed_after_days,
            };
            let mut report = store::build_report(
                &snapshot,
                mode.get_name(),
                now,
                &thresholds,
                &checks.findings,
                &code.findings,
                &info.findings,
            );
            let blobs = local
                .blob_shas()
                .with_context(|| format!("listing files at {}", local.head_sha))?;
            incremental::fill_fingerprints(&mut report, &snapshot, &blobs);
            let mut ai = client.map(|client| {
                let started = Instant::now();
                let token = local.fetch_token(token.as_deref());
                let prepared = ai::prepare(&mut report, &snapshot, &local, &blobs, token);
                eprintln!(
                    "stillvalid: found code for {} issues to ask the model about ({:.1}s)",
                    prepared.excerpts.len(),
                    started.elapsed().as_secs_f64(),
                );
                if let Some((number, err)) = prepared.failed.first() {
                    eprintln!(
                        "stillvalid: could not read code for {} issues (first: #{number}: {err})",
                        prepared.failed.len()
                    );
                }
                (client, prepared)
            });
            if let Some(previous) = previous {
                match incremental::check_previous(&previous, &report) {
                    Ok(()) => {
                        let counts = incremental::reuse(&mut report, &previous);
                        eprintln!(
                            "stillvalid: incremental: {} reused, {} re-checked ({} new)",
                            counts.reused, counts.rechecked, counts.new
                        );
                    }
                    Err(e @ incremental::Unusable::Repo { .. }) => return Err(e.into()),
                    Err(e) => eprintln!("stillvalid: {e}; checking everything"),
                }
            }
            if let Some((client, prepared)) = &mut ai {
                let started = Instant::now();
                let run = ai::run(
                    &mut report,
                    &snapshot,
                    prepared,
                    client,
                    max_llm_calls,
                    ai_debug,
                )
                .await;
                eprintln!(
                    "stillvalid: model: {} calls ({} likely_fixed, {} still_valid, {} cant_tell), {} issues left for the next run ({:.1}s)",
                    run.calls,
                    run.likely_fixed,
                    run.still_valid,
                    run.cant_tell,
                    run.left,
                    started.elapsed().as_secs_f64(),
                );
                if let Some(reason) = &run.stopped {
                    eprintln!("stillvalid: stopped model calls: {reason}");
                }
                if let Some((number, err)) = run.failed.first() {
                    eprintln!(
                        "stillvalid: {} model calls failed (first: #{number}: {err})",
                        run.failed.len()
                    );
                }
            }
            store::write_report(&report, &out)?;
            eprintln!(
                "stillvalid: wrote {} ({} issues, {} PRs, {} issues referenced by PRs/commits; {} issues got a verdict: {} likely_fixed, {} still_valid, {} needs_info; {} PRs abandoned, {} ready_unreviewed, {} conflicts, {} superseded)",
                out.display(),
                report.summary.issues.open,
                report.summary.pulls.open,
                snapshot.references.len(),
                report.summary.issues.open - report.summary.issues.cant_tell,
                report.summary.issues.likely_fixed,
                report.summary.issues.still_valid,
                report.summary.issues.needs_info,
                report.summary.pulls.abandoned,
                report.summary.pulls.ready_unreviewed,
                report.summary.pulls.conflicts,
                report.summary.pulls.superseded,
            );
            if let Some(dir) = &html {
                report::write_site(&report, dir)
                    .with_context(|| format!("writing the dashboard to {}", dir.display()))?;
                eprintln!(
                    "stillvalid: wrote {} and badge.json",
                    dir.join("index.html").display()
                );
            }
            if labels || summary_issue {
                if token.is_none() {
                    eprintln!("stillvalid: --labels / --summary-issue need a token with issues: write; skipping");
                    return Ok(());
                }
                let writer = github::Writer::new(gh, &report.repo);
                if labels {
                    if let Err(e) = apply_labels(&writer, &report, &snapshot, dry_run).await {
                        eprintln!("stillvalid: skipping labels: {e}");
                    }
                }
                if summary_issue {
                    let url = dashboard_url.filter(|u| {
                        let ok = u.starts_with("https://");
                        if !ok {
                            eprintln!(
                                "stillvalid: ignoring --dashboard-url {u}: not an https:// URL"
                            );
                        }
                        ok
                    });
                    let body = github::summary_body(&report, url.as_deref());
                    match writer.sync_summary(&snapshot.issues, &body, dry_run).await {
                        Ok(outcome) => {
                            print_summary(&outcome);
                            if matches!(
                                outcome,
                                github::SummaryOutcome::WouldCreate
                                    | github::SummaryOutcome::WouldUpdate(_)
                            ) {
                                eprintln!("{body}");
                            }
                        }
                        Err(e) => eprintln!("stillvalid: skipping the summary issue: {e}"),
                    }
                }
            }
            Ok(())
        }
    }
}

/// Bring each item's `stillvalid:` label in line with its verdict (or print the changes).
async fn apply_labels(
    writer: &github::Writer,
    report: &store::Report,
    snapshot: &fetch::Snapshot,
    dry_run: bool,
) -> Result<(), github::GithubError> {
    let changes = github::plan_labels(report, snapshot);
    if changes.is_empty() {
        eprintln!("stillvalid: labels already up to date");
        return Ok(());
    }
    let missing = match changes.iter().any(|c| c.add.is_some()) {
        true => github::missing_labels(&changes, &writer.repo_labels().await?),
        false => Vec::new(),
    };
    if dry_run {
        for v in &missing {
            let name = github::label_name(*v).expect("only labeled verdicts are missing");
            eprintln!("stillvalid: would create label `{name}`");
        }
        for change in &changes {
            eprintln!("stillvalid: would {}", change.describe());
        }
        return Ok(());
    }
    let done = writer.apply_labels(&changes, &missing).await?;
    eprintln!(
        "stillvalid: labels: created {} labels, updated {} items",
        done.created, done.changed
    );
    if let Some((number, err)) = done.failed.first() {
        eprintln!(
            "stillvalid: could not label {} items (first: #{number}: {err})",
            done.failed.len()
        );
    }
    Ok(())
}

fn print_summary(outcome: &github::SummaryOutcome) {
    use github::SummaryOutcome::*;
    match outcome {
        Unchanged(n) => eprintln!("stillvalid: summary issue #{n} already up to date"),
        Updated(n) => eprintln!("stillvalid: updated summary issue #{n}"),
        Created { number, pinned } => {
            eprintln!("stillvalid: created summary issue #{number}");
            if let Err(e) = pinned {
                eprintln!("stillvalid: could not pin #{number}: {e}");
            }
        }
        Closed(n) => eprintln!(
            "stillvalid: summary issue #{n} was closed by a maintainer; leaving it closed"
        ),
        WouldUpdate(n) => eprintln!("stillvalid: would update summary issue #{n}"),
        WouldCreate => eprintln!("stillvalid: would create the summary issue"),
    }
}

/// `GITHUB_TOKEN` if set, otherwise the GitHub CLI's token, otherwise none.
fn github_token() -> Option<String> {
    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        if !token.trim().is_empty() {
            return Some(token.trim().to_string());
        }
    }
    let out = std::process::Command::new("gh")
        .args(["auth", "token"])
        .output()
        .ok()?;
    let token = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (out.status.success() && !token.is_empty()).then_some(token)
}
