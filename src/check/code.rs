//! Issue check against the local repo: the code an issue names is gone.

use super::pulls::short_sha;
use super::{issues, Finding};
use crate::fetch::{Issue, Snapshot};
use crate::index::{self, RefKind};
use crate::repo::{Repo, RepoError};
use crate::store::{Confidence, Evidence, EvidenceType};
use std::collections::{BTreeMap, HashSet};

/// Docs, config, and lock files: churn in them is not a fix.
const NON_CODE_EXTS: &[&str] = &[
    "md", "txt", "rst", "lock", "toml", "yml", "yaml", "json", "cfg", "ini", "conf", "xml", "html",
    "css",
];

/// A file counts as heavily rewritten when at least this percentage of its lines changed ...
const REWRITTEN_PERCENT: usize = 75;
/// ... and it had at least this many lines when the issue was filed.
const REWRITTEN_MIN_LINES: usize = 20;
const MIN_SYMBOL_LEN: usize = 4;
const MIN_ERROR_LEN: usize = 20;
/// Renames followed per path before giving up.
const MAX_RENAMES: usize = 10;

/// Result of [`check_all`].
#[derive(Debug, Default)]
pub struct CodeChecks {
    /// Findings keyed by issue number.
    pub findings: BTreeMap<u64, Finding>,
    /// Issues that could not be checked.
    pub failed: Vec<(u64, RepoError)>,
}

/// Run [`code_gone`] on every issue that [`issues::likely_fixed`] has no finding for. Only saves
/// git work: `store::build_report` decides that a PR or commit that says it fixes the issue wins.
pub fn check_all(repo: &Repo, snapshot: &Snapshot, token: Option<&str>) -> CodeChecks {
    let token = repo.fetch_token(token);
    let mut checks = CodeChecks::default();
    for issue in &snapshot.issues {
        if issues::likely_fixed(issue, snapshot).is_some() {
            continue;
        }
        match code_gone(issue, repo, token) {
            Ok(Some(f)) => {
                checks.findings.insert(issue.number, f);
            }
            Ok(None) => {}
            Err(e) => checks.failed.push((issue.number, e)),
        }
    }
    checks
}

/// An issue is likely fixed (`medium`) when code files, symbols, or error strings it names
/// existed on the branch when it was filed, at least one of them was since deleted or heavily
/// rewritten, and none is still there. Files moved by a rename are followed to their new path.
/// A file only counts as gone when the issue ties it to code with a line number (`path:line`,
/// stack frame, panic location, blob URL `#L`); a plain mention may be a command argument or
/// input file, so it can only block. References that never existed in the repo (repro files,
/// other repos) are ignored.
/// `token` authenticates the lazy blob fetches of a blobless clone.
pub fn code_gone(
    issue: &Issue,
    repo: &Repo,
    token: Option<&str>,
) -> Result<Option<Finding>, RepoError> {
    let Some(base) = repo.commit_before(issue.created_at)? else {
        return Ok(None);
    };
    let refs = index::extract(&issue.title, issue.body.as_deref().unwrap_or(""));

    let mut base_files = None;
    let mut seen_paths = HashSet::new();
    let mut seen_needles = HashSet::new();
    let mut evidence = Vec::new();
    for r in refs {
        let gone = match r.kind {
            RefKind::Path => {
                let Some(path) = r.path.as_deref().filter(|p| is_code_file(p)) else {
                    continue;
                };
                let Some(path) = resolve_path(repo, &base, path, &mut base_files)? else {
                    continue;
                };
                let tied = r.line.is_some();
                if !seen_paths.insert((path.clone(), tied)) {
                    continue;
                }
                match path_gone(repo, &base, &path, token)? {
                    State::Gone(_) if !tied => State::Absent,
                    state => state,
                }
            }
            RefKind::Symbol | RefKind::Error => {
                let (needle, min, word) = match r.kind {
                    RefKind::Symbol => (symbol_needle(&r.text), MIN_SYMBOL_LEN, true),
                    _ => (r.text.as_str(), MIN_ERROR_LEN, false),
                };
                if needle.chars().count() < min || !seen_needles.insert(needle.to_string()) {
                    continue;
                }
                string_gone(repo, &base, needle, word, token)?
            }
            // Frames also emit their symbol and path, which are checked on their own.
            RefKind::Frame => continue,
        };
        match gone {
            State::Gone(e) => evidence.push(e),
            State::Present => return Ok(None),
            State::Absent => {}
        }
    }

    Ok((!evidence.is_empty()).then_some(Finding {
        confidence: Confidence::Medium,
        evidence,
    }))
}

enum State {
    /// Not in the repo when the issue was filed.
    Absent,
    /// Still there at the scanned commit.
    Present,
    /// Deleted or rewritten since.
    Gone(Evidence),
}

fn is_code_file(path: &str) -> bool {
    path.rsplit_once('.')
        .is_some_and(|(_, ext)| !NON_CODE_EXTS.contains(&ext.to_ascii_lowercase().as_str()))
}

/// The file at `base` that `path` names: the path itself, the path without a diff's `a/` or
/// `b/` prefix, or for an absolute path the one file at `base` ending in at least its last two
/// components.
fn resolve_path(
    repo: &Repo,
    base: &str,
    path: &str,
    base_files: &mut Option<Vec<String>>,
) -> Result<Option<String>, RepoError> {
    let undiffed = path.strip_prefix("a/").or_else(|| path.strip_prefix("b/"));
    for p in std::iter::once(path).chain(undiffed) {
        if repo.is_file(base, p) {
            return Ok(Some(p.to_string()));
        }
    }
    let absolute = path.starts_with('/') || index::is_drive_path(path);
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    if !absolute || parts.len() < 2 {
        return Ok(None);
    }
    let files = match base_files {
        Some(files) => files,
        None => base_files.insert(repo.files_at(base)?),
    };
    // Longest suffix first; a suffix matching several files is ambiguous.
    for start in 1..=parts.len() - 2 {
        let suffix = parts[start..].join("/");
        let nested = format!("/{suffix}");
        let mut matches = files
            .iter()
            .filter(|f| *f == &suffix || f.ends_with(&nested));
        if let Some(f) = matches.next() {
            return Ok(matches.next().is_none().then(|| f.clone()));
        }
    }
    Ok(None)
}

/// Follow `path` from `base` to the scanned commit through renames.
fn path_gone(repo: &Repo, base: &str, path: &str, token: Option<&str>) -> Result<State, RepoError> {
    let mut from = base.to_string();
    let mut current = path.to_string();
    let mut moves = Vec::new();
    for _ in 0..MAX_RENAMES {
        if repo.is_file(&repo.head_sha, &current) {
            let (lines, changed) = repo.lines_changed(base, path, &current, token)?;
            if lines < REWRITTEN_MIN_LINES || changed * 100 < REWRITTEN_PERCENT * lines {
                return Ok(State::Present);
            }
            let percent = changed * 100 / lines;
            return Ok(State::Gone(Evidence {
                kind: EvidenceType::Code,
                reference: current.clone(),
                note: format!(
                    "{}{percent}% of its lines changed since the issue was filed (at {})",
                    moved_note(path, &moves),
                    short_sha(base)
                ),
            }));
        }
        match repo.removal(&from, &current, token)? {
            Some((commit, Some(to))) => {
                moves.push(to.clone());
                current = to;
                from = commit;
            }
            Some((commit, None)) => {
                let note = if moves.is_empty() {
                    format!("Deleted {path} (named in the issue)")
                } else {
                    format!("{}then deleted", moved_note(path, &moves))
                };
                return Ok(State::Gone(Evidence {
                    kind: EvidenceType::Commit,
                    reference: short_sha(&commit).to_string(),
                    note,
                }));
            }
            // Gone from the first-parent history some other way; don't guess.
            None => return Ok(State::Absent),
        }
    }
    Ok(State::Absent)
}

/// `Renamed a.rs -> b.rs -> c.rs, ` or nothing.
fn moved_note(path: &str, moves: &[String]) -> String {
    if moves.is_empty() {
        return String::new();
    }
    format!("Renamed {path} -> {}, ", moves.join(" -> "))
}

fn string_gone(
    repo: &Repo,
    base: &str,
    needle: &str,
    word: bool,
    token: Option<&str>,
) -> Result<State, RepoError> {
    if repo.grep(&repo.head_sha, needle, word, token)?.is_some() {
        return Ok(State::Present);
    }
    let Some((path, line)) = repo.grep(base, needle, word, token)? else {
        return Ok(State::Absent);
    };
    let Some(commit) = repo.last_change_of(base, needle, word, token)? else {
        return Ok(State::Absent);
    };
    Ok(State::Gone(Evidence {
        kind: EvidenceType::Commit,
        reference: short_sha(&commit).to_string(),
        note: format!("Removed `{needle}` (named in the issue; was at {path}:{line} when filed)"),
    }))
}

/// The last `::` or `.` segment of a symbol: `grep_searcher::LineStep` → `LineStep`.
fn symbol_needle(symbol: &str) -> &str {
    symbol.rsplit([':', '.']).next().unwrap_or(symbol)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use std::path::Path;
    use std::process::Command;
    use tempfile::TempDir;

    /// Run git with a fixed identity and no user/system config; panics on failure.
    fn sh(dir: &Path, args: &[&str], date: &str) -> String {
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
            .env("GIT_AUTHOR_DATE", date)
            .env("GIT_COMMITTER_DATE", date)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    /// A repo whose commits are dated one per month of 2026, starting in January.
    struct Git {
        tmp: TempDir,
        month: u32,
    }

    impl Git {
        fn new() -> Self {
            let tmp = TempDir::new().unwrap();
            sh(tmp.path(), &["init", "--quiet", "-b", "main"], "");
            Self { tmp, month: 0 }
        }

        /// Apply `files` (`None` deletes) and commit; returns the SHA.
        fn commit(&mut self, files: &[(&str, Option<&str>)]) -> String {
            for (path, text) in files {
                let full = self.tmp.path().join(path);
                match text {
                    Some(text) => {
                        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
                        std::fs::write(full, text).unwrap();
                    }
                    None => std::fs::remove_file(full).unwrap(),
                }
            }
            self.month += 1;
            let date = format!("2026-{:02}-01T00:00:00Z", self.month);
            let dir = self.tmp.path();
            sh(dir, &["add", "-A"], &date);
            sh(dir, &["commit", "--quiet", "-m", "change"], &date);
            sh(dir, &["rev-parse", "HEAD"], &date)
        }

        fn mv(&mut self, from: &str, to: &str) -> String {
            let text = std::fs::read_to_string(self.tmp.path().join(from)).unwrap();
            self.commit(&[(from, None), (to, Some(&text))])
        }

        fn repo(&self) -> Repo {
            let head = sh(self.tmp.path(), &["rev-parse", "HEAD"], "");
            Repo::open_existing(self.tmp.path(), &head).unwrap()
        }
    }

    /// An issue filed mid-January, after the first commit.
    fn issue(body: &str) -> Issue {
        Issue {
            number: 7,
            title: "Crash".into(),
            html_url: "https://github.com/o/r/issues/7".into(),
            created_at: "2026-01-15T00:00:00Z".parse::<DateTime<Utc>>().unwrap(),
            body: Some(body.into()),
            pull_request: None,
        }
    }

    fn lines(n: usize, word: &str) -> String {
        (0..n).map(|i| format!("{word} {i}\n")).collect()
    }

    const POOL: &str = "fn release_slot() {}\n";

    #[test]
    fn deleted_file_is_gone_with_the_deleting_commit() {
        let mut g = Git::new();
        g.commit(&[
            ("src/pool.rs", Some(POOL)),
            ("src/main.rs", Some("fn main() {}\n")),
        ]);
        let del = g.commit(&[("src/pool.rs", None)]);
        let f = code_gone(&issue("Panics in src/pool.rs:3"), &g.repo(), None)
            .unwrap()
            .unwrap();
        assert_eq!(f.confidence, Confidence::Medium);
        assert_eq!(
            f.evidence,
            [Evidence {
                kind: EvidenceType::Commit,
                reference: short_sha(&del).into(),
                note: "Deleted src/pool.rs (named in the issue)".into(),
            }]
        );
    }

    #[test]
    fn plain_path_mention_cannot_make_it_gone() {
        // ripgrep #478: the deleted file was only the input to `rg -f`.
        let mut g = Git::new();
        g.commit(&[("src/search_stream.rs", Some(POOL))]);
        g.commit(&[("src/search_stream.rs", None)]);
        let body = "```\n$ rg -e foo -f src/search_stream.rs\nError parsing regex\n```";
        assert_eq!(code_gone(&issue(body), &g.repo(), None).unwrap(), None);
    }

    #[test]
    fn plain_path_mention_still_blocks() {
        let mut g = Git::new();
        g.commit(&[
            ("src/pool.rs", Some(POOL)),
            ("src/main.rs", Some("fn main() {}\n")),
        ]);
        g.commit(&[("src/pool.rs", None)]);
        let body = "src/pool.rs:1 is called from src/main.rs";
        assert_eq!(code_gone(&issue(body), &g.repo(), None).unwrap(), None);
        let f = code_gone(&issue("src/pool.rs and src/pool.rs:1"), &g.repo(), None);
        assert!(f.unwrap().is_some());
    }

    #[test]
    fn renamed_file_is_followed() {
        let mut g = Git::new();
        g.commit(&[("src/pool.rs", Some(&lines(30, "pool")))]);
        g.mv("src/pool.rs", "crates/pool.rs");
        assert_eq!(
            code_gone(&issue("See src/pool.rs:3"), &g.repo(), None).unwrap(),
            None
        );

        let del = g.commit(&[("crates/pool.rs", None)]);
        let f = code_gone(&issue("See src/pool.rs:3"), &g.repo(), None)
            .unwrap()
            .unwrap();
        assert_eq!(f.evidence[0].reference, short_sha(&del));
        assert_eq!(
            f.evidence[0].note,
            "Renamed src/pool.rs -> crates/pool.rs, then deleted"
        );
    }

    #[test]
    fn heavily_rewritten_file_is_gone() {
        let mut g = Git::new();
        let base = g.commit(&[("src/pool.rs", Some(&lines(40, "old")))]);
        let text = lines(10, "old") + &lines(30, "new");
        g.commit(&[("src/pool.rs", Some(&text))]);
        let f = code_gone(&issue("src/pool.rs:1"), &g.repo(), None)
            .unwrap()
            .unwrap();
        assert_eq!(
            f.evidence,
            [Evidence {
                kind: EvidenceType::Code,
                reference: "src/pool.rs".into(),
                note: format!(
                    "75% of its lines changed since the issue was filed (at {})",
                    short_sha(&base)
                ),
            }]
        );
    }

    #[test]
    fn lightly_changed_or_short_file_is_present() {
        let mut g = Git::new();
        g.commit(&[
            ("src/pool.rs", Some(&lines(40, "old"))),
            ("src/tiny.rs", Some(&lines(10, "old"))),
        ]);
        let mut text = lines(20, "old");
        text.push_str(&lines(20, "new"));
        g.commit(&[
            ("src/pool.rs", Some(&text)),
            ("src/tiny.rs", Some(&lines(10, "new"))),
        ]);
        let repo = g.repo();
        assert_eq!(
            code_gone(&issue("src/pool.rs:1"), &repo, None).unwrap(),
            None
        );
        assert_eq!(
            code_gone(&issue("src/tiny.rs:1"), &repo, None).unwrap(),
            None
        );
    }

    #[test]
    fn files_not_in_the_repo_when_filed_are_ignored() {
        let mut g = Git::new();
        g.commit(&[("src/main.rs", Some("fn main() {}\n"))]);
        // Added after the issue was filed, then deleted.
        g.commit(&[("src/late.rs", Some(POOL))]);
        g.commit(&[("src/late.rs", None)]);
        let body = "repro in test/repro.rs:1, see src/late.rs:1";
        assert_eq!(code_gone(&issue(body), &g.repo(), None).unwrap(), None);
    }

    #[test]
    fn non_code_files_are_ignored() {
        let mut g = Git::new();
        g.commit(&[
            ("docs/guide.md", Some("hi\n")),
            ("Cargo.toml", Some("[x]\n")),
        ]);
        g.commit(&[("docs/guide.md", None), ("Cargo.toml", None)]);
        let body = "docs/guide.md:1 and Cargo.toml:1";
        assert_eq!(code_gone(&issue(body), &g.repo(), None).unwrap(), None);
    }

    #[test]
    fn diff_prefix_and_absolute_paths_resolve() {
        let mut g = Git::new();
        g.commit(&[
            ("src/pool.rs", Some(POOL)),
            ("crates/a/src/lib.rs", Some("1\n")),
        ]);
        let del = g.commit(&[("src/pool.rs", None), ("crates/a/src/lib.rs", None)]);
        let body = "`a/src/pool.rs:3`\n\nat /home/u/proj/crates/a/src/lib.rs:3";
        let f = code_gone(&issue(body), &g.repo(), None).unwrap().unwrap();
        let notes: Vec<&str> = f.evidence.iter().map(|e| e.note.as_str()).collect();
        assert_eq!(
            notes,
            [
                "Deleted src/pool.rs (named in the issue)",
                "Deleted crates/a/src/lib.rs (named in the issue)"
            ]
        );
        assert!(f.evidence.iter().all(|e| e.reference == short_sha(&del)));
    }

    #[test]
    fn relative_paths_get_no_suffix_match() {
        let mut g = Git::new();
        g.commit(&[("src/pool.rs", Some(POOL))]);
        g.commit(&[("src/pool.rs", None)]);
        assert_eq!(
            code_gone(&issue("my crate's test/src/pool.rs:1"), &g.repo(), None).unwrap(),
            None
        );
    }

    #[test]
    fn removed_symbol_is_gone_with_the_removing_commit() {
        let mut g = Git::new();
        g.commit(&[("src/pool.rs", Some("fn keep() {}\nfn release_slot() {}\n"))]);
        let removed = g.commit(&[("src/pool.rs", Some("fn keep() {}\n"))]);
        // A longer name containing it is not the removal.
        g.commit(&[(
            "src/pool.rs",
            Some("fn keep() {}\nfn release_slots_v2() {}\n"),
        )]);
        let f = code_gone(&issue("`Pool::release_slot` hangs"), &g.repo(), None)
            .unwrap()
            .unwrap();
        assert_eq!(
            f.evidence,
            [Evidence {
                kind: EvidenceType::Commit,
                reference: short_sha(&removed).into(),
                note:
                    "Removed `release_slot` (named in the issue; was at src/pool.rs:2 when filed)"
                        .into(),
            }]
        );
    }

    #[test]
    fn surviving_or_short_symbols_and_removed_panic_message() {
        let mut g = Git::new();
        g.commit(&[(
            "src/pool.rs",
            Some("fn release_slot() {}\nfn run() { panic!(\"pool closed while waiting\") }\n"),
        )]);
        let removed = g.commit(&[("src/pool.rs", Some("fn release_slot() {}\n"))]);
        let repo = g.repo();
        assert_eq!(
            code_gone(&issue("`release_slot`"), &repo, None).unwrap(),
            None
        );
        // `run` is too short to count.
        assert_eq!(
            code_gone(&issue("`Pool::run()`"), &repo, None).unwrap(),
            None
        );

        let panic = "thread 'main' panicked at src/gone.rs:1:5:\npool closed while waiting";
        let f = code_gone(&issue(panic), &repo, None).unwrap().unwrap();
        assert_eq!(f.evidence[0].reference, short_sha(&removed));
        assert_eq!(
            f.evidence[0].note,
            "Removed `pool closed while waiting` (named in the issue; was at src/pool.rs:2 when filed)"
        );
    }

    #[test]
    fn issue_older_than_the_history_is_skipped() {
        let mut g = Git::new();
        g.commit(&[("src/pool.rs", Some(POOL))]);
        let mut old = issue("src/pool.rs:1");
        old.created_at = "2025-01-01T00:00:00Z".parse().unwrap();
        assert_eq!(code_gone(&old, &g.repo(), None).unwrap(), None);
    }
}
