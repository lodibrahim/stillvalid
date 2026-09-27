//! Issue check: `needs_info` for bug reports that give nothing to check against.

use super::pulls::short_sha;
use super::Finding;
use crate::fetch::{Association, Issue, Snapshot};
use crate::index::{self, RefKind};
use crate::store::{Confidence, Evidence, EvidenceType};
use chrono::{DateTime, Duration, Utc};
use regex::Regex;
use std::sync::LazyLock;

/// Days an issue must be open before the reporter's silence means anything.
pub const MIN_AGE_DAYS: i64 = 14;

/// Labels that make an issue something other than a bug report (features, questions, docs, ...).
static OTHER_LABEL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)feature|enhancement|request|question|discussion|\bdocs?\b|documentation|idea|proposal|rfc|wontfix|duplicate|support")
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
static INFO: LazyLock<[Regex; 7]> = LazyLock::new(|| {
    [
        r"```|~~~|(?m)^(?: {4}|\t)\S",
        r"`[^`\n]+`",
        r"(?m)^\s*[$>#]\s+\S",
        r"\bv?\d+\.\d+(?:\.\d+)?\b",
        r"(?i)\b(?:windows|win10|win11|macos|mac os|osx|linux|ubuntu|debian|fedora|arch|nixos|freebsd|wsl|android|ios)\b",
        r"(?i)steps|reproduc|\brepro\b|expected|actual",
        r"(?i)!\[[^\]]*\]\(|<img|<video|user-attachments|\.(?:png|jpe?g|gif|mp4|mov|webm)\b|asciinema",
    ]
    .map(|re| Regex::new(re).unwrap())
});

/// An issue needs info when it is a bug report (a bug-like label, or no labels and a bug word in
/// the title; never a feature request, question, or docs issue), filed by someone outside the
/// project (not an owner, member, collaborator, or contributor) at least [`MIN_AGE_DAYS`] ago,
/// has no code, command, version, OS, repro steps, screenshot, or code reference
/// ([`index::extract`] path, frame, or symbol), the reporter never commented, and nothing
/// references it. Always `low`: it rests on what is missing. Evidence is the scanned commit.
/// `None` without fetched issue activity (no token).
pub fn needs_info(issue: &Issue, snapshot: &Snapshot, now: DateTime<Utc>) -> Option<Finding> {
    let activity = snapshot.issue_activity.get(&issue.number)?;
    let body = issue.body.as_deref().unwrap_or("");
    let insider = matches!(
        activity.association,
        Association::Owner
            | Association::Member
            | Association::Collaborator
            | Association::Contributor
    );
    if insider
        || activity.reporter_commented
        || now - issue.created_at < Duration::days(MIN_AGE_DAYS)
        || snapshot.references.contains_key(&issue.number)
        || !is_bug_report(&issue.title, &activity.labels)
        || has_info(&issue.title, body)
    {
        return None;
    }
    Some(Finding {
        confidence: Confidence::Low,
        evidence: vec![Evidence {
            kind: EvidenceType::Commit,
            reference: short_sha(&snapshot.head_sha).to_string(),
            note: format!(
                "No version, OS, repro steps, command, code, or screenshot in the issue, and no comment from the reporter since it was filed on {}",
                issue.created_at.format("%Y-%m-%d")
            ),
        }],
    })
}

fn is_bug_report(title: &str, labels: &[String]) -> bool {
    if labels.iter().any(|l| OTHER_LABEL.is_match(l)) || OTHER_TITLE.is_match(title) {
        return false;
    }
    if title.trim_end().ends_with('?') {
        return false;
    }
    if labels.is_empty() {
        BUG_TITLE.is_match(title)
    } else {
        labels.iter().any(|l| BUG_LABEL.is_match(l))
    }
}

fn has_info(title: &str, body: &str) -> bool {
    let text = format!("{title}\n{body}");
    INFO.iter().any(|re| re.is_match(&text))
        || index::extract(title, body)
            .iter()
            .any(|r| matches!(r.kind, RefKind::Path | RefKind::Frame | RefKind::Symbol))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fetch::{IssueActivity, PullRef, Reference};

    fn ts(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    const NOW: &str = "2026-09-27T00:00:00Z";

    fn issue(title: &str, body: &str) -> Issue {
        Issue {
            number: 7,
            title: title.into(),
            html_url: "https://github.com/o/r/issues/7".into(),
            created_at: ts("2026-09-01T00:00:00Z"),
            body: Some(body.into()),
            pull_request: None,
        }
    }

    fn activity(labels: &[&str]) -> IssueActivity {
        IssueActivity {
            labels: labels.iter().map(|l| l.to_string()).collect(),
            association: Association::Other,
            reporter_commented: false,
        }
    }

    fn snapshot(activity: Option<IssueActivity>) -> Snapshot {
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
            issue_activity: activity.into_iter().map(|a| (7, a)).collect(),
        }
    }

    fn check(issue: &Issue, activity: IssueActivity) -> Option<Finding> {
        needs_info(issue, &snapshot(Some(activity)), ts(NOW))
    }

    const VAGUE: &str = "it happens when I try to search inside of the staged changes panel";

    #[test]
    fn vague_bug_report_needs_info() {
        let f = check(
            &issue("panic: nil pointer dereference", VAGUE),
            activity(&["bug"]),
        )
        .unwrap();
        assert_eq!(f.confidence, Confidence::Low);
        assert_eq!(
            f.evidence,
            [Evidence {
                kind: EvidenceType::Commit,
                reference: "9f1c2ab".into(),
                note: "No version, OS, repro steps, command, code, or screenshot in the issue, and no comment from the reporter since it was filed on 2026-09-01".into(),
            }]
        );
        // Unlabeled, with a bug word in the title.
        assert!(check(&issue("Search is broken", VAGUE), activity(&[])).is_some());
        assert!(check(&issue("Search is broken", ""), activity(&["type: bug"])).is_some());
    }

    #[test]
    fn only_bug_reports_qualify() {
        let vague = |title: &str, labels: &[&str]| check(&issue(title, VAGUE), activity(labels));
        assert!(vague("Search is broken", &["bug", "enhancement"]).is_none());
        assert!(vague("Search is broken", &["question"]).is_none());
        assert!(vague("Search is broken", &["documentation"]).is_none());
        assert!(vague("Search is broken", &["debug-tools"]).is_none());
        assert!(vague("Search is broken", &["performance"]).is_none());
        assert!(vague("Why is search broken?", &["bug"]).is_none());
        assert!(vague("[Feature] error on search", &["bug"]).is_none());
        assert!(vague("Limit threads via an env variable", &[]).is_none());
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
            let i = issue("Search is broken", &format!("{VAGUE}\n{body}"));
            assert!(check(&i, activity(&["bug"])).is_none(), "{body}");
        }
        let i = issue("`quit` doesn't work while searching", VAGUE);
        assert!(check(&i, activity(&["bug"])).is_none());
    }

    #[test]
    fn insiders_replies_references_and_new_issues_are_skipped() {
        let i = issue("Search is broken", VAGUE);
        for association in [
            Association::Owner,
            Association::Member,
            Association::Collaborator,
            Association::Contributor,
        ] {
            let a = IssueActivity {
                association,
                ..activity(&["bug"])
            };
            assert!(check(&i, a).is_none());
        }
        let replied = IssueActivity {
            reporter_commented: true,
            ..activity(&["bug"])
        };
        assert!(check(&i, replied).is_none());

        let mut snap = snapshot(Some(activity(&["bug"])));
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
        assert!(needs_info(&i, &snap, ts(NOW)).is_none());

        // 13 days old is too new; 14 is old enough.
        let a = activity(&["bug"]);
        assert!(check(&i, a.clone()).is_some());
        assert!(needs_info(&i, &snapshot(Some(a.clone())), ts("2026-09-14T00:00:00Z")).is_none());
        assert!(needs_info(&i, &snapshot(Some(a)), ts("2026-09-15T00:00:00Z")).is_some());

        // No fetched activity (no token): no verdict.
        assert!(needs_info(&i, &snapshot(None), ts(NOW)).is_none());
    }
}
