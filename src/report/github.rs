//! GitHub outputs: one `stillvalid: <verdict>` label per item and the "Backlog health" summary
//! issue (docs/DESIGN.md §6, items 1 and 2).
//!
//! The only module that writes to GitHub, and only for `scan --labels` / `--summary-issue`.
//! Changes are diffed against what GitHub already has, so a run where nothing changed makes no
//! writes. Only labels starting with `stillvalid: ` and the one summary issue are ever touched.

use crate::fetch::{Issue, Snapshot};
use crate::report::html::{self, GITHUB};
use crate::store::{Kind, Report, Verdict};
use octocrab::{Octocrab, Page};
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeMap;
use std::fmt::Write;
use std::time::Duration;

pub const LABEL_PREFIX: &str = "stillvalid: ";
pub const SUMMARY_TITLE: &str = "Backlog health";
/// Hidden in the summary issue's body; only an issue carrying it is ever edited.
pub const SUMMARY_MARKER: &str = "<!-- stillvalid:backlog-health -->";
/// How many likely-fixed issues the summary lists.
const TOP_LIKELY_FIXED: usize = 20;
/// Pause before each write, below GitHub's content-creation limit (~80/min).
const WRITE_PAUSE: Duration = Duration::from_secs(1);

#[derive(Debug, thiserror::Error)]
pub enum GithubError {
    #[error("not allowed (needs a token with issues: write) or rate limited: {0}")]
    Forbidden(octocrab::Error),
    #[error("GitHub API request failed: {0}")]
    Api(octocrab::Error),
}

impl From<octocrab::Error> for GithubError {
    fn from(e: octocrab::Error) -> Self {
        match status(&e) {
            Some(403 | 429) => Self::Forbidden(e),
            _ => Self::Api(e),
        }
    }
}

fn status(e: &octocrab::Error) -> Option<u16> {
    match e {
        octocrab::Error::GitHub { source, .. } => Some(source.status_code.as_u16()),
        _ => None,
    }
}

/// `stillvalid: likely-fixed`; `cant_tell` gets no label.
pub fn label_name(v: Verdict) -> Option<String> {
    (v != Verdict::CantTell).then(|| format!("{LABEL_PREFIX}{}", html::key(v).replace('_', "-")))
}

/// Color and description for a label this tool creates.
fn label_style(v: Verdict) -> (&'static str, &'static str) {
    match v {
        Verdict::LikelyFixed => (
            "0e8a16",
            "stillvalid: code or history shows this was likely addressed",
        ),
        Verdict::StillValid => (
            "d93f0b",
            "stillvalid: the described behavior is still possible in current code",
        ),
        Verdict::Duplicate => ("cfd3d7", "stillvalid: same problem as another issue"),
        Verdict::NeedsInfo => (
            "fbca04",
            "stillvalid: not enough information to check against the code",
        ),
        Verdict::StillApplies => ("1d76db", "stillvalid: merges cleanly and is still relevant"),
        Verdict::Superseded => (
            "0e8a16",
            "stillvalid: its change already exists on the branch",
        ),
        Verdict::Conflicts => ("b60205", "stillvalid: conflicts with the current branch"),
        Verdict::Abandoned => (
            "6a737d",
            "stillvalid: no author activity, failing checks or conflicts",
        ),
        Verdict::ReadyUnreviewed => ("1d76db", "stillvalid: checks pass, no review yet"),
        Verdict::CantTell => ("ededed", "stillvalid: no conclusion"),
    }
}

fn is_ours(label: &str) -> bool {
    label
        .get(..LABEL_PREFIX.len())
        .is_some_and(|p| p.eq_ignore_ascii_case(LABEL_PREFIX))
}

/// One item's label edit: add the verdict's label and/or remove stale stillvalid labels.
#[derive(Debug, Clone, PartialEq)]
pub struct LabelChange {
    pub number: u64,
    pub add: Option<Verdict>,
    pub remove: Vec<String>,
}

impl LabelChange {
    /// "add `stillvalid: likely-fixed` to #12, remove `stillvalid: conflicts`"
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if let Some(name) = self.add.and_then(label_name) {
            parts.push(format!("add `{name}` to #{}", self.number));
        }
        for name in &self.remove {
            parts.push(format!("remove `{name}` from #{}", self.number));
        }
        parts.join(", ")
    }
}

/// The label edits that bring every item's stillvalid label in line with its verdict, given the
/// labels the snapshot fetched. Items already right are left out.
pub fn plan_labels(report: &Report, snapshot: &Snapshot) -> Vec<LabelChange> {
    let current: BTreeMap<u64, Vec<&str>> = snapshot
        .issues
        .iter()
        .map(|i| (i.number, &i.labels))
        .chain(snapshot.pulls.iter().map(|p| (p.number, &p.labels)))
        .map(|(n, labels)| {
            let ours = labels
                .iter()
                .map(|l| l.name.as_str())
                .filter(|l| is_ours(l));
            (n, ours.collect())
        })
        .collect();
    report
        .items
        .iter()
        .filter_map(|item| {
            let want = label_name(item.verdict);
            let have = current
                .get(&item.number)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let matches = |l: &&str| want.as_ref().is_some_and(|w| l.eq_ignore_ascii_case(w));
            let add = want.is_some() && !have.iter().any(matches);
            let change = LabelChange {
                number: item.number,
                add: add.then_some(item.verdict),
                remove: have
                    .iter()
                    .filter(|l| !matches(l))
                    .map(|l| l.to_string())
                    .collect(),
            };
            (change.add.is_some() || !change.remove.is_empty()).then_some(change)
        })
        .collect()
}

/// Verdicts whose label the changes add but the repo doesn't have yet (names compare
/// case-insensitively, as GitHub does).
pub fn missing_labels(changes: &[LabelChange], existing: &[String]) -> Vec<Verdict> {
    let mut missing = Vec::new();
    for v in changes.iter().filter_map(|c| c.add) {
        let name = label_name(v).expect("only labeled verdicts are added");
        if !missing.contains(&v) && !existing.iter().any(|e| e.eq_ignore_ascii_case(&name)) {
            missing.push(v);
        }
    }
    missing
}

/// The open issue this tool created as the summary: exact title and the hidden marker. The
/// oldest wins if there are several.
pub fn find_summary(open: &[Issue]) -> Option<&Issue> {
    open.iter()
        .filter(|i| is_summary(&i.title, i.body.as_deref()))
        .min_by_key(|i| i.number)
}

/// Remove the summary issue(s) from a fresh snapshot, so they are never checked, counted, or
/// labeled: the summary's own body quotes evidence the checks would otherwise pick up.
pub fn drop_summary(snapshot: &mut Snapshot) {
    snapshot
        .issues
        .retain(|i| !is_summary(&i.title, i.body.as_deref()));
}

fn is_summary(title: &str, body: Option<&str>) -> bool {
    title == SUMMARY_TITLE && body.is_some_and(|b| b.contains(SUMMARY_MARKER))
}

/// The summary issue's Markdown body. It has no timestamp, so it only changes when the results do.
pub fn summary_body(report: &Report, dashboard_url: Option<&str>) -> String {
    let base = format!("{GITHUB}{}", html::encode_path(&report.repo));
    let mut out = format!("{SUMMARY_MARKER}\n");
    let _ = write!(
        out,
        "stillvalid {} checked `{}` at [`{}`]({base}/commit/{}) in {} mode.",
        md(&report.tool.version),
        report.branch.replace('`', ""),
        crate::check::pulls::short_sha(&report.head_sha),
        html::encode_path(&report.head_sha),
        md(&report.mode),
    );
    if let Some(url) = dashboard_url {
        let _ = write!(out, " [Open the dashboard]({url}).");
    }
    out.push_str("\n\n");

    for (kind, heading, verdicts, open) in [
        (
            Kind::Issue,
            "Issues",
            html::ISSUE_VERDICTS,
            report.summary.issues.open,
        ),
        (
            Kind::Pull,
            "Pull requests",
            html::PULL_VERDICTS,
            report.summary.pulls.open,
        ),
    ] {
        let _ = writeln!(out, "### {heading} ({open} open)\n");
        let rows: Vec<(Verdict, usize)> = verdicts
            .iter()
            .map(|&v| {
                let n = report
                    .items
                    .iter()
                    .filter(|i| i.kind == kind && i.verdict == v)
                    .count();
                (v, n)
            })
            .filter(|&(_, n)| n > 0)
            .collect();
        if rows.is_empty() {
            out.push_str("None.\n\n");
            continue;
        }
        out.push_str("| Verdict | Count |\n|---|---:|\n");
        for (v, n) in rows {
            let _ = writeln!(out, "| {} | {n} |", html::label(v));
        }
        out.push('\n');
    }

    let mut fixed: Vec<_> = report
        .items
        .iter()
        .filter(|i| i.kind == Kind::Issue && i.verdict == Verdict::LikelyFixed)
        .collect();
    fixed.sort_by_key(|i| (i.confidence, i.created_at, i.number));
    if !fixed.is_empty() {
        out.push_str("### Likely fixed\n\n");
        for item in fixed.iter().take(TOP_LIKELY_FIXED) {
            let _ = write!(
                out,
                "- #{} {} ({} confidence)",
                item.number,
                md(&item.title),
                html::confidence_name(item.confidence),
            );
            if let Some(e) = item.evidence.first() {
                let reference = format!("`{}`", e.reference.replace('`', ""));
                let shown = match html::evidence_href(report, &base, e) {
                    Some(href) => format!("[{reference}]({href})"),
                    None => reference,
                };
                let _ = write!(out, ": {shown}");
                if !e.note.is_empty() {
                    let _ = write!(out, " {}", md(&e.note));
                }
            }
            out.push('\n');
        }
        if fixed.len() > TOP_LIKELY_FIXED {
            let _ = writeln!(out, "- and {} more", fixed.len() - TOP_LIKELY_FIXED);
        }
        out.push('\n');
    }
    out.push_str(
        "This issue is rewritten by stillvalid on each run; edits are overwritten. \
         stillvalid never closes, locks, or edits other issues.\n",
    );
    out
}

/// Untrusted text as inline Markdown: one line, punctuation escaped, and `@` kept from mentioning.
fn md(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '|' | '~' | '#' | '!' => {
                out.push('\\');
                out.push(c);
            }
            // A word joiner after `@` stops GitHub from treating it as a mention.
            '@' => out.push_str("@\u{2060}"),
            '\r' | '\n' => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// What happened to the summary issue.
#[derive(Debug)]
pub enum SummaryOutcome {
    Unchanged(u64),
    Updated(u64),
    /// Created, and whether pinning it worked.
    Created {
        number: u64,
        pinned: Result<(), GithubError>,
    },
    /// A maintainer closed it; it is left alone and not recreated.
    Closed(u64),
    WouldUpdate(u64),
    WouldCreate,
}

/// Counts from [`Writer::apply_labels`]; items whose edit failed for a reason other than
/// permission are skipped.
#[derive(Debug, Default)]
pub struct LabelsApplied {
    pub created: usize,
    pub changed: usize,
    pub failed: Vec<(u64, GithubError)>,
}

#[derive(Deserialize)]
struct RepoLabel {
    name: String,
}

#[derive(Deserialize)]
struct SearchResults {
    items: Vec<SearchIssue>,
}

#[derive(Deserialize)]
struct SearchIssue {
    number: u64,
    title: String,
    body: Option<String>,
}

#[derive(Deserialize)]
struct Created {
    number: u64,
    node_id: String,
}

pub struct Writer {
    gh: Octocrab,
    /// `owner/name`.
    repo: String,
    pause: Duration,
}

impl Writer {
    pub fn new(gh: Octocrab, repo: &str) -> Self {
        Self {
            gh,
            repo: repo.to_string(),
            pause: WRITE_PAUSE,
        }
    }

    /// Names of every label in the repo.
    pub async fn repo_labels(&self) -> Result<Vec<String>, GithubError> {
        let route = format!("/repos/{}/labels", self.repo);
        let first: Page<RepoLabel> = self.gh.get(route, Some(&[("per_page", "100")])).await?;
        let labels = self.gh.all_pages(first).await?;
        Ok(labels.into_iter().map(|l| l.name).collect())
    }

    /// Create the `missing` labels, then apply each change. A 403/429 stops everything; any other
    /// failure skips that item.
    pub async fn apply_labels(
        &self,
        changes: &[LabelChange],
        missing: &[Verdict],
    ) -> Result<LabelsApplied, GithubError> {
        let mut done = LabelsApplied::default();
        for &v in missing {
            let (color, description) = label_style(v);
            let body = json!({ "name": label_name(v), "color": color, "description": description });
            match self
                .post(format!("/repos/{}/labels", self.repo), &body)
                .await
            {
                Ok(()) => done.created += 1,
                // 422: it exists after all (created since we listed); adding it still works.
                Err(GithubError::Api(e)) if status(&e) == Some(422) => {}
                Err(e) => return Err(e),
            }
        }
        for change in changes {
            match self.change(change).await {
                Ok(()) => done.changed += 1,
                Err(e @ GithubError::Forbidden(_)) => return Err(e),
                Err(e) => done.failed.push((change.number, e)),
            }
        }
        Ok(done)
    }

    async fn change(&self, change: &LabelChange) -> Result<(), GithubError> {
        let route = format!("/repos/{}/issues/{}/labels", self.repo, change.number);
        for name in &change.remove {
            tokio::time::sleep(self.pause).await;
            let url = format!("{route}/{}", html::encode_path(name).replace('/', "%2F"));
            match self.gh.delete::<serde_json::Value, _, ()>(url, None).await {
                // 404: already gone.
                Err(e) if status(&e) != Some(404) => return Err(e.into()),
                _ => {}
            }
        }
        if let Some(name) = change.add.and_then(label_name) {
            self.post(route, &json!({ "labels": [name] })).await?;
        }
        Ok(())
    }

    /// Update the open summary issue if its body changed; else, unless a maintainer closed one,
    /// create it and try to pin it. `dry_run` only reads.
    pub async fn sync_summary(
        &self,
        open: &[Issue],
        body: &str,
        dry_run: bool,
    ) -> Result<SummaryOutcome, GithubError> {
        if let Some(issue) = find_summary(open) {
            let number = issue.number;
            if issue.body.as_deref() == Some(body) {
                return Ok(SummaryOutcome::Unchanged(number));
            }
            if dry_run {
                return Ok(SummaryOutcome::WouldUpdate(number));
            }
            tokio::time::sleep(self.pause).await;
            let route = format!("/repos/{}/issues/{number}", self.repo);
            let _: serde_json::Value = self.gh.patch(route, Some(&json!({ "body": body }))).await?;
            return Ok(SummaryOutcome::Updated(number));
        }

        let q = format!(
            "repo:{} is:issue is:closed in:title \"{SUMMARY_TITLE}\"",
            self.repo
        );
        let found: SearchResults = self
            .gh
            .get(
                "/search/issues",
                Some(&[("q", q.as_str()), ("per_page", "100")]),
            )
            .await?;
        if let Some(closed) = found
            .items
            .iter()
            .filter(|i| is_summary(&i.title, i.body.as_deref()))
            .min_by_key(|i| i.number)
        {
            return Ok(SummaryOutcome::Closed(closed.number));
        }
        if dry_run {
            return Ok(SummaryOutcome::WouldCreate);
        }

        tokio::time::sleep(self.pause).await;
        let route = format!("/repos/{}/issues", self.repo);
        let created: Created = self
            .gh
            .post(
                route,
                Some(&json!({ "title": SUMMARY_TITLE, "body": body })),
            )
            .await?;
        let pinned = self.pin(&created.node_id).await;
        Ok(SummaryOutcome::Created {
            number: created.number,
            pinned,
        })
    }

    /// Best effort: fails when the repo already has three pinned issues.
    async fn pin(&self, node_id: &str) -> Result<(), GithubError> {
        let payload = json!({
            "query": "mutation($id: ID!) { pinIssue(input: {issueId: $id}) { issue { number } } }",
            "variables": { "id": node_id },
        });
        let _: serde_json::Value = self.gh.graphql(&payload).await?;
        Ok(())
    }

    async fn post(&self, route: String, body: &serde_json::Value) -> Result<(), GithubError> {
        tokio::time::sleep(self.pause).await;
        let _: serde_json::Value = self.gh.post(route, Some(body)).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fetch::{Label, Pull, PullBase, PullHead};
    use crate::store::Confidence;
    use wiremock::matchers::{body_json, body_partial_json, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// The schema example: issue #1204 `likely_fixed` (high), PR #2890 `superseded`.
    fn example() -> Report {
        serde_json::from_str(include_str!("../../schema/report.example.json")).unwrap()
    }

    fn labels(names: &[&str]) -> Vec<Label> {
        names
            .iter()
            .map(|n| Label {
                name: n.to_string(),
            })
            .collect()
    }

    fn issue(number: u64, title: &str, body: Option<&str>, names: &[&str]) -> Issue {
        Issue {
            number,
            title: title.into(),
            body: body.map(Into::into),
            labels: labels(names),
            ..Default::default()
        }
    }

    fn snapshot(issue_labels: &[&str], pull_labels: &[&str]) -> Snapshot {
        Snapshot {
            repo: "acme/rocketdb".into(),
            branch: "main".into(),
            head_sha: "9f1c2ab".into(),
            issues: vec![issue(1204, "Panic", Some("It panics"), issue_labels)],
            pulls: vec![Pull {
                number: 2890,
                title: "Fix typo".into(),
                html_url: String::new(),
                created_at: "2026-01-15T09:30:00Z".parse().unwrap(),
                head: PullHead {
                    sha: "c0ffee1".into(),
                },
                base: PullBase {
                    name: "main".into(),
                },
                labels: labels(pull_labels),
            }],
            references: Default::default(),
            reopened_at: Default::default(),
            commits_on_branch: Default::default(),
            pull_activity: Default::default(),
        }
    }

    fn writer(server: &MockServer) -> Writer {
        let gh = Octocrab::builder()
            .base_uri(server.uri())
            .unwrap()
            .build()
            .unwrap();
        Writer {
            pause: Duration::ZERO,
            ..Writer::new(gh, "acme/rocketdb")
        }
    }

    fn error(status: u16) -> ResponseTemplate {
        ResponseTemplate::new(status).set_body_json(json!({ "message": "nope" }))
    }

    #[test]
    fn label_names_follow_report_verdicts() {
        assert_eq!(
            label_name(Verdict::LikelyFixed).as_deref(),
            Some("stillvalid: likely-fixed")
        );
        assert_eq!(
            label_name(Verdict::ReadyUnreviewed).as_deref(),
            Some("stillvalid: ready-unreviewed")
        );
        assert_eq!(label_name(Verdict::CantTell), None);
        for v in html::ISSUE_VERDICTS.iter().chain(html::PULL_VERDICTS) {
            assert!(
                label_style(*v).1.len() <= 100,
                "GitHub limits descriptions to 100"
            );
        }
    }

    #[test]
    fn plans_only_what_changed_and_leaves_other_labels_alone() {
        let report = example();
        let changes = plan_labels(&report, &snapshot(&["bug"], &["stillvalid: conflicts"]));
        assert_eq!(
            changes,
            [
                LabelChange {
                    number: 1204,
                    add: Some(Verdict::LikelyFixed),
                    remove: vec![],
                },
                LabelChange {
                    number: 2890,
                    add: Some(Verdict::Superseded),
                    remove: vec!["stillvalid: conflicts".into()],
                },
            ]
        );
        assert_eq!(
            changes[1].describe(),
            "add `stillvalid: superseded` to #2890, remove `stillvalid: conflicts` from #2890"
        );

        // Already labeled (any case): nothing to do.
        let done = snapshot(
            &["bug", "StillValid: Likely-Fixed"],
            &["stillvalid: superseded"],
        );
        assert!(plan_labels(&report, &done).is_empty());
    }

    #[test]
    fn cant_tell_loses_its_label() {
        let mut report = example();
        report.items.retain(|i| i.kind == Kind::Pull);
        report.items[0].verdict = Verdict::CantTell;
        let snap = snapshot(&[], &["stillvalid: superseded", "stillvalid: abandoned"]);
        let changes = plan_labels(&report, &snap);
        assert_eq!(
            changes,
            [LabelChange {
                number: 2890,
                add: None,
                remove: vec![
                    "stillvalid: superseded".into(),
                    "stillvalid: abandoned".into()
                ],
            }]
        );
    }

    #[test]
    fn missing_labels_are_deduplicated_and_case_insensitive() {
        let add = |number, v| LabelChange {
            number,
            add: Some(v),
            remove: vec![],
        };
        let changes = [
            add(1, Verdict::LikelyFixed),
            add(2, Verdict::LikelyFixed),
            add(3, Verdict::Conflicts),
        ];
        assert_eq!(
            missing_labels(&changes, &["Stillvalid: Conflicts".into()]),
            [Verdict::LikelyFixed]
        );
    }

    #[test]
    fn summary_is_found_by_title_and_marker() {
        let marked = format!("x\n{SUMMARY_MARKER}\ny");
        let open = [
            issue(9, SUMMARY_TITLE, Some("no marker"), &[]),
            issue(8, "Backlog health report", Some(&marked), &[]),
            issue(7, SUMMARY_TITLE, Some(&marked), &[]),
            issue(5, SUMMARY_TITLE, Some(&marked), &[]),
        ];
        assert_eq!(find_summary(&open).map(|i| i.number), Some(5));
        assert!(find_summary(&open[..2]).is_none());
    }

    #[test]
    fn marked_summary_issues_are_dropped_from_the_snapshot() {
        let mut snap = snapshot(&[], &[]);
        let marked = format!("{SUMMARY_MARKER}\ntotals");
        snap.issues
            .push(issue(7, SUMMARY_TITLE, Some(&marked), &[]));
        snap.issues
            .push(issue(9, SUMMARY_TITLE, Some(&marked), &[]));
        snap.issues
            .push(issue(8, SUMMARY_TITLE, Some("someone else's"), &[]));
        drop_summary(&mut snap);
        let left: Vec<u64> = snap.issues.iter().map(|i| i.number).collect();
        assert_eq!(left, [1204, 8]);
    }

    #[test]
    fn summary_body_lists_totals_and_likely_fixed_with_links() {
        let body = summary_body(&example(), Some("https://acme.github.io/rocketdb/"));
        assert!(body.starts_with(SUMMARY_MARKER));
        for want in [
            "checked `main` at [`9f1c2ab`](https://github.com/acme/rocketdb/commit/9f1c2ab7e0d4c3b8a1f2e3d4c5b6a7f8e9d0c1b2) in free-ai mode.",
            "[Open the dashboard](https://acme.github.io/rocketdb/)",
            "### Issues (3412 open)",
            "| Likely fixed | 1 |",
            "### Pull requests (286 open)",
            "| Superseded | 1 |",
            "- #1204 Panic when compacting an empty SSTable (high confidence): [`#2977`](https://github.com/acme/rocketdb/pull/2977) Merged 2026-03-11; adds empty check; does not link this issue",
        ] {
            assert!(body.contains(want), "missing {want:?} in\n{body}");
        }
        assert!(!body.contains("Can't tell"), "zero rows are left out");
        assert_eq!(
            body,
            summary_body(&example(), Some("https://acme.github.io/rocketdb/"))
        );
    }

    #[test]
    fn summary_body_sorts_by_confidence_then_age_and_caps_the_list() {
        let mut report = example();
        let template = report.items[0].clone();
        report.items = (1..=TOP_LIKELY_FIXED as u64 + 2)
            .map(|n| {
                let mut item = template.clone();
                item.number = n;
                item.created_at += chrono::Duration::days(n as i64);
                item.confidence = if n == 3 {
                    Confidence::High
                } else {
                    Confidence::Medium
                };
                item
            })
            .collect();
        let body = summary_body(&report, None);
        let first = body.find("- #3 ").unwrap();
        assert!(first < body.find("- #1 ").unwrap());
        assert!(body.find("- #1 ").unwrap() < body.find("- #2 ").unwrap());
        assert!(!body.contains("- #22 "));
        assert!(body.contains("- and 2 more"));
        assert!(!body.contains("dashboard"));
    }

    #[test]
    fn untrusted_text_cannot_mention_or_break_markdown() {
        assert_eq!(
            md("@ann fix `x` | #3\nnext"),
            "@\u{2060}ann fix \\`x\\` \\| \\#3 next"
        );
    }

    #[tokio::test]
    async fn applies_labels_creating_missing_ones() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/repos/acme/rocketdb/labels"))
            .and(body_json(json!({
                "name": "stillvalid: likely-fixed",
                "color": "0e8a16",
                "description": label_style(Verdict::LikelyFixed).1,
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path(
                "/repos/acme/rocketdb/issues/2890/labels/stillvalid%3A%20conflicts",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .expect(1)
            .mount(&server)
            .await;
        // Removed by someone else meanwhile.
        Mock::given(method("DELETE"))
            .and(path(
                "/repos/acme/rocketdb/issues/2890/labels/stillvalid%3A%20abandoned",
            ))
            .respond_with(error(404))
            .expect(1)
            .mount(&server)
            .await;
        for (n, name) in [
            (1204, "stillvalid: likely-fixed"),
            (2890, "stillvalid: superseded"),
        ] {
            Mock::given(method("POST"))
                .and(path(format!("/repos/acme/rocketdb/issues/{n}/labels")))
                .and(body_json(json!({ "labels": [name] })))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
                .expect(1)
                .mount(&server)
                .await;
        }

        let snap = snapshot(&[], &["stillvalid: conflicts", "stillvalid: abandoned"]);
        let changes = plan_labels(&example(), &snap);
        let missing = missing_labels(&changes, &["stillvalid: superseded".into()]);
        let done = writer(&server)
            .apply_labels(&changes, &missing)
            .await
            .unwrap();
        assert_eq!((done.created, done.changed, done.failed.len()), (1, 2, 0));
    }

    #[tokio::test]
    async fn forbidden_stops_all_label_writes() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(error(403))
            .expect(1)
            .mount(&server)
            .await;
        let changes = plan_labels(&example(), &snapshot(&[], &[]));
        let err = writer(&server)
            .apply_labels(&changes, &[Verdict::LikelyFixed])
            .await
            .unwrap_err();
        assert!(matches!(err, GithubError::Forbidden(_)), "{err}");
    }

    #[tokio::test]
    async fn one_failed_item_is_skipped() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/repos/acme/rocketdb/issues/1204/labels"))
            .respond_with(error(500))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/repos/acme/rocketdb/issues/2890/labels"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .expect(1)
            .mount(&server)
            .await;
        let changes = plan_labels(&example(), &snapshot(&[], &[]));
        let done = writer(&server).apply_labels(&changes, &[]).await.unwrap();
        assert_eq!(done.changed, 1);
        assert_eq!(done.failed[0].0, 1204);
    }

    #[tokio::test]
    async fn lists_repo_labels_across_pages() {
        let server = MockServer::start().await;
        let next = format!(
            "<{}/repos/acme/rocketdb/labels?per_page=100&page=2>; rel=\"next\"",
            server.uri()
        );
        Mock::given(method("GET"))
            .and(path("/repos/acme/rocketdb/labels"))
            .and(query_param("page", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([{ "name": "b" }])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/acme/rocketdb/labels"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Link", next.as_str())
                    .set_body_json(json!([{ "name": "a" }])),
            )
            .mount(&server)
            .await;
        assert_eq!(writer(&server).repo_labels().await.unwrap(), ["a", "b"]);
    }

    fn marked(body: &str) -> String {
        format!("{SUMMARY_MARKER}\n{body}")
    }

    async fn no_closed_summary(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path("/search/issues"))
            .and(query_param(
                "q",
                "repo:acme/rocketdb is:issue is:closed in:title \"Backlog health\"",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "total_count": 1,
                "items": [{ "number": 4, "title": "Backlog health", "body": "someone else's" }],
            })))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn unchanged_summary_makes_no_requests() {
        let server = MockServer::start().await;
        let body = marked("same");
        let open = [issue(7, SUMMARY_TITLE, Some(&body), &[])];
        let out = writer(&server)
            .sync_summary(&open, &body, false)
            .await
            .unwrap();
        assert!(matches!(out, SummaryOutcome::Unchanged(7)));
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn changed_summary_is_patched() {
        let server = MockServer::start().await;
        let body = marked("new");
        Mock::given(method("PATCH"))
            .and(path("/repos/acme/rocketdb/issues/7"))
            .and(body_json(json!({ "body": body })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .mount(&server)
            .await;
        let open = [issue(7, SUMMARY_TITLE, Some(&marked("old")), &[])];
        let w = writer(&server);
        assert!(matches!(
            w.sync_summary(&open, &body, true).await.unwrap(),
            SummaryOutcome::WouldUpdate(7)
        ));
        assert!(matches!(
            w.sync_summary(&open, &body, false).await.unwrap(),
            SummaryOutcome::Updated(7)
        ));
    }

    #[tokio::test]
    async fn summary_patch_forbidden() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .respond_with(error(403))
            .mount(&server)
            .await;
        let open = [issue(7, SUMMARY_TITLE, Some(&marked("old")), &[])];
        let err = writer(&server)
            .sync_summary(&open, &marked("new"), false)
            .await
            .unwrap_err();
        assert!(matches!(err, GithubError::Forbidden(_)));
    }

    #[tokio::test]
    async fn closed_summary_is_left_alone() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search/issues"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "total_count": 1,
                "items": [{ "number": 3, "title": "Backlog health", "body": marked("old") }],
            })))
            .expect(1)
            .mount(&server)
            .await;
        let out = writer(&server)
            .sync_summary(&[], &marked("new"), false)
            .await
            .unwrap();
        assert!(matches!(out, SummaryOutcome::Closed(3)));
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn missing_summary_is_created_and_pinned() {
        let server = MockServer::start().await;
        no_closed_summary(&server).await;
        let body = marked("new");
        Mock::given(method("POST"))
            .and(path("/repos/acme/rocketdb/issues"))
            .and(body_json(
                json!({ "title": "Backlog health", "body": body }),
            ))
            .respond_with(
                ResponseTemplate::new(201)
                    .set_body_json(json!({ "number": 12, "node_id": "I_12" })),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(body_partial_json(json!({ "variables": { "id": "I_12" } })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": { "pinIssue": { "issue": { "number": 12 } } }
            })))
            .expect(1)
            .mount(&server)
            .await;
        let out = writer(&server)
            .sync_summary(&[], &body, false)
            .await
            .unwrap();
        assert!(matches!(
            out,
            SummaryOutcome::Created {
                number: 12,
                pinned: Ok(())
            }
        ));
    }

    #[tokio::test]
    async fn pin_failure_is_reported_but_the_issue_stays_created() {
        let server = MockServer::start().await;
        no_closed_summary(&server).await;
        Mock::given(method("POST"))
            .and(path("/repos/acme/rocketdb/issues"))
            .respond_with(
                ResponseTemplate::new(201)
                    .set_body_json(json!({ "number": 12, "node_id": "I_12" })),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": null,
                "errors": [{ "message": "You can't pin more than 3 issues to a repository" }]
            })))
            .mount(&server)
            .await;
        let out = writer(&server)
            .sync_summary(&[], &marked("new"), false)
            .await
            .unwrap();
        assert!(matches!(
            out,
            SummaryOutcome::Created {
                number: 12,
                pinned: Err(_)
            }
        ));
    }

    #[tokio::test]
    async fn dry_run_and_forbidden_create() {
        let server = MockServer::start().await;
        no_closed_summary(&server).await;
        Mock::given(method("POST"))
            .and(path("/repos/acme/rocketdb/issues"))
            .respond_with(error(403))
            .expect(1)
            .mount(&server)
            .await;
        let w = writer(&server);
        let body = marked("new");
        assert!(matches!(
            w.sync_summary(&[], &body, true).await.unwrap(),
            SummaryOutcome::WouldCreate
        ));
        let err = w.sync_summary(&[], &body, false).await.unwrap_err();
        assert!(matches!(err, GithubError::Forbidden(_)));
    }
}
