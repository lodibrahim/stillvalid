//! Local copy of the scanned repository at the scanned commit: a cached blobless clone of the
//! dev branch, or an existing checkout the user points at. Uses the system `git` (2.38+).

use base64::Engine;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

const MIN_GIT: (u32, u32) = (2, 38);

#[derive(Debug, thiserror::Error)]
pub enum RepoError {
    #[error("could not run `git` (is it installed and on PATH?): {0}")]
    GitMissing(std::io::Error),
    #[error("{0} is too old; stillvalid needs git 2.38 or newer")]
    GitTooOld(String),
    #[error("`git {args}` failed: {stderr}")]
    Git { args: String, stderr: String },
    #[error(
        "branch `{branch}` moved: GitHub said {expected}, remote has {found}; re-run the scan"
    )]
    BranchMoved {
        branch: String,
        expected: String,
        found: String,
    },
    #[error(
        "commit {sha} is not in {path}; fetch it first (with actions/checkout, set fetch-depth: 0)"
    )]
    MissingCommit { sha: String, path: PathBuf },
    #[error("no cache directory: set HOME (or LOCALAPPDATA on Windows)")]
    NoCacheDir,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// A local git repository containing `head_sha`. Later checks read files from git objects at
/// `head_sha`, not from the working tree.
#[derive(Debug)]
pub struct Repo {
    pub path: PathBuf,
    pub head_sha: String,
}

impl Repo {
    /// Clone `remote` into `dir` (blobless, only `branch`), or fetch `branch` if `dir` already
    /// holds a clone, then check out `head_sha` detached. `token` is sent as an HTTP header to
    /// github.com only; it is never written to disk.
    pub fn clone_or_update(
        remote: &str,
        dir: &Path,
        branch: &str,
        head_sha: &str,
        token: Option<&str>,
    ) -> Result<Self, RepoError> {
        check_git_version()?;
        if dir.join(".git").exists() {
            let refspec = format!("+refs/heads/{branch}:refs/remotes/origin/{branch}");
            git(Some(dir), &["fetch", "--quiet", "origin", &refspec], token)?;
        } else {
            if let Some(parent) = dir.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let dir = dir.to_string_lossy();
            git(
                None,
                &[
                    "clone",
                    "--quiet",
                    "--filter=blob:none",
                    "--single-branch",
                    "--no-checkout",
                    "--branch",
                    branch,
                    remote,
                    &dir,
                ],
                token,
            )?;
        }

        // The branch may have moved since the API call; the older commit is still fine to scan.
        let found = git(
            Some(dir),
            &["rev-parse", &format!("refs/remotes/origin/{branch}")],
            token,
        )?;
        if found != head_sha && !has_commit(dir, head_sha, token) {
            return Err(RepoError::BranchMoved {
                branch: branch.to_string(),
                expected: head_sha.to_string(),
                found,
            });
        }
        git(
            Some(dir),
            &["checkout", "--quiet", "--force", "--detach", head_sha],
            token,
        )?;
        Ok(Self {
            path: dir.to_path_buf(),
            head_sha: head_sha.to_string(),
        })
    }

    /// Use an existing checkout as is (no fetch, no checkout); it only has to contain `head_sha`.
    pub fn open_existing(dir: &Path, head_sha: &str) -> Result<Self, RepoError> {
        check_git_version()?;
        if !has_commit(dir, head_sha, None) {
            return Err(RepoError::MissingCommit {
                sha: head_sha.to_string(),
                path: dir.to_path_buf(),
            });
        }
        Ok(Self {
            path: dir.to_path_buf(),
            head_sha: head_sha.to_string(),
        })
    }

    /// Every file at `head_sha` and its blob SHA. Reads trees only, so a blobless clone
    /// fetches nothing.
    pub fn blob_shas(&self) -> Result<HashMap<String, String>, RepoError> {
        let out = git(
            Some(&self.path),
            &["ls-tree", "-r", "-z", "--full-tree", &self.head_sha],
            None,
        )?;
        // Each entry: `<mode> <type> <sha>\t<path>`.
        Ok(out
            .split('\0')
            .filter_map(|entry| {
                let (meta, path) = entry.split_once('\t')?;
                let mut meta = meta.split(' ').skip(1);
                if meta.next()? != "blob" {
                    return None;
                }
                Some((path.to_string(), meta.next()?.to_string()))
            })
            .collect())
    }

    /// Run git in this repo and return trimmed stdout.
    pub(crate) fn git(&self, args: &[&str], token: Option<&str>) -> Result<String, RepoError> {
        git(Some(&self.path), args, token)
    }

    /// Fetch commits by SHA from `origin`: objects only, no refs or FETCH_HEAD. One request for
    /// all SHAs; if that fails (a SHA gone after a force-push), one request per SHA, skipping
    /// failures. Commits that are still missing make later git commands fail.
    pub fn fetch_commits(&self, shas: &[&str], token: Option<&str>) {
        if shas.is_empty() {
            return;
        }
        let fetch = |shas: &[&str]| {
            let mut args = vec!["fetch", "--quiet", "--no-write-fetch-head", "origin"];
            args.extend(shas);
            self.git(&args, token)
        };
        if fetch(shas).is_err() {
            for &sha in shas {
                let _ = fetch(&[sha]);
            }
        }
    }

    /// `git merge-tree --write-tree ours theirs`; writes the result tree to the object store only.
    pub fn merge_tree(
        &self,
        ours: &str,
        theirs: &str,
        token: Option<&str>,
    ) -> Result<MergeTree, RepoError> {
        let args = [
            "merge-tree",
            "--write-tree",
            "--name-only",
            "--no-messages",
            "-z",
            ours,
            theirs,
        ];
        // Exit 1 means conflicts.
        let stdout = git_output(Some(&self.path), &args, token, &[0, 1])?;
        let mut fields = stdout.split('\0').filter(|f| !f.is_empty());
        Ok(MergeTree {
            tree: fields.next().unwrap_or_default().to_string(),
            conflicts: fields.map(str::to_string).collect(),
        })
    }

    /// The last first-parent commit before `head_sha` committed at or before `at`.
    pub fn commit_before(&self, at: DateTime<Utc>) -> Result<Option<String>, RepoError> {
        let before = format!("--before={}", at.to_rfc3339());
        let out = self.git(
            &["rev-list", "-1", "--first-parent", &before, &self.head_sha],
            None,
        )?;
        Ok((!out.is_empty()).then_some(out))
    }

    /// Every file path at `rev`. Reads trees only.
    pub fn files_at(&self, rev: &str) -> Result<Vec<String>, RepoError> {
        let out = self.git(
            &["ls-tree", "-r", "-z", "--name-only", "--full-tree", rev],
            None,
        )?;
        Ok(out
            .split('\0')
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect())
    }

    /// Whether `path` is a file at `rev`. Reads trees only, so a blobless clone fetches nothing.
    pub fn is_file(&self, rev: &str, path: &str) -> bool {
        // `<mode> <type> <sha>\t<path>`, or nothing.
        self.git(&["ls-tree", "--full-tree", rev, "--", path], None)
            .is_ok_and(|out| out.split(' ').nth(1) == Some("blob"))
    }

    /// The last first-parent commit in `from..head_sha` that removed `path`, and where it moved
    /// the file if git's rename detection saw a move.
    pub fn removal(
        &self,
        from: &str,
        path: &str,
    ) -> Result<Option<(String, Option<String>)>, RepoError> {
        let range = format!("{from}..{}", self.head_sha);
        let commit = self.git(
            &[
                "log",
                "--first-parent",
                "--diff-filter=D",
                "--no-renames",
                "--format=%H",
                "-1",
                &range,
                "--",
                path,
            ],
            None,
        )?;
        if commit.is_empty() {
            return Ok(None);
        }
        let parent = format!("{commit}^");
        let out = self.git(
            &[
                "diff",
                "-z",
                "-M",
                "--name-status",
                "--diff-filter=R",
                &parent,
                &commit,
            ],
            None,
        )?;
        // Each rename: `R<score>\0<old>\0<new>\0`.
        let fields: Vec<&str> = out.split('\0').collect();
        let moved_to = fields
            .chunks(3)
            .find(|c| c.len() == 3 && c[1] == path)
            .map(|c| c[2].to_string());
        Ok(Some((commit, moved_to)))
    }

    /// Lines in `old` at `from`, and how many of them are deleted or changed in `new` at
    /// `head_sha`.
    pub fn lines_changed(
        &self,
        from: &str,
        old: &str,
        new: &str,
    ) -> Result<(usize, usize), RepoError> {
        let old = format!("{from}:{old}");
        let text = self.git(&["cat-file", "-p", &old], None)?;
        let new = format!("{}:{new}", self.head_sha);
        let numstat = self.git(&["diff", "--numstat", &old, &new], None)?;
        // `<added>\t<deleted>\t<path>`; binary files show `-`.
        let deleted = numstat
            .split('\t')
            .nth(1)
            .and_then(|d| d.parse().ok())
            .unwrap_or(0);
        Ok((text.lines().count(), deleted))
    }

    /// The first `path` and line at `rev` containing `needle` as a fixed string (as a whole
    /// word with `word`), skipping binary files.
    pub fn grep(
        &self,
        rev: &str,
        needle: &str,
        word: bool,
    ) -> Result<Option<(String, u32)>, RepoError> {
        let mut args = vec!["grep", "-z", "-n", "-I", "-F", "-e", needle];
        if word {
            args.push("-w");
        }
        args.extend([rev, "--"]);
        // Exit 1 means no match. Each match: `<rev>:<path>\0<line>\0<text>\n`.
        let out = git_output(Some(&self.path), &args, None, &[0, 1])?;
        let mut fields = out.split('\0');
        let path = fields
            .next()
            .and_then(|p| p.strip_prefix(&format!("{rev}:")));
        let line = fields.next().and_then(|l| l.parse().ok());
        Ok(path.zip(line).map(|(p, l)| (p.to_string(), l)))
    }

    /// The last first-parent commit in `from..head_sha` that changed how often `needle`
    /// appears (`git log -S`).
    pub fn last_change_of(&self, from: &str, needle: &str) -> Result<Option<String>, RepoError> {
        let range = format!("{from}..{}", self.head_sha);
        let pickaxe = format!("-S{needle}");
        let out = self.git(
            &[
                "log",
                "--first-parent",
                "--format=%H",
                "-1",
                &pickaxe,
                &range,
            ],
            None,
        )?;
        Ok((!out.is_empty()).then_some(out))
    }
}

/// Result of merging two commits without touching the index or working tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeTree {
    /// Tree of the merge result (with conflict markers when there are conflicts).
    pub tree: String,
    /// Paths with conflicts; empty for a clean merge.
    pub conflicts: Vec<String>,
}

/// Where the clone of `owner/name` is cached: `<OS cache dir>/stillvalid/repos/<owner>/<name>`.
pub fn cache_path(owner: &str, name: &str) -> Result<PathBuf, RepoError> {
    let env_dir = |key| {
        std::env::var_os(key)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let base = if cfg!(windows) {
        env_dir("LOCALAPPDATA")
    } else if cfg!(target_os = "macos") {
        env_dir("HOME").map(|h| h.join("Library").join("Caches"))
    } else {
        env_dir("XDG_CACHE_HOME")
            .filter(|p| p.is_absolute())
            .or_else(|| env_dir("HOME").map(|h| h.join(".cache")))
    };
    Ok(base
        .ok_or(RepoError::NoCacheDir)?
        .join("stillvalid")
        .join("repos")
        .join(owner)
        .join(name))
}

fn has_commit(dir: &Path, sha: &str, token: Option<&str>) -> bool {
    let spec = format!("{sha}^{{commit}}");
    git(Some(dir), &["cat-file", "-e", &spec], token).is_ok()
}

fn check_git_version() -> Result<(), RepoError> {
    let version = git(None, &["--version"], None)?;
    match parse_git_version(&version) {
        Some(v) if v >= MIN_GIT => Ok(()),
        _ => Err(RepoError::GitTooOld(version)),
    }
}

/// `git version 2.39.3 (Apple Git-146)` or `git version 2.45.1.windows.1` → (2, 39).
fn parse_git_version(s: &str) -> Option<(u32, u32)> {
    let mut parts = s.strip_prefix("git version ")?.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

/// Run git and return trimmed stdout.
fn git(dir: Option<&Path>, args: &[&str], token: Option<&str>) -> Result<String, RepoError> {
    Ok(git_output(dir, args, token, &[0])?.trim().to_string())
}

/// Run git and return raw stdout; exit codes outside `ok` are errors.
fn git_output(
    dir: Option<&Path>,
    args: &[&str],
    token: Option<&str>,
    ok: &[i32],
) -> Result<String, RepoError> {
    let out = git_command(dir, token)
        .args(args)
        .output()
        .map_err(RepoError::GitMissing)?;
    if !out.status.code().is_some_and(|c| ok.contains(&c)) {
        return Err(RepoError::Git {
            args: args.join(" "),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The token goes in through `GIT_CONFIG_*` env vars: not in argv, `.git/config`, or the URL.
fn git_command(dir: Option<&Path>, token: Option<&str>) -> Command {
    let mut cmd = Command::new("git");
    if let Some(dir) = dir {
        cmd.arg("-C").arg(dir);
    }
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    if let Some(token) = token {
        // Append to any GIT_CONFIG_* entries the caller already set.
        let n: usize = std::env::var("GIT_CONFIG_COUNT")
            .ok()
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        cmd.env("GIT_CONFIG_COUNT", (n + 1).to_string())
            .env(
                format!("GIT_CONFIG_KEY_{n}"),
                "http.https://github.com/.extraheader",
            )
            .env(format!("GIT_CONFIG_VALUE_{n}"), auth_header(token));
    }
    cmd
}

/// Same scheme as actions/checkout.
fn auth_header(token: &str) -> String {
    let basic = base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{token}"));
    format!("Authorization: Basic {basic}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const MISSING: &str = "0123456789abcdef0123456789abcdef01234567";

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
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    /// A bare "remote" plus a working repo that pushes to it. `commit` returns the new SHA.
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
            sh(&work, &["init", "--quiet", "-b", "main"]);
            let remote = Self { tmp };
            sh(&work, &["remote", "add", "origin", &remote.url()]);
            remote
        }

        fn work(&self) -> PathBuf {
            self.tmp.path().join("work")
        }

        fn url(&self) -> String {
            let path = self.tmp.path().join("remote.git");
            let path = path.to_string_lossy().replace('\\', "/");
            format!(
                "file://{}{path}",
                if path.starts_with('/') { "" } else { "/" }
            )
        }

        fn commit(&self, file: &str, text: &str) -> String {
            std::fs::write(self.work().join(file), text).unwrap();
            sh(&self.work(), &["add", "."]);
            sh(&self.work(), &["commit", "--quiet", "-m", text]);
            sh(&self.work(), &["push", "--quiet", "origin", "main"]);
            sh(&self.work(), &["rev-parse", "HEAD"])
        }

        fn cache(&self) -> PathBuf {
            self.tmp.path().join("cache").join("acme").join("rocketdb")
        }
    }

    #[test]
    fn fresh_clone_checks_out_head() {
        let remote = Remote::new();
        let sha = remote.commit("a.txt", "one");
        let repo =
            Repo::clone_or_update(&remote.url(), &remote.cache(), "main", &sha, None).unwrap();
        assert_eq!(repo.path, remote.cache());
        assert_eq!(repo.head_sha, sha);
        assert_eq!(sh(&repo.path, &["rev-parse", "HEAD"]), sha);
        assert_eq!(
            std::fs::read_to_string(repo.path.join("a.txt")).unwrap(),
            "one"
        );
    }

    #[test]
    fn existing_clone_is_updated() {
        let remote = Remote::new();
        let first = remote.commit("a.txt", "one");
        Repo::clone_or_update(&remote.url(), &remote.cache(), "main", &first, None).unwrap();
        let second = remote.commit("a.txt", "two");
        let repo =
            Repo::clone_or_update(&remote.url(), &remote.cache(), "main", &second, None).unwrap();
        assert_eq!(sh(&repo.path, &["rev-parse", "HEAD"]), second);
        assert_eq!(
            std::fs::read_to_string(repo.path.join("a.txt")).unwrap(),
            "two"
        );
    }

    #[test]
    fn head_behind_remote_is_still_checked_out() {
        let remote = Remote::new();
        let scanned = remote.commit("a.txt", "one");
        remote.commit("a.txt", "two");
        let repo =
            Repo::clone_or_update(&remote.url(), &remote.cache(), "main", &scanned, None).unwrap();
        assert_eq!(sh(&repo.path, &["rev-parse", "HEAD"]), scanned);
    }

    #[test]
    fn unknown_head_is_branch_moved() {
        let remote = Remote::new();
        let sha = remote.commit("a.txt", "one");
        let err = Repo::clone_or_update(&remote.url(), &remote.cache(), "main", MISSING, None)
            .unwrap_err();
        match err {
            RepoError::BranchMoved {
                branch,
                expected,
                found,
            } => {
                assert_eq!(branch, "main");
                assert_eq!(expected, MISSING);
                assert_eq!(found, sha);
            }
            other => panic!("expected BranchMoved, got {other:?}"),
        }
    }

    #[test]
    fn existing_checkout_is_used_as_is() {
        let remote = Remote::new();
        let first = remote.commit("a.txt", "one");
        let second = remote.commit("a.txt", "two");
        let repo = Repo::open_existing(&remote.work(), &first).unwrap();
        assert_eq!(repo.path, remote.work());
        assert_eq!(repo.head_sha, first);
        assert_eq!(sh(&remote.work(), &["rev-parse", "HEAD"]), second);

        let err = Repo::open_existing(&remote.work(), MISSING).unwrap_err();
        assert!(matches!(err, RepoError::MissingCommit { .. }), "{err:?}");
    }

    #[test]
    fn blob_shas_lists_files_at_head() {
        let remote = Remote::new();
        std::fs::create_dir_all(remote.work().join("src")).unwrap();
        std::fs::write(remote.work().join("src").join("b c.rs"), "b").unwrap();
        let first = remote.commit("a.txt", "one");
        remote.commit("a.txt", "two");
        let repo = Repo::open_existing(&remote.work(), &first).unwrap();
        let blobs = repo.blob_shas().unwrap();
        assert_eq!(blobs.len(), 2);
        assert_eq!(
            blobs["a.txt"],
            sh(&remote.work(), &["rev-parse", &format!("{first}:a.txt")])
        );
        assert_eq!(
            blobs["src/b c.rs"],
            sh(
                &remote.work(),
                &["rev-parse", &format!("{first}:src/b c.rs")]
            )
        );
    }

    #[test]
    fn parses_git_versions() {
        assert_eq!(parse_git_version("git version 2.43.0"), Some((2, 43)));
        assert_eq!(
            parse_git_version("git version 2.39.3 (Apple Git-146)"),
            Some((2, 39))
        );
        assert_eq!(
            parse_git_version("git version 2.45.1.windows.1"),
            Some((2, 45))
        );
        assert_eq!(parse_git_version("nonsense"), None);
    }

    #[test]
    fn auth_header_is_basic_x_access_token() {
        assert_eq!(
            auth_header("abc"),
            "Authorization: Basic eC1hY2Nlc3MtdG9rZW46YWJj"
        );
    }
}
