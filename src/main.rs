//! stillvalid — is that issue still valid?
//!
//! Scanning fetches open issues and PRs and writes report.json. Issues that a merged PR or a
//! commit on the branch says it fixes, or whose named code is gone, get `likely_fixed`; vague bug
//! reports get `needs_info`; open PRs get the activity rules (`abandoned`, `ready_unreviewed`) and a
//! local `git merge-tree` against the branch (`conflicts`, `superseded`); everything else is
//! `cant_tell`. See docs/DESIGN.md and docs/ROADMAP.md.

use anyhow::{Context, Result};
use chrono::{SubsecRound, Utc};
use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
use std::time::Instant;
use stillvalid::check::pulls::PullThresholds;
use stillvalid::check::{code, info, merge};
use stillvalid::{fetch, incremental, repo, report, store};

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
        #[arg(long, value_enum, default_value_t = Mode::FreeAi)]
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
    },
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum Mode {
    /// Heuristics only, no AI
    Basic,
    /// Heuristics + GitHub Models via GITHUB_TOKEN
    FreeAi,
    /// Heuristics + your own LLM provider
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
        } => {
            let (owner, name) = fetch::split_repo(&repo)?;
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
            let fetcher = fetch::Fetcher::new(gh.build()?);
            let snapshot = fetcher
                .fetch(&repo, branch.as_deref(), token.is_some())
                .await
                .with_context(|| format!("fetching {repo}"))?;

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
            store::write_report(&report, &out)?;
            eprintln!(
                "stillvalid: wrote {} ({} issues, {} PRs, {} issues referenced by PRs/commits; {} issues got a verdict: {} likely_fixed, {} needs_info; {} PRs abandoned, {} ready_unreviewed, {} conflicts, {} superseded)",
                out.display(),
                report.summary.issues.open,
                report.summary.pulls.open,
                snapshot.references.len(),
                report.summary.issues.open - report.summary.issues.cant_tell,
                report.summary.issues.likely_fixed,
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
            Ok(())
        }
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
