//! stillvalid — is that issue still valid?
//!
//! Scanning fetches open issues and PRs and writes report.json. Issues that a merged PR or a
//! commit on the branch says it fixes get `likely_fixed`; everything else is `cant_tell`. See docs/DESIGN.md and docs/ROADMAP.md.

use anyhow::{Context, Result};
use chrono::{SubsecRound, Utc};
use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
use std::time::Instant;
use stillvalid::{fetch, repo, store};

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

        /// Also write the HTML dashboard here
        #[arg(long)]
        html: Option<PathBuf>,

        /// Previous report.json for incremental runs
        #[arg(long)]
        previous: Option<PathBuf>,

        /// Use this existing checkout instead of cloning (must contain the branch's head commit)
        #[arg(long)]
        repo_path: Option<PathBuf>,
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
        } => {
            let (owner, name) = fetch::split_repo(&repo)?;
            if html.is_some() || previous.is_some() {
                eprintln!("stillvalid: --html and --previous are not implemented yet; ignoring");
            }

            let mut gh = octocrab::Octocrab::builder();
            let token = github_token();
            match &token {
                Some(token) => gh = gh.personal_token(token.clone()),
                None => eprintln!(
                    "stillvalid: no GITHUB_TOKEN or `gh auth token`; using unauthenticated API (60 requests/hour) and skipping issue timelines"
                ),
            }
            let snapshot = fetch::Fetcher::new(gh.build()?)
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

            let mode = mode.to_possible_value().expect("no skipped variants");
            let report =
                store::build_report(&snapshot, mode.get_name(), Utc::now().trunc_subsecs(0));
            store::write_report(&report, &out)?;
            eprintln!(
                "stillvalid: wrote {} ({} issues, {} PRs, {} issues referenced by PRs/commits; {} issues got a verdict: {} likely_fixed)",
                out.display(),
                report.summary.issues.open,
                report.summary.pulls.open,
                snapshot.references.len(),
                report.summary.issues.open - report.summary.issues.cant_tell,
                report.summary.issues.likely_fixed,
            );
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
