//! Incremental runs: fingerprint each item and reuse a previous report's verdict when the item
//! and its related code are unchanged.
//!
//! Heuristic checks (fetched data, git, code) always re-run: they are local and cheap, and some
//! depend on the timeline or the clock. Only LLM verdicts are reused, since model calls are the cost.

use crate::fetch::Snapshot;
use crate::index;
use crate::store::{self, Item, Kind, Report, Tier, Verdict};
use std::collections::{BTreeMap, HashMap};

/// Why a previous report can't be used for this scan.
#[derive(Debug, thiserror::Error)]
pub enum Unusable {
    #[error("previous report is for {previous}, not {current}")]
    Repo { previous: String, current: String },
    #[error("previous report has {field} {previous}, this scan has {current}")]
    Changed {
        field: &'static str,
        previous: String,
        current: String,
    },
}

/// How many items were copied from the previous report and how many were checked this run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub reused: usize,
    pub rechecked: usize,
    /// Re-checked items that weren't in the previous report.
    pub new: usize,
}

/// Fill `related_files` with blob SHAs at the scanned commit (`blobs` from
/// [`crate::repo::Repo::blob_shas`]): for issues, the paths [`index::extract`] finds that exist;
/// for PRs, the changed files that exist on the branch (left out when unknown).
pub fn fill_fingerprints(
    report: &mut Report,
    snapshot: &Snapshot,
    blobs: &HashMap<String, String>,
) {
    let issues: HashMap<u64, _> = snapshot.issues.iter().map(|i| (i.number, i)).collect();
    for item in &mut report.items {
        item.fingerprint.related_files = match item.kind {
            Kind::Issue => issues.get(&item.number).map(|i| {
                let refs = index::extract(&i.title, i.body.as_deref().unwrap_or(""));
                related(refs.iter().filter_map(|r| r.path.as_deref()), blobs)
            }),
            Kind::Pull => snapshot
                .pull_activity
                .get(&item.number)
                .and_then(|a| a.changed_files.as_ref())
                .map(|files| related(files.iter().map(String::as_str), blobs)),
        };
    }
}

fn related<'a>(
    paths: impl Iterator<Item = &'a str>,
    blobs: &HashMap<String, String>,
) -> BTreeMap<String, String> {
    paths
        .filter_map(|p| Some((p.to_string(), format!("blob:{}", blobs.get(p)?))))
        .collect()
}

/// Error if `previous` is for another repository; otherwise say why it can't be used (another
/// schema, tool version, mode, or branch may have checked differently), or `Ok` if it can.
pub fn check_previous(previous: &Report, current: &Report) -> Result<(), Unusable> {
    if !previous.repo.eq_ignore_ascii_case(&current.repo) {
        return Err(Unusable::Repo {
            previous: previous.repo.clone(),
            current: current.repo.clone(),
        });
    }
    let fields = [
        (
            "schema_version",
            previous.schema_version.to_string(),
            current.schema_version.to_string(),
        ),
        (
            "tool version",
            previous.tool.version.clone(),
            current.tool.version.clone(),
        ),
        ("mode", previous.mode.clone(), current.mode.clone()),
        ("branch", previous.branch.clone(), current.branch.clone()),
    ];
    for (field, previous, current) in fields {
        if previous != current {
            return Err(Unusable::Changed {
                field,
                previous,
                current,
            });
        }
    }
    Ok(())
}

/// Whether `previous`'s verdict still holds for `current` (fingerprinted, with this run's
/// heuristics applied): the fingerprint is complete and unchanged, the heuristics found nothing,
/// and the previous verdict came from the LLM (tier `llm`, including its `cant_tell`). A
/// never-checked item (tier `none`) is not reused. The LLM check calls this before the model.
pub fn reusable(current: &Item, previous: &Item) -> bool {
    let fingerprint = &current.fingerprint;
    let complete = fingerprint.related_files.is_some()
        && match current.kind {
            Kind::Issue => fingerprint.body_hash.is_some(),
            Kind::Pull => fingerprint.head_sha.is_some(),
        };
    complete
        && *fingerprint == previous.fingerprint
        && current.verdict == Verdict::CantTell
        && previous.tier == Tier::Llm
}

/// Copy the verdict of every [`reusable`] item from `previous` (keeping its `checked_at`) and
/// recompute the summary.
pub fn reuse(report: &mut Report, previous: &Report) -> Counts {
    let old: HashMap<(Kind, u64), &Item> = previous
        .items
        .iter()
        .map(|i| ((i.kind, i.number), i))
        .collect();
    let mut counts = Counts::default();
    for item in &mut report.items {
        match old.get(&(item.kind, item.number)) {
            Some(prev) if reusable(item, prev) => {
                item.verdict = prev.verdict;
                item.confidence = prev.confidence;
                item.tier = prev.tier;
                item.evidence = prev.evidence.clone();
                item.checked_at = prev.checked_at;
                counts.reused += 1;
            }
            Some(_) => counts.rechecked += 1,
            None => {
                counts.rechecked += 1;
                counts.new += 1;
            }
        }
    }
    report.summary = store::summarize(&report.items);
    counts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::pulls::PullThresholds;
    use crate::fetch::{Issue, Mergeable, Pull, PullActivity, PullBase, PullHead};
    use crate::store::{Confidence, Evidence, EvidenceType, Fingerprint};
    use chrono::{DateTime, Utc};

    fn ts(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn snapshot() -> Snapshot {
        Snapshot {
            repo: "acme/rocketdb".into(),
            branch: "main".into(),
            head_sha: "9f1c2ab".into(),
            issues: vec![Issue {
                number: 1204,
                title: "Panic in src/compaction.rs".into(),
                html_url: "https://github.com/acme/rocketdb/issues/1204".into(),
                created_at: ts("2024-09-02T10:00:00Z"),
                body: Some("at src/compaction.rs:88 and docs/gone.md".into()),
                pull_request: None,
            }],
            pulls: vec![Pull {
                number: 2890,
                title: "Fix typo".into(),
                html_url: "https://github.com/acme/rocketdb/pull/2890".into(),
                created_at: ts("2026-01-15T09:30:00Z"),
                head: PullHead {
                    sha: "c0ffee1".into(),
                },
                base: PullBase {
                    name: "main".into(),
                },
            }],
            references: Default::default(),
            reopened_at: Default::default(),
            commits_on_branch: Default::default(),
            pull_activity: [(
                2890,
                PullActivity {
                    draft: true,
                    author: Some("alice".into()),
                    mergeable: Mergeable::Unknown,
                    head_committed_at: None,
                    checks: None,
                    reviews: vec![],
                    comments: vec![],
                    changed_files: Some(vec!["docs/a.md".into(), "docs/new.md".into()]),
                },
            )]
            .into(),
        }
    }

    fn blobs() -> HashMap<String, String> {
        [("src/compaction.rs", "7c1e"), ("docs/a.md", "aaaa")]
            .map(|(p, s)| (p.to_string(), s.to_string()))
            .into()
    }

    fn scan(snapshot: &Snapshot, blobs: &HashMap<String, String>, at: &str) -> Report {
        let mut r = store::build_report(
            snapshot,
            "basic",
            ts(at),
            &PullThresholds::default(),
            &Default::default(),
            &Default::default(),
        );
        fill_fingerprints(&mut r, snapshot, blobs);
        r
    }

    fn first_scan() -> Report {
        scan(&snapshot(), &blobs(), "2026-09-20T00:00:00Z")
    }

    fn second_scan() -> Report {
        scan(&snapshot(), &blobs(), "2026-09-27T00:00:00Z")
    }

    fn files(pairs: &[(&str, &str)]) -> Option<BTreeMap<String, String>> {
        Some(
            pairs
                .iter()
                .map(|(p, s)| (p.to_string(), s.to_string()))
                .collect(),
        )
    }

    /// Pretend the LLM checked every item: the issue is `likely_fixed`, the PR `cant_tell`.
    fn with_llm_verdicts(mut r: Report) -> Report {
        for item in &mut r.items {
            item.tier = Tier::Llm;
        }
        let issue = &mut r.items[0];
        issue.verdict = Verdict::LikelyFixed;
        issue.confidence = Confidence::Medium;
        issue.evidence = vec![Evidence {
            kind: EvidenceType::Code,
            reference: "src/compaction.rs:88".into(),
            note: "guard added".into(),
        }];
        r.summary = store::summarize(&r.items);
        r
    }

    #[test]
    fn fingerprints_hold_blobs_of_existing_related_files() {
        let r = first_scan();
        assert_eq!(
            r.items[0].fingerprint.related_files,
            files(&[("src/compaction.rs", "blob:7c1e")])
        );
        assert_eq!(
            r.items[1].fingerprint,
            Fingerprint {
                body_hash: None,
                related_files: files(&[("docs/a.md", "blob:aaaa")]),
                head_sha: Some("c0ffee1".into()),
            }
        );
    }

    #[test]
    fn pull_without_changed_files_has_no_related_files() {
        let mut snap = snapshot();
        snap.pull_activity.clear();
        let r = with_llm_verdicts(scan(&snap, &blobs(), "2026-09-20T00:00:00Z"));
        assert_eq!(r.items[1].fingerprint.related_files, None);
        let counts = reuse(&mut scan(&snap, &blobs(), "2026-09-27T00:00:00Z"), &r);
        assert_eq!(counts.reused, 1);
        assert_eq!(counts.rechecked, 1);
    }

    #[test]
    fn unchanged_llm_verdict_is_reused() {
        let previous = with_llm_verdicts(first_scan());
        let mut r = second_scan();
        let counts = reuse(&mut r, &previous);
        assert_eq!(
            counts,
            Counts {
                reused: 2,
                rechecked: 0,
                new: 0
            }
        );
        assert_eq!(r.items[0], previous.items[0]);
        assert_eq!(r.items[0].checked_at, ts("2026-09-20T00:00:00Z"));
        assert_eq!(r.summary.issues.likely_fixed, 1);
        assert_eq!(r.scanned_at, ts("2026-09-27T00:00:00Z"));
    }

    #[test]
    fn changed_body_or_file_is_rechecked() {
        let previous = with_llm_verdicts(first_scan());

        let mut snap = snapshot();
        snap.issues[0].body = Some("edited".into());
        let mut r = scan(&snap, &blobs(), "2026-09-27T00:00:00Z");
        assert_eq!(reuse(&mut r, &previous).rechecked, 1);
        assert_eq!(r.items[0].verdict, Verdict::CantTell);

        let mut changed = blobs();
        changed.insert("src/compaction.rs".into(), "8d2f".into());
        changed.insert("docs/a.md".into(), "bbbb".into());
        let mut r = scan(&snapshot(), &changed, "2026-09-27T00:00:00Z");
        assert_eq!(reuse(&mut r, &previous).rechecked, 2);
        assert_eq!(r.summary.issues.cant_tell, 1);

        let mut snap = snapshot();
        snap.pulls[0].head.sha = "d00d".into();
        let mut r = scan(&snap, &blobs(), "2026-09-27T00:00:00Z");
        let counts = reuse(&mut r, &previous);
        assert_eq!((counts.reused, counts.rechecked), (1, 1));
    }

    #[test]
    fn heuristic_and_unchecked_items_are_not_reused() {
        // Never checked (tier `none`).
        let mut r = second_scan();
        assert_eq!(reuse(&mut r, &first_scan()).reused, 0);

        // A heuristic verdict, e.g. from a closing PR since reopened, or from code.
        let mut previous = with_llm_verdicts(first_scan());
        previous.items[0].tier = Tier::Heuristic;
        let mut r = second_scan();
        assert_eq!(reuse(&mut r, &previous).rechecked, 1);
        assert_eq!(r.items[0].verdict, Verdict::CantTell);
        assert_eq!(r.items[0].checked_at, ts("2026-09-27T00:00:00Z"));

        // A verdict the heuristics give now is kept over the previous one.
        let previous = with_llm_verdicts(first_scan());
        let mut r = second_scan();
        r.items[0].verdict = Verdict::LikelyFixed;
        r.items[0].tier = Tier::Heuristic;
        assert!(!reusable(&r.items[0], &previous.items[0]));
    }

    #[test]
    fn new_items_are_counted() {
        let mut previous = with_llm_verdicts(first_scan());
        previous.items.remove(0);
        let counts = reuse(&mut second_scan(), &previous);
        assert_eq!(
            counts,
            Counts {
                reused: 1,
                rechecked: 1,
                new: 1
            }
        );
    }

    #[test]
    fn previous_report_must_match_the_scan() {
        let current = second_scan();
        assert!(check_previous(&first_scan(), &current).is_ok());
        let mut other_case = first_scan();
        other_case.repo = "Acme/RocketDB".into();
        assert!(check_previous(&other_case, &current).is_ok());

        let mut other = first_scan();
        other.repo = "acme/other".into();
        assert!(matches!(
            check_previous(&other, &current),
            Err(Unusable::Repo { .. })
        ));

        for change in [
            |r: &mut Report| r.schema_version = 0,
            |r: &mut Report| r.tool.version = "0.0.0-old".into(),
            |r: &mut Report| r.mode = "free-ai".into(),
            |r: &mut Report| r.branch = "dev".into(),
        ] {
            let mut previous = first_scan();
            change(&mut previous);
            assert!(matches!(
                check_previous(&previous, &current),
                Err(Unusable::Changed { .. })
            ));
        }
    }
}
