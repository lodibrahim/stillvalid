//! Store: the `report.json` contract (see schema/report.example.json).

use crate::check;
use crate::check::pulls::{self, PullThresholds};
use crate::fetch::Snapshot;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("could not write {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("could not serialize report: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub schema_version: u32,
    pub tool: Tool,
    pub repo: String,
    pub branch: String,
    pub head_sha: String,
    pub mode: String,
    pub scanned_at: DateTime<Utc>,
    pub summary: Summary,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    pub issues: IssueSummary,
    pub pulls: PullSummary,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct IssueSummary {
    pub open: u64,
    pub likely_fixed: u64,
    pub still_valid: u64,
    pub duplicate: u64,
    pub needs_info: u64,
    pub cant_tell: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PullSummary {
    pub open: u64,
    pub still_applies: u64,
    pub superseded: u64,
    pub conflicts: u64,
    pub abandoned: u64,
    pub ready_unreviewed: u64,
    #[serde(default)]
    pub cant_tell: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Issue,
    Pull,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    LikelyFixed,
    StillValid,
    Duplicate,
    NeedsInfo,
    StillApplies,
    Superseded,
    Conflicts,
    Abandoned,
    ReadyUnreviewed,
    CantTell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// No check has run yet.
    None,
    Heuristic,
    Llm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceType {
    Commit,
    Pull,
    Code,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    #[serde(rename = "type")]
    pub kind: EvidenceType,
    #[serde(rename = "ref")]
    pub reference: String,
    pub note: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Fingerprint {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub related_files: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub kind: Kind,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub created_at: DateTime<Utc>,
    pub verdict: Verdict,
    pub confidence: Confidence,
    pub tier: Tier,
    pub evidence: Vec<Evidence>,
    pub fingerprint: Fingerprint,
    pub checked_at: DateTime<Utc>,
}

pub fn body_hash(body: Option<&str>) -> String {
    let digest = Sha256::digest(body.unwrap_or("").as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256:{hex}")
}

/// Build a report from fetched data. Issues get `likely_fixed` when [`check::issues::likely_fixed`] finds
/// evidence; PRs with fetched activity get the activity rules; every other item is `cant_tell`.
pub fn build_report(
    snapshot: &Snapshot,
    mode: &str,
    now: DateTime<Utc>,
    thresholds: &PullThresholds,
) -> Report {
    let unchecked = |kind, number, title: &str, url: &str, created_at, fingerprint| Item {
        kind,
        number,
        title: title.to_string(),
        url: url.to_string(),
        created_at,
        verdict: Verdict::CantTell,
        confidence: Confidence::Low,
        tier: Tier::None,
        evidence: Vec::new(),
        fingerprint,
        checked_at: now,
    };

    let issues = snapshot.issues.iter().map(|i| {
        let item = unchecked(
            Kind::Issue,
            i.number,
            &i.title,
            &i.html_url,
            i.created_at,
            Fingerprint {
                body_hash: Some(body_hash(i.body.as_deref())),
                related_files: Some(BTreeMap::new()),
                head_sha: None,
            },
        );
        match check::issues::likely_fixed(i, snapshot) {
            Some(f) => Item {
                verdict: Verdict::LikelyFixed,
                confidence: f.confidence,
                tier: Tier::Heuristic,
                evidence: f.evidence,
                ..item
            },
            None => item,
        }
    });
    let pulls = snapshot.pulls.iter().map(|p| {
        let mut item = unchecked(
            Kind::Pull,
            p.number,
            &p.title,
            &p.html_url,
            p.created_at,
            Fingerprint {
                head_sha: Some(p.head.sha.clone()),
                ..Fingerprint::default()
            },
        );
        let activity = snapshot.pull_activity.get(&p.number);
        if let Some(f) = activity.and_then(|a| pulls::check(p, a, now, thresholds)) {
            item.verdict = f.verdict;
            item.confidence = f.confidence;
            item.tier = Tier::Heuristic;
            item.evidence = f.evidence;
        }
        item
    });
    let items: Vec<Item> = issues.chain(pulls).collect();

    Report {
        schema_version: SCHEMA_VERSION,
        tool: Tool {
            name: env!("CARGO_PKG_NAME").to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
        repo: snapshot.repo.clone(),
        branch: snapshot.branch.clone(),
        head_sha: snapshot.head_sha.clone(),
        mode: mode.to_string(),
        scanned_at: now,
        summary: summarize(&items),
        items,
    }
}

pub fn summarize(items: &[Item]) -> Summary {
    let mut s = Summary::default();
    for item in items {
        match item.kind {
            Kind::Issue => {
                let i = &mut s.issues;
                i.open += 1;
                match item.verdict {
                    Verdict::LikelyFixed => i.likely_fixed += 1,
                    Verdict::StillValid => i.still_valid += 1,
                    Verdict::Duplicate => i.duplicate += 1,
                    Verdict::NeedsInfo => i.needs_info += 1,
                    _ => i.cant_tell += 1,
                }
            }
            Kind::Pull => {
                let p = &mut s.pulls;
                p.open += 1;
                match item.verdict {
                    Verdict::StillApplies => p.still_applies += 1,
                    Verdict::Superseded => p.superseded += 1,
                    Verdict::Conflicts => p.conflicts += 1,
                    Verdict::Abandoned => p.abandoned += 1,
                    Verdict::ReadyUnreviewed => p.ready_unreviewed += 1,
                    _ => p.cant_tell += 1,
                }
            }
        }
    }
    s
}

pub fn write_report(report: &Report, path: &Path) -> Result<(), StoreError> {
    let mut json = serde_json::to_string_pretty(report)?;
    json.push('\n');
    std::fs::write(path, json).map_err(|source| StoreError::Io {
        path: path.display().to_string(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fetch::{CheckState, Issue, Mergeable, Pull, PullActivity, PullHead};
    use serde_json::Value;

    fn ts(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn snapshot() -> Snapshot {
        Snapshot {
            repo: "acme/rocketdb".into(),
            branch: "main".into(),
            head_sha: "9f1c2ab".into(),
            issues: vec![
                Issue {
                    number: 1204,
                    title: "Panic when compacting an empty SSTable".into(),
                    html_url: "https://github.com/acme/rocketdb/issues/1204".into(),
                    created_at: ts("2024-09-02T10:00:00Z"),
                    body: Some("It panics".into()),
                    pull_request: None,
                },
                Issue {
                    number: 1300,
                    title: "No body".into(),
                    html_url: "https://github.com/acme/rocketdb/issues/1300".into(),
                    created_at: ts("2025-01-01T00:00:00Z"),
                    body: None,
                    pull_request: None,
                },
            ],
            pulls: vec![Pull {
                number: 2890,
                title: "Fix typo in compaction docs".into(),
                html_url: "https://github.com/acme/rocketdb/pull/2890".into(),
                created_at: ts("2026-01-15T09:30:00Z"),
                head: PullHead {
                    sha: "c0ffee1".into(),
                },
            }],
            references: Default::default(),
            reopened_at: Default::default(),
            commits_on_branch: Default::default(),
            pull_activity: Default::default(),
        }
    }

    fn report(snapshot: &Snapshot) -> Report {
        build_report(
            snapshot,
            "free-ai",
            ts("2026-09-27T03:12:00Z"),
            &PullThresholds::default(),
        )
    }

    fn keys(v: &Value) -> Vec<String> {
        let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        k.sort();
        k
    }

    #[test]
    fn every_item_is_cant_tell_without_evidence() {
        let r = report(&snapshot());
        assert_eq!(r.items.len(), 3);
        for item in &r.items {
            assert_eq!(item.verdict, Verdict::CantTell);
            assert!(item.evidence.is_empty());
        }
        assert_eq!(r.summary.issues.open, 2);
        assert_eq!(r.summary.issues.cant_tell, 2);
        assert_eq!(r.summary.pulls.open, 1);
        assert_eq!(r.summary.pulls.cant_tell, 1);
    }

    #[test]
    fn issue_referenced_by_merged_pr_is_likely_fixed() {
        let mut snap = snapshot();
        snap.references.insert(
            1204,
            vec![crate::fetch::Reference::Pull(crate::fetch::PullRef {
                number: 2977,
                url: "https://github.com/acme/rocketdb/pull/2977".into(),
                merged_at: Some(ts("2026-03-11T00:00:00Z")),
                base_ref: "main".into(),
                will_close: true,
            })],
        );
        let r = build_report(
            &snap,
            "basic",
            ts("2026-09-27T03:12:00Z"),
            &PullThresholds::default(),
        );
        let item = &r.items[0];
        assert_eq!(item.verdict, Verdict::LikelyFixed);
        assert_eq!(item.confidence, Confidence::High);
        assert_eq!(item.tier, Tier::Heuristic);
        assert_eq!(item.evidence[0].reference, "#2977");
        assert_eq!(r.items[1].verdict, Verdict::CantTell);
        assert_eq!(r.summary.issues.likely_fixed, 1);
        assert_eq!(r.summary.issues.cant_tell, 1);
    }

    #[test]
    fn pull_with_activity_gets_a_heuristic_verdict() {
        let mut snap = snapshot();
        snap.pull_activity.insert(
            2890,
            PullActivity {
                draft: false,
                author: Some("alice".into()),
                mergeable: Mergeable::Mergeable,
                head_committed_at: None,
                checks: Some(CheckState::Success),
                reviews: vec![],
                comments: vec![],
            },
        );
        let r = report(&snap);
        let pr = r.items.iter().find(|i| i.kind == Kind::Pull).unwrap();
        assert_eq!(pr.verdict, Verdict::ReadyUnreviewed);
        assert_eq!(pr.tier, Tier::Heuristic);
        assert_eq!(pr.evidence[0].reference, "c0ffee1");
        assert_eq!(r.summary.pulls.ready_unreviewed, 1);
        assert_eq!(r.summary.pulls.cant_tell, 0);
        assert_eq!(r.summary.issues.cant_tell, 2);
    }

    #[test]
    fn body_hash_is_sha256_of_body() {
        assert_eq!(
            body_hash(None),
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(body_hash(Some("")), body_hash(None));
        assert_ne!(body_hash(Some("a")), body_hash(Some("b")));
    }

    #[test]
    fn shape_matches_schema_example() {
        let example: Value =
            serde_json::from_str(include_str!("../schema/report.example.json")).unwrap();
        let ours = serde_json::to_value(report(&snapshot())).unwrap();

        assert_eq!(keys(&ours), keys(&example));
        assert_eq!(keys(&ours["tool"]), keys(&example["tool"]));
        assert_eq!(ours["schema_version"], example["schema_version"]);
        assert_eq!(ours["scanned_at"], example["scanned_at"]);

        // Our summary is a superset: pulls also count `cant_tell`.
        for section in ["issues", "pulls"] {
            for k in keys(&example["summary"][section]) {
                assert!(
                    ours["summary"][section].get(&k).is_some(),
                    "missing summary.{section}.{k}"
                );
            }
        }

        let (ex_issue, ex_pull) = (&example["items"][0], &example["items"][1]);
        let (our_issue, our_pull) = (&ours["items"][0], &ours["items"][2]);
        assert_eq!(keys(our_issue), keys(ex_issue));
        assert_eq!(keys(our_pull), keys(ex_pull));
        assert_eq!(
            keys(&our_issue["fingerprint"]),
            keys(&ex_issue["fingerprint"])
        );
        assert_eq!(
            keys(&our_pull["fingerprint"]),
            keys(&ex_pull["fingerprint"])
        );
        assert_eq!(our_issue["kind"], "issue");
        assert_eq!(our_pull["kind"], "pull");
        assert_eq!(our_issue["verdict"], "cant_tell");
        assert_eq!(our_pull["fingerprint"]["head_sha"], "c0ffee1");
    }

    #[test]
    fn example_report_deserializes() {
        let r: Report =
            serde_json::from_str(include_str!("../schema/report.example.json")).unwrap();
        assert_eq!(r.items[0].evidence[1].reference, "src/compaction.rs:88");
        assert_eq!(r.items[1].verdict, Verdict::Superseded);
    }

    #[test]
    fn write_report_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.json");
        let r = report(&snapshot());
        write_report(&r, &path).unwrap();
        let back: Report = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(back, r);
    }
}
