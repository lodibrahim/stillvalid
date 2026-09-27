//! PR merge rules: `conflicts` (merging into the scanned branch conflicts) and `superseded`
//! (merging changes nothing). Unlike the other checks, these run git in the local repo.

use crate::check::pulls::{finding, short_sha, Finding};
use crate::fetch::{Pull, Snapshot};
use crate::repo::{Repo, RepoError};
use crate::store::{Confidence, Evidence, EvidenceType, Verdict};
use std::collections::{BTreeMap, BTreeSet};

/// At most this many conflicting files become evidence.
const MAX_CONFLICT_FILES: usize = 10;

/// Result of [`check_all`].
#[derive(Debug, Default)]
pub struct MergeChecks {
    /// Merge verdicts keyed by PR number.
    pub findings: BTreeMap<u64, Finding>,
    /// PRs that could not be checked (head not fetchable, unrelated history, ...).
    pub failed: Vec<(u64, RepoError)>,
}

/// Merge-check every open PR based on the scanned branch.
pub fn check_all(
    repo: &Repo,
    snapshot: &Snapshot,
    token: Option<&str>,
) -> Result<MergeChecks, RepoError> {
    let pulls: Vec<&Pull> = snapshot
        .pulls
        .iter()
        .filter(|p| p.base.name == snapshot.branch)
        .collect();
    // actions/checkout persists its own github.com auth header; a second one gets requests
    // rejected, so use the repo's when it has one.
    let own_header = [
        "config",
        "--get-all",
        "http.https://github.com/.extraheader",
    ];
    let token = token.filter(|_| repo.git(&own_header, None).is_err());
    let heads: Vec<&str> = pulls.iter().map(|p| p.head.sha.as_str()).collect();
    repo.fetch_commits(&heads, token);

    let head = &snapshot.head_sha;
    let git = Git {
        repo,
        token,
        head,
        head_tree: repo.git(&["rev-parse", &format!("{head}^{{tree}}")], token)?,
        branch: &snapshot.branch,
    };
    let mut checks = MergeChecks::default();
    for pull in pulls {
        match check(&git, &pull.head.sha) {
            Ok(Some(f)) => {
                checks.findings.insert(pull.number, f);
            }
            Ok(None) => {}
            Err(e) => checks.failed.push((pull.number, e)),
        }
    }
    Ok(checks)
}

/// The repo plus what every PR is merged into.
struct Git<'a> {
    repo: &'a Repo,
    token: Option<&'a str>,
    head: &'a str,
    head_tree: String,
    branch: &'a str,
}

impl Git<'_> {
    fn run(&self, args: &[&str]) -> Result<String, RepoError> {
        self.repo.git(args, self.token)
    }

    /// Files changed between two commits (trees only; no rename detection, which reads blobs).
    fn changed_files(&self, from: &str, to: &str) -> Result<BTreeSet<String>, RepoError> {
        let out = self.run(&["diff", "--name-only", "--no-renames", "-z", from, to])?;
        Ok(out
            .split('\0')
            .filter(|f| !f.is_empty())
            .map(str::to_string)
            .collect())
    }

    /// Merging `pr` into `commit` is clean and leaves `commit`'s tree unchanged.
    fn is_noop(&self, commit: &str, pr: &str) -> Result<bool, RepoError> {
        let merge = self.repo.merge_tree(commit, pr, self.token)?;
        Ok(merge.conflicts.is_empty()
            && merge.tree == self.run(&["rev-parse", &format!("{commit}^{{tree}}")])?)
    }

    /// Short SHA and committer date (`YYYY-MM-DD`) of `rev`.
    fn describe(&self, rev: &str) -> Result<String, RepoError> {
        let out = self.run(&["show", "-s", "--format=%H %cs", rev])?;
        Ok(describe_line(&out))
    }
}

/// `"<sha> <date>"` → `"<sha7> (<date>)"`.
fn describe_line(line: &str) -> String {
    let (sha, date) = line.split_once(' ').unwrap_or((line, ""));
    format!("{} ({date})", short_sha(sha))
}

/// Judge one PR head `pr` against the scanned head. `None` means no merge verdict.
fn check(git: &Git, pr: &str) -> Result<Option<Finding>, RepoError> {
    let (head, branch) = (git.head, git.branch);
    let base = git.run(&["merge-base", head, pr])?;
    if base == pr {
        let note = format!("PR head {} is already on {branch}", short_sha(pr));
        return Ok(Some(superseded(pr, note)));
    }
    let changed = git.changed_files(&base, pr)?;
    if changed.is_empty() {
        return Ok(None);
    }

    let merge = git.repo.merge_tree(head, pr, git.token)?;
    if !merge.conflicts.is_empty() {
        return conflicts(git, &base, &changed, &merge.conflicts).map(Some);
    }
    if merge.tree != git.head_tree {
        return Ok(None);
    }

    // Find the first commit on the branch's first-parent line where merging became a no-op.
    let line = git.run(&[
        "rev-list",
        "--first-parent",
        "--reverse",
        &format!("{base}..{head}"),
    ])?;
    let commits: Vec<&str> = line.lines().collect();
    let (mut lo, mut hi) = (0, commits.len().saturating_sub(1));
    while lo < hi {
        let mid = (lo + hi) / 2;
        if git.is_noop(commits[mid], pr)? {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    let landed = commits.get(lo).copied().unwrap_or(head);
    let note = format!(
        "Merging this PR into {branch} changes nothing; its change landed in {}",
        git.describe(landed)?
    );
    Ok(Some(superseded(landed, note)))
}

fn superseded(sha: &str, note: String) -> Finding {
    finding(Verdict::Superseded, Confidence::High, sha, note)
}

fn conflicts(
    git: &Git,
    base: &str,
    changed: &BTreeSet<String>,
    files: &[String],
) -> Result<Finding, RepoError> {
    let (head, branch) = (git.head, git.branch);
    let on_branch = git.changed_files(base, head)?;
    let n = changed.len();
    let both = changed.intersection(&on_branch).count();
    let summary = format!(
        "{} of {n} changed files conflict ({both} of {n} changed on {branch} since the PR branched). ",
        files.len()
    );
    let range = format!("{base}..{head}");
    let mut evidence = Vec::new();
    for (i, file) in files.iter().take(MAX_CONFLICT_FILES).enumerate() {
        let last = git.run(&[
            "log",
            "-1",
            "--format=%H %cs",
            &range,
            "--",
            &format!(":(literal){file}"),
        ])?;
        let lead = if i == 0 { summary.as_str() } else { "" };
        let mut note = format!("{lead}Conflicts with {branch}");
        if !last.is_empty() {
            note += &format!("; last changed there in {}", describe_line(&last));
        }
        evidence.push(Evidence {
            kind: EvidenceType::Code,
            reference: file.clone(),
            note,
        });
    }
    Ok(Finding {
        verdict: Verdict::Conflicts,
        confidence: Confidence::High,
        evidence,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fetch::{PullBase, PullHead};
    use std::path::Path;
    use std::process::Command;
    use tempfile::TempDir;

    /// Run git with a fixed identity and no user/system config; panics on failure.
    fn sh(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            )
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_DATE", "2026-05-01T00:00:00Z")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    /// A bare "remote" that serves PR heads by SHA, plus a working repo that pushes to it.
    struct Remote {
        tmp: TempDir,
    }

    impl Remote {
        fn new() -> Self {
            let tmp = TempDir::new().unwrap();
            let bare = tmp.path().join("remote.git");
            let work = tmp.path().join("work");
            std::fs::create_dir_all(&bare).unwrap();
            std::fs::create_dir_all(&work).unwrap();
            sh(&bare, &["init", "--quiet", "--bare", "-b", "main"]);
            sh(&bare, &["config", "uploadpack.allowFilter", "true"]);
            sh(&bare, &["config", "uploadpack.allowAnySHA1InWant", "true"]);
            sh(&work, &["init", "--quiet", "-b", "main"]);
            let path = bare.to_string_lossy().replace('\\', "/");
            let url = format!(
                "file://{}{path}",
                if path.starts_with('/') { "" } else { "/" }
            );
            sh(&work, &["remote", "add", "origin", &url]);
            let remote = Self { tmp };
            remote.commit(&[("a.txt", "a1\na2\na3\n"), ("b.txt", "b1\n")], "init");
            remote
        }

        fn work(&self) -> std::path::PathBuf {
            self.tmp.path().join("work")
        }

        fn checkout(&self, rev: &str) {
            sh(&self.work(), &["checkout", "--quiet", "-B", "cur", rev]);
        }

        /// Commit `files` on the current branch; returns the new SHA.
        fn commit(&self, files: &[(&str, &str)], msg: &str) -> String {
            for (name, text) in files {
                std::fs::write(self.work().join(name), text).unwrap();
            }
            sh(&self.work(), &["add", "."]);
            sh(&self.work(), &["commit", "--quiet", "-m", msg]);
            sh(&self.work(), &["rev-parse", "HEAD"])
        }

        /// Push the current commit as `main`, or as PR `n`'s head.
        fn push(&self, n: Option<u64>) {
            let dst = match n {
                Some(n) => format!("HEAD:refs/pull/{n}/head"),
                None => "HEAD:refs/heads/main".into(),
            };
            sh(
                &self.work(),
                &["push", "--quiet", "--force", "origin", &dst],
            );
        }

        /// Blobless clone at the pushed `main`, as a scan would use.
        fn repo(&self) -> Repo {
            let url = sh(&self.work(), &["remote", "get-url", "origin"]);
            let head = sh(&self.work(), &["rev-parse", "refs/remotes/origin/main"]);
            let dir = self.tmp.path().join("cache");
            Repo::clone_or_update(&url, &dir, "main", &head, None).unwrap()
        }
    }

    fn snapshot(repo: &Repo, pulls: &[(u64, &str, &str)]) -> Snapshot {
        Snapshot {
            repo: "acme/rocketdb".into(),
            branch: "main".into(),
            head_sha: repo.head_sha.clone(),
            issues: vec![],
            pulls: pulls
                .iter()
                .map(|&(number, sha, base)| Pull {
                    number,
                    title: "PR".into(),
                    html_url: String::new(),
                    created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
                    head: PullHead { sha: sha.into() },
                    base: PullBase { name: base.into() },
                })
                .collect(),
            references: Default::default(),
            reopened_at: Default::default(),
            commits_on_branch: Default::default(),
            pull_activity: Default::default(),
        }
    }

    fn run(remote: &Remote, pulls: &[(u64, &str, &str)]) -> BTreeMap<u64, Finding> {
        let repo = remote.repo();
        let checks = check_all(&repo, &snapshot(&repo, pulls), None).unwrap();
        assert!(checks.failed.is_empty(), "{:?}", checks.failed);
        checks.findings
    }

    #[test]
    fn clean_pr_has_no_merge_verdict() {
        let r = Remote::new();
        let base = sh(&r.work(), &["rev-parse", "HEAD"]);
        r.commit(&[("a.txt", "a1\na2\nmain\n")], "main change");
        r.push(None);
        r.checkout(&base);
        let pr = r.commit(&[("b.txt", "pr\n")], "pr change");
        r.push(Some(1));
        assert!(run(&r, &[(1, &pr, "main")]).is_empty());
    }

    #[test]
    fn conflicting_pr_lists_files_and_last_branch_commit() {
        let r = Remote::new();
        let base = sh(&r.work(), &["rev-parse", "HEAD"]);
        let main = r.commit(&[("a.txt", "main\na2\na3\n"), ("c.txt", "c\n")], "main");
        r.push(None);
        r.checkout(&base);
        let pr = r.commit(&[("a.txt", "pr\na2\na3\n"), ("b.txt", "pr\n")], "pr");
        r.push(Some(2));

        let f = &run(&r, &[(2, &pr, "main")])[&2];
        assert_eq!(f.verdict, Verdict::Conflicts);
        assert_eq!(f.confidence, Confidence::High);
        assert_eq!(
            f.evidence,
            [Evidence {
                kind: EvidenceType::Code,
                reference: "a.txt".into(),
                note: format!(
                    "1 of 2 changed files conflict (1 of 2 changed on main since the PR branched). \
                     Conflicts with main; last changed there in {} (2026-05-01)",
                    &main[..7]
                ),
            }]
        );
    }

    #[test]
    fn superseded_pr_points_at_the_commit_that_landed_it() {
        let r = Remote::new();
        let base = sh(&r.work(), &["rev-parse", "HEAD"]);
        r.commit(&[("b.txt", "b2\n")], "unrelated");
        // Main makes the PR's change together with another one.
        let landed = r.commit(&[("a.txt", "a1\nfixed\na3\n"), ("c.txt", "c\n")], "fix");
        r.commit(&[("c.txt", "c2\n")], "later");
        r.commit(&[("b.txt", "b3\n")], "later again");
        r.push(None);
        r.checkout(&base);
        let pr = r.commit(&[("a.txt", "a1\nfixed\na3\n")], "pr fix");
        r.push(Some(3));

        let f = &run(&r, &[(3, &pr, "main")])[&3];
        assert_eq!(f.verdict, Verdict::Superseded);
        assert_eq!(f.confidence, Confidence::High);
        assert_eq!(f.evidence[0].kind, EvidenceType::Commit);
        assert_eq!(f.evidence[0].reference, &landed[..7]);
        assert_eq!(
            f.evidence[0].note,
            format!(
                "Merging this PR into main changes nothing; its change landed in {} (2026-05-01)",
                &landed[..7]
            )
        );
    }

    #[test]
    fn pr_head_already_on_branch_is_superseded() {
        let r = Remote::new();
        let pr = r.commit(&[("b.txt", "pr\n")], "pr");
        r.push(Some(4));
        r.commit(&[("c.txt", "c\n")], "after");
        r.push(None);

        let f = &run(&r, &[(4, &pr, "main")])[&4];
        assert_eq!(f.verdict, Verdict::Superseded);
        assert_eq!(f.evidence[0].reference, &pr[..7]);
        assert_eq!(
            f.evidence[0].note,
            format!("PR head {} is already on main", &pr[..7])
        );
    }

    #[test]
    fn other_bases_are_skipped_and_missing_heads_fail_alone() {
        let r = Remote::new();
        let base = sh(&r.work(), &["rev-parse", "HEAD"]);
        r.commit(&[("a.txt", "main\na2\na3\n")], "main");
        r.push(None);
        r.checkout(&base);
        let pr = r.commit(&[("a.txt", "pr\na2\na3\n")], "pr");
        r.push(Some(5));

        let repo = r.repo();
        let missing = "0123456789abcdef0123456789abcdef01234567";
        let snap = snapshot(
            &repo,
            &[(5, &pr, "main"), (6, &pr, "release"), (7, missing, "main")],
        );
        let MergeChecks { findings, failed } = check_all(&repo, &snap, None).unwrap();
        assert_eq!(findings.keys().copied().collect::<Vec<_>>(), [5]);
        assert_eq!(findings[&5].verdict, Verdict::Conflicts);
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].0, 7);
    }
}
