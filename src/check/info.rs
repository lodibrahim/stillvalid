//! Issue check: `needs_info` for bug reports that give nothing to check against.

use super::pulls::short_sha;
use super::Finding;
use crate::fetch::{Association, FetchError, Fetcher, Issue, Label, Snapshot};
use crate::index::{self, RefKind};
use crate::store::{Confidence, Evidence, EvidenceType};
use chrono::{DateTime, Duration, Utc};
use regex::{Regex, RegexSet};
use std::collections::BTreeMap;
use std::sync::LazyLock;

/// Days an issue must be open before the reporter's silence means anything.
pub const MIN_AGE_DAYS: i64 = 14;

/// Labels that make an issue something other than an unclear bug report (features, questions,
/// docs, ...), or show a maintainer already judged it (not a bug, invalid, confirmed, reproduced).
static OTHER_LABEL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)feature|enhancement|request|question|discussion|\bdocs?\b|documentation|idea|proposal|rfc|wontfix|duplicate|support|not.?a.?bug|invalid|confirmed|reproduc")
        .unwrap()
});
static BUG_LABEL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bbug|crash|regression|defect").unwrap());
static OTHER_TITLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*\[?\s*(?:feature|feat|request|question|rfc|proposal|idea|discussion)\b")
        .unwrap()
});
static BUG_TITLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:bug|crash(?:es|ed)?|panic(?:s|ked)?|segfault|broken|doesn'?t work|does not work|not working|fails?|failed|failing|wrong|incorrect|error|regression|hangs?|freezes?)\b")
        .unwrap()
});

/// Anything in the issue text a maintainer could check against: code, a command, a version,
/// an OS, repro steps, or a screenshot.
static INFO: LazyLock<RegexSet> = LazyLock::new(|| {
    RegexSet::new([
        r"```|~~~|(?m)^(?: {4}|\t)\S",
        r"`[^`\n]+`",
        r"(?m)^\s*[$>#]\s+\S",
        r"\bv?\d+\.\d+(?:\.\d+)?\b",
        r"(?i)\b(?:windows|win10|win11|macos|mac os|osx|linux|ubuntu|debian|fedora|arch|nixos|freebsd|wsl|android|ios)\b",
        r"(?i)steps|reproduc|\brepro\b|expected|actual",
        r"(?i)!\[[^\]]*\]\(|<img|<video|user-attachments|\.(?:png|jpe?g|gif|mp4|mov|webm)\b|asciinema",
    ])
    .unwrap()
});

/// `needs_info` findings from [`check_all`].
#[derive(Debug, Default)]
pub struct InfoChecks {
    /// Findings keyed by issue number.
    pub findings: BTreeMap<u64, Finding>,
    /// Issues whose comments could not be read.
    pub failed: Vec<(u64, FetchError)>,
}

/// Check every candidate (see [`candidate`]) without a finding in `skip`: it needs info when its
/// reporter never commented, read with [`Fetcher::commented`] only when it has comments.
/// `snapshot` must be fetched with references (a token), or every issue looks unreferenced.
pub async fn check_all(
    fetcher: &Fetcher,
    snapshot: &Snapshot,
    skip: &BTreeMap<u64, Finding>,
    now: DateTime<Utc>,
) -> InfoChecks {
    let mut checks = InfoChecks::default();
    for issue in &snapshot.issues {
        if skip.contains_key(&issue.number) {
            continue;
        }
        let Some(reporter) = candidate(issue, snapshot, now) else {
            continue;
        };
        let commented = match issue.comments {
            0 => Ok(false),
            _ => {
                fetcher
                    .commented(&snapshot.repo, issue.number, reporter)
                    .await
            }
        };
        match commented {
            Ok(false) => {
                checks
                    .findings
                    .insert(issue.number, finding(issue, &snapshot.head_sha));
            }
            Ok(true) => {}
            Err(e) => checks.failed.push((issue.number, e)),
        }
    }
    checks
}

/// An issue is a `needs_info` candidate when it is a bug report (a bug-like label, or no labels
/// and a bug word in the title; never a feature request, question, or docs issue), filed by a
/// known account outside the project (not an owner, member, collaborator, or contributor) at
/// least [`MIN_AGE_DAYS`] ago, with no code, command, version, OS, repro steps, screenshot, or
/// code reference ([`index::extract`] path, frame, or symbol), and nothing references it.
/// Returns the reporter's login.
pub fn candidate<'a>(issue: &'a Issue, snapshot: &Snapshot, now: DateTime<Utc>) -> Option<&'a str> {
    let reporter = issue.user.as_ref()?;
    let insider = matches!(
        issue.author_association,
        Association::Owner
            | Association::Member
            | Association::Collaborator
            | Association::Contributor
    );
    let qualifies = !insider
        && now - issue.created_at >= Duration::days(MIN_AGE_DAYS)
        && !snapshot.references.contains_key(&issue.number)
        && is_bug_report(&issue.title, &issue.labels)
        && !has_info(&issue.title, issue.body.as_deref().unwrap_or(""));
    qualifies.then_some(reporter.login.as_str())
}

/// The `needs_info` finding for a candidate whose reporter never commented. Always `low`: it
/// rests on what is missing. Evidence is the scanned commit.
pub fn finding(issue: &Issue, head_sha: &str) -> Finding {
    Finding {
        confidence: Confidence::Low,
        evidence: vec![Evidence {
            kind: EvidenceType::Commit,
            reference: short_sha(head_sha).to_string(),
            note: format!(
                "No version, OS, repro steps, command, code, or screenshot in the issue, and no comment from the reporter since it was filed on {}",
                issue.created_at.format("%Y-%m-%d")
            ),
        }],
    }
}

fn is_bug_report(title: &str, labels: &[Label]) -> bool {
    if labels.iter().any(|l| OTHER_LABEL.is_match(&l.name))
        || OTHER_TITLE.is_match(title)
        || title.trim_end().ends_with('?')
    {
        return false;
    }
    if labels.is_empty() {
        BUG_TITLE.is_match(title)
    } else {
        labels.iter().any(|l| BUG_LABEL.is_match(&l.name))
    }
}

fn has_info(title: &str, body: &str) -> bool {
    INFO.is_match(&format!("{title}\n{body}"))
        || index::extract(title, body)
            .iter()
            .any(|r| matches!(r.kind, RefKind::Path | RefKind::Frame | RefKind::Symbol))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fetch::{Login, PullRef, Reference};
    use octocrab::Octocrab;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn ts(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    const NOW: &str = "2026-09-27T00:00:00Z";

    const VAGUE: &str = "it happens when I try to search inside of the staged changes panel";

    fn issue(title: &str, body: &str, labels: &[&str]) -> Issue {
        Issue {
            number: 7,
            title: title.into(),
            html_url: "https://github.com/o/r/issues/7".into(),
            created_at: ts("2026-09-01T00:00:00Z"),
            body: Some(body.into()),
            labels: labels
                .iter()
                .map(|l| Label {
                    name: l.to_string(),
                })
                .collect(),
            user: Some(Login {
                login: "ann".into(),
            }),
            ..Default::default()
        }
    }

    fn snapshot() -> Snapshot {
        Snapshot {
            repo: "o/r".into(),
            branch: "main".into(),
            head_sha: "9f1c2ab7e0d4".into(),
            issues: Vec::new(),
            pulls: Vec::new(),
            references: Default::default(),
            reopened_at: Default::default(),
            commits_on_branch: Default::default(),
            pull_activity: Default::default(),
        }
    }

    fn is_candidate(issue: &Issue) -> bool {
        candidate(issue, &snapshot(), ts(NOW)).is_some()
    }

    #[test]
    fn vague_bug_report_is_a_candidate() {
        let i = issue("panic: nil pointer dereference", VAGUE, &["bug"]);
        assert!(is_candidate(&i));
        assert_eq!(
            finding(&i, "9f1c2ab7e0d4"),
            Finding {
                confidence: Confidence::Low,
                evidence: vec![Evidence {
                    kind: EvidenceType::Commit,
                    reference: "9f1c2ab".into(),
                    note: "No version, OS, repro steps, command, code, or screenshot in the issue, and no comment from the reporter since it was filed on 2026-09-01".into(),
                }],
            }
        );
        // Unlabeled, with a bug word in the title; or any bug-like label and no body.
        assert!(is_candidate(&issue("Search is broken", VAGUE, &[])));
        assert!(is_candidate(&issue("Search is broken", "", &["type: bug"])));
    }

    #[test]
    fn only_bug_reports_qualify() {
        let vague = |title: &str, labels: &[&str]| is_candidate(&issue(title, VAGUE, labels));
        assert!(!vague("Search is broken", &["bug", "enhancement"]));
        assert!(!vague("Search is broken", &["question"]));
        assert!(!vague("Search is broken", &["documentation"]));
        assert!(!vague("Search is broken", &["debug-tools"]));
        assert!(!vague("Search is broken", &["not a bug"]));
        assert!(!vague("Search is broken", &["not-a-bug"]));
        assert!(!vague("Search is broken", &["invalid"]));
        assert!(!vague("Search is broken", &["bug", "confirmed"]));
        assert!(!vague("Search is broken", &["bug", "status: reproduced"]));
        assert!(!vague("Search is broken", &["performance"]));
        assert!(!vague("Why is search broken?", &["bug"]));
        assert!(!vague("[Feature] error on search", &["bug"]));
        assert!(!vague("Limit threads via an env variable", &[]));
    }

    #[test]
    fn any_detail_is_enough_info() {
        for body in [
            "```\nrg foo\n```",
            "    rg foo",
            "running `rg foo` does nothing",
            "$ rg foo",
            "on 14.1.0",
            "on Windows 11",
            "Steps: open it and search",
            "What I expected: a result",
            "![shot](https://example.com/a.png)",
            "see https://github.com/o/r/blob/main/src/search.rs#L10",
            "thread 'main' panicked at src/search.rs:10:5:\nboom",
        ] {
            let i = issue("Search is broken", &format!("{VAGUE}\n{body}"), &["bug"]);
            assert!(!is_candidate(&i), "{body}");
        }
        let i = issue("`quit` doesn't work while searching", VAGUE, &["bug"]);
        assert!(!is_candidate(&i));
    }

    #[test]
    fn insiders_deleted_users_references_and_new_issues_are_skipped() {
        let i = issue("Search is broken", VAGUE, &["bug"]);
        for author_association in [
            Association::Owner,
            Association::Member,
            Association::Collaborator,
            Association::Contributor,
        ] {
            assert!(!is_candidate(&Issue {
                author_association,
                ..i.clone()
            }));
        }
        assert!(!is_candidate(&Issue {
            user: None,
            ..i.clone()
        }));

        let mut snap = snapshot();
        snap.references.insert(
            7,
            vec![Reference::Pull(PullRef {
                number: 9,
                url: "https://github.com/o/r/pull/9".into(),
                merged_at: None,
                base_ref: "main".into(),
                will_close: false,
            })],
        );
        assert!(candidate(&i, &snap, ts(NOW)).is_none());

        // 13 days old is too new; 14 is old enough.
        assert!(candidate(&i, &snapshot(), ts("2026-09-14T00:00:00Z")).is_none());
        assert_eq!(
            candidate(&i, &snapshot(), ts("2026-09-15T00:00:00Z")),
            Some("ann")
        );
    }

    #[tokio::test]
    async fn check_all_reads_comments_only_when_there_are_some() {
        let server = MockServer::start().await;
        for (number, login) in [(2, "bob"), (3, "ann")] {
            Mock::given(method("GET"))
                .and(path(format!("/repos/o/r/issues/{number}/comments")))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(json!([{ "user": { "login": login } }])),
                )
                .expect(1)
                .mount(&server)
                .await;
        }
        let fetcher = Fetcher::new(
            Octocrab::builder()
                .base_uri(server.uri())
                .unwrap()
                .build()
                .unwrap(),
        );
        let vague = |number, comments| Issue {
            number,
            comments,
            ..issue("Search is broken", VAGUE, &["bug"])
        };
        let mut snap = snapshot();
        // 1: no comments; 2: others commented; 3: the reporter replied; 4: comments fail to load;
        // 5: already has a stronger finding.
        snap.issues = vec![
            vague(1, 0),
            vague(2, 1),
            vague(3, 1),
            vague(4, 1),
            vague(5, 0),
        ];
        let skip = BTreeMap::from([(5, finding(&vague(5, 0), "aaaaaaa"))]);

        let checks = check_all(&fetcher, &snap, &skip, ts(NOW)).await;
        let found: Vec<u64> = checks.findings.keys().copied().collect();
        assert_eq!(found, [1, 2]);
        assert_eq!(checks.findings[&1].evidence[0].reference, "9f1c2ab");
        let failed: Vec<u64> = checks.failed.iter().map(|(n, _)| *n).collect();
        assert_eq!(failed, [4]);
    }
}
