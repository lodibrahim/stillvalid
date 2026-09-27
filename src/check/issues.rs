//! Issue checks.

use super::Finding;
use crate::fetch::{Issue, Reference, Snapshot};
use crate::store::{Confidence, Evidence, EvidenceType};

/// GitHub's closing keywords.
const CLOSING_KEYWORDS: [&str; 9] = [
    "close", "closes", "closed", "fix", "fixes", "fixed", "resolve", "resolves", "resolved",
];

/// An issue is likely fixed when something that says it fixes the issue landed on the scanned
/// branch and the issue was not reopened afterwards: a same-repo PR that will close it, merged
/// into the branch after the issue was filed (`high`), or a commit on the branch whose message
/// closes it (`medium`). Plain mentions don't count.
pub fn likely_fixed(issue: &Issue, snapshot: &Snapshot) -> Option<Finding> {
    let refs = snapshot.references.get(&issue.number)?;
    let reopened_at = snapshot.reopened_at.get(&issue.number);
    let branch = &snapshot.branch;

    let mut confidence = Confidence::Medium;
    let mut evidence = Vec::new();
    for r in refs {
        match r {
            Reference::Pull(pr) => {
                let Some(merged_at) = pr.merged_at else {
                    continue;
                };
                if !pr.will_close
                    || pr.base_ref != *branch
                    || merged_at <= issue.created_at
                    || reopened_at.is_some_and(|at| *at > merged_at)
                {
                    continue;
                }
                confidence = Confidence::High;
                evidence.push(Evidence {
                    kind: EvidenceType::Pull,
                    reference: format!("#{}", pr.number),
                    note: format!(
                        "Merged {} into {branch}; says it fixes this issue",
                        merged_at.format("%Y-%m-%d")
                    ),
                });
            }
            Reference::Commit {
                oid,
                message,
                referenced_at,
                ..
            } => {
                if !closes_issue(message, &snapshot.repo, issue.number)
                    || !snapshot.commits_on_branch.contains(oid)
                    || reopened_at.is_some_and(|at| at > referenced_at)
                {
                    continue;
                }
                evidence.push(Evidence {
                    kind: EvidenceType::Commit,
                    reference: oid.chars().take(7).collect(),
                    note: format!("On {branch}; says it fixes this issue"),
                });
            }
        }
    }

    (!evidence.is_empty()).then_some(Finding {
        confidence,
        evidence,
    })
}

/// Whether a commit message closes issue `number` of `repo` with a closing keyword, as GitHub
/// parses it: `Fixes #12`, `closes: owner/repo#12`, `Resolved https://github.com/owner/repo/issues/12`.
/// Case-insensitive; the keyword must start a word and be followed by spaces or tabs.
pub fn closes_issue(message: &str, repo: &str, number: u64) -> bool {
    let text = message.to_ascii_lowercase();
    let repo = repo.to_ascii_lowercase();
    let prefixes = [
        "#".to_string(),
        format!("{repo}#"),
        format!("https://github.com/{repo}/issues/"),
    ];
    text.char_indices().any(|(i, _)| {
        let word_start = text[..i]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric());
        word_start
            && CLOSING_KEYWORDS.iter().any(|kw| {
                let Some(rest) = text[i..].strip_prefix(kw) else {
                    return false;
                };
                let rest = rest.strip_prefix(':').unwrap_or(rest);
                let target = rest.trim_start_matches([' ', '\t']);
                target.len() < rest.len()
                    && prefixes.iter().any(|p| {
                        target.strip_prefix(p.as_str()).is_some_and(|n| {
                            let end = n.find(|c: char| !c.is_ascii_digit()).unwrap_or(n.len());
                            n[..end].parse() == Ok(number)
                        })
                    })
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fetch::PullRef;
    use chrono::{DateTime, Utc};

    fn ts(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn issue() -> Issue {
        Issue {
            number: 7,
            title: "Crash on empty input".into(),
            html_url: "https://github.com/o/r/issues/7".into(),
            created_at: ts("2026-01-01T00:00:00Z"),
            body: None,
            pull_request: None,
        }
    }

    fn pr(number: u64, merged_at: Option<&str>, base_ref: &str, will_close: bool) -> Reference {
        Reference::Pull(PullRef {
            number,
            url: format!("https://github.com/o/r/pull/{number}"),
            merged_at: merged_at.map(ts),
            base_ref: base_ref.into(),
            will_close,
        })
    }

    fn commit(oid: &str, message: &str, referenced_at: &str) -> Reference {
        Reference::Commit {
            oid: oid.into(),
            url: format!("https://github.com/o/r/commit/{oid}"),
            message: message.into(),
            referenced_at: ts(referenced_at),
        }
    }

    fn snapshot(refs: Vec<Reference>) -> Snapshot {
        Snapshot {
            repo: "o/r".into(),
            branch: "main".into(),
            head_sha: "abc".into(),
            issues: vec![issue()],
            pulls: Vec::new(),
            references: [(7, refs)].into(),
            reopened_at: Default::default(),
            commits_on_branch: Default::default(),
        }
    }

    const MERGED: Option<&str> = Some("2026-03-11T12:00:00Z");

    #[test]
    fn merged_pr_that_fixes_the_issue_is_high() {
        let f = likely_fixed(&issue(), &snapshot(vec![pr(1500, MERGED, "main", true)])).unwrap();
        assert_eq!(f.confidence, Confidence::High);
        assert_eq!(
            f.evidence,
            [Evidence {
                kind: EvidenceType::Pull,
                reference: "#1500".into(),
                note: "Merged 2026-03-11 into main; says it fixes this issue".into(),
            }]
        );
    }

    #[test]
    fn merged_pr_that_only_mentions_the_issue_is_ignored() {
        assert_eq!(
            likely_fixed(&issue(), &snapshot(vec![pr(1500, MERGED, "main", false)])),
            None
        );
    }

    #[test]
    fn pr_merged_into_another_branch_is_ignored() {
        assert_eq!(
            likely_fixed(&issue(), &snapshot(vec![pr(1500, MERGED, "dev", true)])),
            None
        );
    }

    #[test]
    fn pr_merged_before_the_issue_was_filed_is_ignored() {
        let early = Some("2025-12-31T00:00:00Z");
        assert_eq!(
            likely_fixed(&issue(), &snapshot(vec![pr(1500, early, "main", true)])),
            None
        );
    }

    #[test]
    fn unmerged_pr_is_ignored() {
        assert_eq!(
            likely_fixed(&issue(), &snapshot(vec![pr(1500, None, "main", true)])),
            None
        );
    }

    #[test]
    fn reopened_after_merge_is_ignored_but_reopen_before_merge_is_not() {
        let mut snap = snapshot(vec![pr(1500, MERGED, "main", true)]);
        snap.reopened_at.insert(7, ts("2026-04-01T00:00:00Z"));
        assert_eq!(likely_fixed(&issue(), &snap), None);

        snap.reopened_at.insert(7, ts("2026-02-01T00:00:00Z"));
        assert!(likely_fixed(&issue(), &snap).is_some());
    }

    #[test]
    fn closing_commit_on_branch_is_medium() {
        let oid = "0123456789abcdef";
        let mut snap = snapshot(vec![commit(
            oid,
            "Handle empty input\n\nFixes #7",
            "2026-02-01T00:00:00Z",
        )]);
        snap.commits_on_branch.insert(oid.into());
        let f = likely_fixed(&issue(), &snap).unwrap();
        assert_eq!(f.confidence, Confidence::Medium);
        assert_eq!(
            f.evidence,
            [Evidence {
                kind: EvidenceType::Commit,
                reference: "0123456".into(),
                note: "On main; says it fixes this issue".into(),
            }]
        );
    }

    #[test]
    fn commit_that_only_mentions_the_issue_is_ignored() {
        let oid = "0123456789";
        let mut snap = snapshot(vec![commit(oid, "See also #7", "2026-02-01T00:00:00Z")]);
        snap.commits_on_branch.insert(oid.into());
        assert_eq!(likely_fixed(&issue(), &snap), None);
    }

    #[test]
    fn commit_not_on_branch_is_ignored() {
        let snap = snapshot(vec![commit(
            "0123456789",
            "Fixes #7",
            "2026-02-01T00:00:00Z",
        )]);
        assert_eq!(likely_fixed(&issue(), &snap), None);
    }

    #[test]
    fn commit_referenced_before_reopen_is_ignored() {
        let oid = "0123456789";
        let mut snap = snapshot(vec![commit(oid, "Fixes #7", "2026-02-01T00:00:00Z")]);
        snap.commits_on_branch.insert(oid.into());
        snap.reopened_at.insert(7, ts("2026-03-01T00:00:00Z"));
        assert_eq!(likely_fixed(&issue(), &snap), None);
    }

    #[test]
    fn all_qualifying_refs_are_evidence_and_highest_confidence_wins() {
        let oid = "fedcba9876543210";
        let mut snap = snapshot(vec![
            commit(oid, "closes #7", "2026-02-01T00:00:00Z"),
            pr(1400, None, "main", true),
            pr(1500, MERGED, "main", true),
        ]);
        snap.commits_on_branch.insert(oid.into());
        let f = likely_fixed(&issue(), &snap).unwrap();
        assert_eq!(f.confidence, Confidence::High);
        let refs: Vec<&str> = f.evidence.iter().map(|e| e.reference.as_str()).collect();
        assert_eq!(refs, ["fedcba9", "#1500"]);
    }

    #[test]
    fn closing_keywords_match_like_github() {
        let closes = |msg: &str| closes_issue(msg, "Owner/Repo", 12);
        for yes in [
            "Fixes #12",
            "fix #12",
            "FIXED #12.",
            "closes: #12",
            "Close #12, #13",
            "Resolved\t#12",
            "resolves owner/repo#12",
            "Resolve https://github.com/OWNER/repo/issues/12",
            "Refactor parser\n\nfixes #12",
            // GitHub's parser accepts this too; not special-cased.
            "This does not fix #12",
        ] {
            assert!(closes(yes), "{yes:?} should close #12");
        }
        for no in [
            "See #12",
            "hotfix #12",
            "prefix #12",
            "fixing #12",
            "fixes #123",
            "fixes #1",
            "fixes #13, #12",
            "fixes\n#12",
            "fixes other/repo#12",
            "fixes https://github.com/other/repo/issues/12",
            "fixes#12",
        ] {
            assert!(!closes(no), "{no:?} should not close #12");
        }
    }

    #[test]
    fn issue_without_references_has_no_finding() {
        let mut snap = snapshot(Vec::new());
        snap.references.clear();
        assert_eq!(likely_fixed(&issue(), &snap), None);
    }
}
