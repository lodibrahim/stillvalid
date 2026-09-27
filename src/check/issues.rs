//! Issue checks.

use super::Finding;
use crate::fetch::{Issue, Reference, Snapshot};
use crate::store::{Confidence, Evidence, EvidenceType};

/// An issue is likely fixed when something that says it fixes the issue landed on the scanned
/// branch and the issue was not reopened afterwards: a same-repo PR that will close it, merged
/// into the branch after the issue was filed (`high`), or a commit on the branch whose message
/// closes it (`medium`, see [`Reference::Commit`]). Plain mentions don't count.
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
                will_close,
                referenced_at,
                ..
            } => {
                if !will_close
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

    fn commit(oid: &str, will_close: bool, referenced_at: &str) -> Reference {
        Reference::Commit {
            oid: oid.into(),
            url: format!("https://github.com/o/r/commit/{oid}"),
            will_close,
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
        let mut snap = snapshot(vec![commit(oid, true, "2026-02-01T00:00:00Z")]);
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
        let mut snap = snapshot(vec![commit(oid, false, "2026-02-01T00:00:00Z")]);
        snap.commits_on_branch.insert(oid.into());
        assert_eq!(likely_fixed(&issue(), &snap), None);
    }

    #[test]
    fn commit_not_on_branch_is_ignored() {
        let snap = snapshot(vec![commit("0123456789", true, "2026-02-01T00:00:00Z")]);
        assert_eq!(likely_fixed(&issue(), &snap), None);
    }

    #[test]
    fn commit_referenced_before_reopen_is_ignored() {
        let oid = "0123456789";
        let mut snap = snapshot(vec![commit(oid, true, "2026-02-01T00:00:00Z")]);
        snap.commits_on_branch.insert(oid.into());
        snap.reopened_at.insert(7, ts("2026-03-01T00:00:00Z"));
        assert_eq!(likely_fixed(&issue(), &snap), None);
    }

    #[test]
    fn all_qualifying_refs_are_evidence_and_highest_confidence_wins() {
        let oid = "fedcba9876543210";
        let mut snap = snapshot(vec![
            commit(oid, true, "2026-02-01T00:00:00Z"),
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
    fn issue_without_references_has_no_finding() {
        let mut snap = snapshot(Vec::new());
        snap.references.clear();
        assert_eq!(likely_fixed(&issue(), &snap), None);
    }
}
