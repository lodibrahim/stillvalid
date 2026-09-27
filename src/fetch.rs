//! Fetcher: pulls open issues and pull requests for a repository from the GitHub REST API,
//! and the PRs/commits that reference each issue from the GraphQL timeline.

use chrono::{DateTime, Utc};
use octocrab::{Octocrab, Page};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("expected owner/repo, got `{0}`")]
    BadRepo(String),
    #[error("GitHub API request failed: {0}")]
    Api(#[from] octocrab::Error),
}

/// An open issue. The REST issues endpoint also returns pull requests; those carry `pull_request`.
#[derive(Debug, Clone, Deserialize)]
pub struct Issue {
    pub number: u64,
    pub title: String,
    pub html_url: String,
    pub created_at: DateTime<Utc>,
    pub body: Option<String>,
    #[serde(default)]
    pub pull_request: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Pull {
    pub number: u64,
    pub title: String,
    pub html_url: String,
    pub created_at: DateTime<Utc>,
    pub head: PullHead,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PullHead {
    pub sha: String,
}

/// Everything fetched for one scan.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub repo: String,
    pub branch: String,
    pub head_sha: String,
    pub issues: Vec<Issue>,
    pub pulls: Vec<Pull>,
    /// Same-repo PRs and commits that reference each open issue, keyed by issue number.
    /// Empty when fetched without references.
    pub references: BTreeMap<u64, Vec<Reference>>,
    /// Latest time each open issue was reopened, for issues that ever were.
    pub reopened_at: BTreeMap<u64, DateTime<Utc>>,
    /// Referenced commits that are on `branch`.
    pub commits_on_branch: BTreeSet<String>,
}

/// Something in the same repository that mentions an issue.
#[derive(Debug, Clone, PartialEq)]
pub enum Reference {
    Pull(PullRef),
    Commit {
        oid: String,
        url: String,
        /// The commit message closes the issue with a closing keyword (`fixes #N`).
        will_close: bool,
        /// When the commit last referenced the issue.
        referenced_at: DateTime<Utc>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct PullRef {
    pub number: u64,
    pub url: String,
    pub merged_at: Option<DateTime<Utc>>,
    /// Branch the PR targets (or was merged into).
    pub base_ref: String,
    /// The PR says it closes the issue (`fixes #N`, or linked in the Development sidebar).
    pub will_close: bool,
}

/// Issues per GraphQL page, and timeline events per issue (events past the cap are not fetched).
const ISSUES_PER_PAGE: u32 = 50;
const EVENTS_PER_ISSUE: u32 = 100;

const REFERENCES_QUERY: &str = r#"
query($owner: String!, $name: String!, $cursor: String, $issues: Int!, $events: Int!) {
  repository(owner: $owner, name: $name) {
    issues(states: OPEN, first: $issues, after: $cursor) {
      pageInfo { hasNextPage endCursor }
      nodes {
        number
        timelineItems(first: $events, itemTypes: [CROSS_REFERENCED_EVENT, CONNECTED_EVENT, REFERENCED_EVENT, REOPENED_EVENT]) {
          nodes {
            __typename
            ... on CrossReferencedEvent {
              isCrossRepository
              willCloseTarget
              source { __typename ... on PullRequest { number url mergedAt baseRefName } }
            }
            ... on ConnectedEvent {
              isCrossRepository
              source { __typename ... on PullRequest { number url mergedAt baseRefName } }
              subject { __typename ... on PullRequest { number url mergedAt baseRefName } }
            }
            ... on ReferencedEvent {
              isCrossRepository
              createdAt
              commit { oid url message }
            }
            ... on ReopenedEvent { createdAt }
          }
        }
      }
    }
  }
}
"#;

#[derive(Deserialize)]
struct RefsData {
    repository: RefsRepo,
}

#[derive(Deserialize)]
struct RefsRepo {
    issues: RefsIssues,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RefsIssues {
    page_info: PageInfo,
    nodes: Vec<RefsIssue>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    has_next_page: bool,
    end_cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RefsIssue {
    number: u64,
    timeline_items: Timeline,
}

#[derive(Deserialize)]
struct Timeline {
    nodes: Vec<TimelineItem>,
}

#[derive(Deserialize)]
#[serde(tag = "__typename")]
enum TimelineItem {
    #[serde(rename_all = "camelCase")]
    CrossReferencedEvent {
        is_cross_repository: bool,
        will_close_target: bool,
        source: Node,
    },
    #[serde(rename_all = "camelCase")]
    ConnectedEvent {
        is_cross_repository: bool,
        source: Node,
        subject: Node,
    },
    #[serde(rename_all = "camelCase")]
    ReferencedEvent {
        is_cross_repository: bool,
        created_at: DateTime<Utc>,
        commit: Option<CommitNode>,
    },
    #[serde(rename_all = "camelCase")]
    ReopenedEvent { created_at: DateTime<Utc> },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(tag = "__typename")]
enum Node {
    PullRequest(PullNode),
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PullNode {
    number: u64,
    url: String,
    merged_at: Option<DateTime<Utc>>,
    base_ref_name: String,
}

#[derive(Deserialize)]
struct CommitNode {
    oid: String,
    url: String,
    message: String,
}

impl PullNode {
    fn into_ref(self, will_close: bool) -> Reference {
        Reference::Pull(PullRef {
            number: self.number,
            url: self.url,
            merged_at: self.merged_at,
            base_ref: self.base_ref_name,
            will_close,
        })
    }
}

/// GitHub's closing keywords.
const CLOSING_KEYWORDS: [&str; 9] = [
    "close", "closes", "closed", "fix", "fixes", "fixed", "resolve", "resolves", "resolved",
];

/// Whether a commit message closes issue `number` of `repo` with a closing keyword, as GitHub
/// parses it: `Fixes #12`, `closes: owner/repo#12`, `Resolved https://github.com/owner/repo/issues/12`.
/// Case-insensitive; the keyword must start a word and be followed by spaces or tabs.
fn closes_issue(message: &str, repo: &str, number: u64) -> bool {
    let text = message.to_ascii_lowercase();
    let repo = repo.to_ascii_lowercase();
    let prefixes = [
        "#".to_string(),
        format!("{repo}#"),
        format!("https://github.com/{repo}/issues/"),
    ];
    CLOSING_KEYWORDS.iter().any(|kw| {
        text.match_indices(kw).any(|(i, _)| {
            let word_start = text[..i]
                .chars()
                .next_back()
                .is_none_or(|c| !c.is_alphanumeric());
            let rest = &text[i + kw.len()..];
            let rest = rest.strip_prefix(':').unwrap_or(rest);
            let target = rest.trim_start_matches([' ', '\t']);
            word_start
                && rest.starts_with([' ', '\t'])
                && prefixes.iter().any(|p| {
                    target.strip_prefix(p.as_str()).is_some_and(|n| {
                        let end = n.find(|c: char| !c.is_ascii_digit()).unwrap_or(n.len());
                        n[..end].parse() == Ok(number)
                    })
                })
        })
    })
}

/// Turn issue `number`'s timeline into same-repo references, one per PR/commit,
/// plus the latest time the issue was reopened.
fn collect_references(
    events: Vec<TimelineItem>,
    repo: &str,
    number: u64,
) -> (Vec<Reference>, Option<DateTime<Utc>>) {
    let mut refs: Vec<Reference> = Vec::new();
    let mut reopened_at = None;
    let mut add = |new: Reference| {
        for existing in refs.iter_mut() {
            match (existing, &new) {
                (Reference::Pull(pr), Reference::Pull(new_pr)) if pr.number == new_pr.number => {
                    pr.will_close |= new_pr.will_close;
                    return;
                }
                (
                    Reference::Commit {
                        oid,
                        will_close,
                        referenced_at,
                        ..
                    },
                    Reference::Commit {
                        oid: new_oid,
                        will_close: new_close,
                        referenced_at: new_at,
                        ..
                    },
                ) if oid == new_oid => {
                    *will_close |= *new_close;
                    *referenced_at = (*referenced_at).max(*new_at);
                    return;
                }
                _ => {}
            }
        }
        refs.push(new);
    };

    for event in events {
        match event {
            TimelineItem::CrossReferencedEvent {
                is_cross_repository: false,
                will_close_target,
                source: Node::PullRequest(pr),
            } => add(pr.into_ref(will_close_target)),
            TimelineItem::ConnectedEvent {
                is_cross_repository: false,
                source,
                subject,
            } => {
                // On an issue's timeline, the linked PR can be either side of the connection.
                for node in [source, subject] {
                    if let Node::PullRequest(pr) = node {
                        add(pr.into_ref(true));
                    }
                }
            }
            TimelineItem::ReferencedEvent {
                is_cross_repository: false,
                created_at,
                commit: Some(c),
            } => add(Reference::Commit {
                oid: c.oid,
                url: c.url,
                will_close: closes_issue(&c.message, repo, number),
                referenced_at: created_at,
            }),
            TimelineItem::ReopenedEvent { created_at } => {
                reopened_at = reopened_at.max(Some(created_at));
            }
            _ => {}
        }
    }
    (refs, reopened_at)
}

#[derive(Deserialize)]
struct Comparison {
    status: String,
}

#[derive(Deserialize)]
struct RepoInfo {
    default_branch: String,
}

#[derive(Deserialize)]
struct BranchInfo {
    commit: BranchCommit,
}

#[derive(Deserialize)]
struct BranchCommit {
    sha: String,
}

pub fn split_repo(repo: &str) -> Result<(&str, &str), FetchError> {
    match repo.split_once('/') {
        Some((owner, name)) if !owner.is_empty() && !name.is_empty() && !name.contains('/') => {
            Ok((owner, name))
        }
        _ => Err(FetchError::BadRepo(repo.to_string())),
    }
}

pub struct Fetcher {
    gh: Octocrab,
}

impl Fetcher {
    pub fn new(gh: Octocrab) -> Self {
        Self { gh }
    }

    /// Fetch all open issues and PRs, plus the head commit of `branch` (or the default branch).
    /// `with_references` also fetches issue timelines; GitHub's GraphQL API requires a token.
    pub async fn fetch(
        &self,
        repo: &str,
        branch: Option<&str>,
        with_references: bool,
    ) -> Result<Snapshot, FetchError> {
        let (owner, name) = split_repo(repo)?;
        let base = format!("/repos/{owner}/{name}");

        let branch = match branch {
            Some(b) => b.to_string(),
            None => {
                self.gh
                    .get::<RepoInfo, _, ()>(&base, None)
                    .await?
                    .default_branch
            }
        };
        let head_sha = self
            .gh
            .get::<BranchInfo, _, ()>(format!("{base}/branches/{branch}"), None)
            .await?
            .commit
            .sha;

        let issues = self
            .all_open::<Issue>(&format!("{base}/issues"))
            .await?
            .into_iter()
            .filter(|i| i.pull_request.is_none())
            .collect();
        let pulls = self.all_open::<Pull>(&format!("{base}/pulls")).await?;
        let (references, reopened_at) = if with_references {
            self.references(owner, name).await?
        } else {
            Default::default()
        };

        let oids: BTreeSet<&str> = references
            .values()
            .flatten()
            .filter_map(|r| match r {
                // Only commits that claim to fix an issue can yield a verdict.
                Reference::Commit {
                    oid,
                    will_close: true,
                    ..
                } => Some(oid.as_str()),
                _ => None,
            })
            .collect();
        let mut commits_on_branch = BTreeSet::new();
        for oid in oids {
            if self.is_on_branch(&base, &head_sha, oid).await? {
                commits_on_branch.insert(oid.to_string());
            }
        }

        Ok(Snapshot {
            repo: format!("{owner}/{name}"),
            branch,
            head_sha,
            issues,
            pulls,
            references,
            reopened_at,
            commits_on_branch,
        })
    }

    /// Whether commit `oid` is contained in the scanned branch head: comparing head...oid is
    /// `behind` or `identical`. 404/422 (unknown commit, or no common history) mean it is not.
    async fn is_on_branch(&self, base: &str, head: &str, oid: &str) -> Result<bool, FetchError> {
        let route = format!("{base}/compare/{head}...{oid}");
        match self
            .gh
            .get::<Comparison, _, _>(route, Some(&[("per_page", "1")]))
            .await
        {
            Ok(c) => Ok(c.status == "behind" || c.status == "identical"),
            Err(octocrab::Error::GitHub { source, .. })
                if matches!(source.status_code.as_u16(), 404 | 422) =>
            {
                Ok(false)
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Page through open issues' timelines via GraphQL: references per issue (issues with none
    /// are omitted) and latest reopen time per issue.
    async fn references(
        &self,
        owner: &str,
        name: &str,
    ) -> Result<(BTreeMap<u64, Vec<Reference>>, BTreeMap<u64, DateTime<Utc>>), FetchError> {
        let repo = format!("{owner}/{name}");
        let mut out = BTreeMap::new();
        let mut reopened = BTreeMap::new();
        let mut cursor: Option<String> = None;
        loop {
            let payload = json!({
                "query": REFERENCES_QUERY,
                "variables": {
                    "owner": owner,
                    "name": name,
                    "cursor": cursor,
                    "issues": ISSUES_PER_PAGE,
                    "events": EVENTS_PER_ISSUE,
                },
            });
            let data: RefsData = self.gh.graphql(&payload).await?;
            let issues = data.repository.issues;
            for issue in issues.nodes {
                let (refs, reopened_at) =
                    collect_references(issue.timeline_items.nodes, &repo, issue.number);
                if !refs.is_empty() {
                    out.insert(issue.number, refs);
                }
                if let Some(at) = reopened_at {
                    reopened.insert(issue.number, at);
                }
            }
            match issues.page_info.end_cursor {
                Some(next) if issues.page_info.has_next_page => cursor = Some(next),
                _ => return Ok((out, reopened)),
            }
        }
    }

    /// GET every page of open items at `route`, following the `Link: rel="next"` header.
    async fn all_open<T: DeserializeOwned>(&self, route: &str) -> Result<Vec<T>, FetchError> {
        let params = [("state", "open"), ("per_page", "100")];
        let first: Page<T> = self.gh.get(route, Some(&params)).await?;
        Ok(self.gh.all_pages(first).await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_partial_json, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn issue(n: u64) -> serde_json::Value {
        json!({
            "number": n,
            "title": format!("Issue {n}"),
            "html_url": format!("https://github.com/o/r/issues/{n}"),
            "created_at": "2026-01-01T00:00:00Z",
            "body": "body"
        })
    }

    fn pull(n: u64) -> serde_json::Value {
        json!({
            "number": n,
            "title": format!("PR {n}"),
            "html_url": format!("https://github.com/o/r/pull/{n}"),
            "created_at": "2026-02-01T00:00:00Z",
            "head": { "sha": format!("sha{n}") }
        })
    }

    async fn fetcher(server: &MockServer) -> Fetcher {
        let gh = Octocrab::builder()
            .base_uri(server.uri())
            .unwrap()
            .build()
            .unwrap();
        Fetcher::new(gh)
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
    fn split_repo_accepts_owner_repo_only() {
        assert_eq!(split_repo("o/r").unwrap(), ("o", "r"));
        for bad in ["o", "o/", "/r", "o/r/x", ""] {
            assert!(split_repo(bad).is_err(), "{bad} should be rejected");
        }
    }

    #[tokio::test]
    async fn fetches_all_pages_and_separates_prs_from_issues() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/repos/o/r"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "default_branch": "main" })),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/branches/main"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "commit": { "sha": "abc123" } })),
            )
            .mount(&server)
            .await;

        // Issues: page 1 links to page 2; page 2 includes a PR, which must be dropped.
        let next = format!(
            "<{}/repos/o/r/issues?state=open&per_page=100&page=2>; rel=\"next\"",
            server.uri()
        );
        Mock::given(method("GET"))
            .and(path("/repos/o/r/issues"))
            .and(query_param("page", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                issue(3),
                { "number": 4, "title": "PR 4", "html_url": "https://github.com/o/r/pull/4",
                  "created_at": "2026-01-01T00:00:00Z", "body": null,
                  "pull_request": { "url": "https://api.github.com/repos/o/r/pulls/4" } }
            ])))
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/issues"))
            .and(query_param("state", "open"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("link", next.as_str())
                    .set_body_json(json!([issue(1), issue(2)])),
            )
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/repos/o/r/pulls"))
            .and(query_param("state", "open"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([pull(4)])))
            .mount(&server)
            .await;

        let snap = fetcher(&server)
            .await
            .fetch("o/r", None, false)
            .await
            .unwrap();

        assert_eq!(snap.repo, "o/r");
        assert_eq!(snap.branch, "main");
        assert_eq!(snap.head_sha, "abc123");
        let issue_numbers: Vec<u64> = snap.issues.iter().map(|i| i.number).collect();
        assert_eq!(issue_numbers, [1, 2, 3]);
        assert_eq!(snap.pulls.len(), 1);
        assert_eq!(snap.pulls[0].head.sha, "sha4");
    }

    #[tokio::test]
    async fn explicit_branch_skips_default_branch_lookup() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/branches/dev"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "commit": { "sha": "def456" } })),
            )
            .mount(&server)
            .await;
        for route in ["/repos/o/r/issues", "/repos/o/r/pulls"] {
            Mock::given(method("GET"))
                .and(path(route))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
                .mount(&server)
                .await;
        }

        let snap = fetcher(&server)
            .await
            .fetch("o/r", Some("dev"), false)
            .await
            .unwrap();

        assert_eq!(snap.branch, "dev");
        assert_eq!(snap.head_sha, "def456");
        assert!(snap.issues.is_empty() && snap.pulls.is_empty());
        assert!(snap.references.is_empty());
    }

    fn pr_node(n: u64, merged: bool) -> serde_json::Value {
        json!({
            "__typename": "PullRequest",
            "number": n,
            "url": format!("https://github.com/o/r/pull/{n}"),
            "mergedAt": if merged { json!("2026-03-11T00:00:00Z") } else { json!(null) },
            "baseRefName": "main"
        })
    }

    fn refs_page(issues: serde_json::Value, next: Option<&str>) -> serde_json::Value {
        json!({ "data": { "repository": { "issues": {
            "pageInfo": { "hasNextPage": next.is_some(), "endCursor": next },
            "nodes": issues
        }}}})
    }

    #[test]
    fn collects_same_repo_references_and_merges_duplicates() {
        let events: Vec<TimelineItem> = serde_json::from_value(json!([
            // Mentioned by PR 10 without "fixes", then linked in the sidebar: one entry, will_close.
            { "__typename": "CrossReferencedEvent", "isCrossRepository": false,
              "willCloseTarget": false, "source": pr_node(10, true) },
            { "__typename": "ConnectedEvent", "isCrossRepository": false,
              "source": { "__typename": "Issue" }, "subject": pr_node(10, true) },
            // PR in another repo: ignored.
            { "__typename": "CrossReferencedEvent", "isCrossRepository": true,
              "willCloseTarget": true, "source": pr_node(99, true) },
            // Mentioned by another issue: ignored.
            { "__typename": "CrossReferencedEvent", "isCrossRepository": false,
              "willCloseTarget": false, "source": { "__typename": "Issue" } },
            // Commit mention, twice: one entry with the later time. Deleted commit (null): ignored.
            { "__typename": "ReferencedEvent", "isCrossRepository": false,
              "createdAt": "2026-04-02T00:00:00Z",
              "commit": { "oid": "abc", "url": "https://github.com/o/r/commit/abc",
                          "message": "Fix empty input\n\nFixes #1" } },
            { "__typename": "ReferencedEvent", "isCrossRepository": false,
              "createdAt": "2026-04-01T00:00:00Z",
              "commit": { "oid": "abc", "url": "https://github.com/o/r/commit/abc",
                          "message": "Fix empty input\n\nFixes #1" } },
            { "__typename": "ReferencedEvent", "isCrossRepository": false,
              "createdAt": "2026-04-03T00:00:00Z", "commit": null },
            // Reopened twice: the latest counts.
            { "__typename": "ReopenedEvent", "createdAt": "2026-05-02T00:00:00Z" },
            { "__typename": "ReopenedEvent", "createdAt": "2026-05-01T00:00:00Z" },
            // Unrequested event type: ignored.
            { "__typename": "LabeledEvent" }
        ]))
        .unwrap();

        let (refs, reopened_at) = collect_references(events, "o/r", 1);

        assert_eq!(
            refs,
            [
                Reference::Pull(PullRef {
                    number: 10,
                    url: "https://github.com/o/r/pull/10".into(),
                    merged_at: Some("2026-03-11T00:00:00Z".parse().unwrap()),
                    base_ref: "main".into(),
                    will_close: true,
                }),
                Reference::Commit {
                    oid: "abc".into(),
                    url: "https://github.com/o/r/commit/abc".into(),
                    will_close: true,
                    referenced_at: "2026-04-02T00:00:00Z".parse().unwrap(),
                },
            ]
        );
        assert_eq!(reopened_at, Some("2026-05-02T00:00:00Z".parse().unwrap()));
    }

    #[tokio::test]
    async fn fetches_references_across_graphql_pages() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/branches/main"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "commit": { "sha": "abc123" } })),
            )
            .mount(&server)
            .await;
        for route in ["/repos/o/r/issues", "/repos/o/r/pulls"] {
            Mock::given(method("GET"))
                .and(path(route))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
                .mount(&server)
                .await;
        }

        let cross_ref = |pr: u64, closes: bool| {
            json!({ "__typename": "CrossReferencedEvent", "isCrossRepository": false,
                    "willCloseTarget": closes, "source": pr_node(pr, true) })
        };
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(body_partial_json(
                json!({ "variables": { "cursor": null } }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(refs_page(
                json!([
                    { "number": 1, "timelineItems": { "nodes": [cross_ref(10, true)] } },
                    { "number": 2, "timelineItems": { "nodes": [] } }
                ]),
                Some("c1"),
            )))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(body_partial_json(
                json!({ "variables": { "cursor": "c1" } }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(refs_page(
                json!([{ "number": 3, "timelineItems": { "nodes": [cross_ref(11, false)] } }]),
                None,
            )))
            .mount(&server)
            .await;

        let snap = fetcher(&server)
            .await
            .fetch("o/r", Some("main"), true)
            .await
            .unwrap();

        let keys: Vec<u64> = snap.references.keys().copied().collect();
        assert_eq!(keys, [1, 3], "issue 2 has no references and is omitted");
        assert!(
            matches!(&snap.references[&1][0], Reference::Pull(p) if p.number == 10 && p.will_close)
        );
        assert!(
            matches!(&snap.references[&3][0], Reference::Pull(p) if p.number == 11 && !p.will_close)
        );
    }

    /// Server with branch `main` and no open issues or PRs.
    async fn empty_repo() -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/branches/main"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "commit": { "sha": "abc123" } })),
            )
            .mount(&server)
            .await;
        for route in ["/repos/o/r/issues", "/repos/o/r/pulls"] {
            Mock::given(method("GET"))
                .and(path(route))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
                .mount(&server)
                .await;
        }
        server
    }

    fn commit_ref(oid: &str) -> serde_json::Value {
        json!({ "__typename": "ReferencedEvent", "isCrossRepository": false,
                "createdAt": "2026-04-01T00:00:00Z",
                "commit": { "oid": oid, "url": format!("https://github.com/o/r/commit/{oid}"),
                           "message": format!("Fixes #1 via {oid}") } })
    }

    #[tokio::test]
    async fn parses_reopens_and_checks_referenced_commits_against_branch() {
        let server = empty_repo().await;

        // Issue 1 references every commit; issue 2 references c1 again (compared once) and was
        // reopened. Every message says "Fixes #1", so c7, referenced only by issue 2, is never compared.
        let all_commits = ["c1", "c2", "c3", "c4", "c5", "c6"].map(commit_ref);
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(refs_page(
                json!([
                    { "number": 1, "timelineItems": { "nodes": all_commits } },
                    { "number": 2, "timelineItems": { "nodes": [
                        commit_ref("c1"),
                        commit_ref("c7"),
                        { "__typename": "ReopenedEvent", "createdAt": "2026-05-01T00:00:00Z" }
                    ] } }
                ]),
                None,
            )))
            .mount(&server)
            .await;

        let statuses = [
            ("c1", 200, "behind"),
            ("c2", 200, "identical"),
            ("c3", 200, "diverged"),
            ("c4", 200, "ahead"),
            ("c5", 404, ""),
            ("c6", 422, ""),
            ("c7", 200, "behind"),
        ];
        for (oid, code, status) in statuses {
            let body = if code == 200 {
                json!({ "status": status })
            } else {
                json!({ "message": "No common ancestor" })
            };
            Mock::given(method("GET"))
                .and(path(format!("/repos/o/r/compare/abc123...{oid}")))
                .and(query_param("per_page", "1"))
                .respond_with(ResponseTemplate::new(code).set_body_json(body))
                .expect(if oid == "c7" { 0 } else { 1 })
                .mount(&server)
                .await;
        }

        let snap = fetcher(&server)
            .await
            .fetch("o/r", Some("main"), true)
            .await
            .unwrap();

        let on_branch: Vec<&str> = snap.commits_on_branch.iter().map(String::as_str).collect();
        assert_eq!(on_branch, ["c1", "c2"]);
        assert!(matches!(
            &snap.references[&1][0],
            Reference::Commit {
                will_close: true,
                ..
            }
        ));
        assert!(matches!(
            &snap.references[&2][0],
            Reference::Commit {
                will_close: false,
                ..
            }
        ));
        let reopened: Vec<(u64, DateTime<Utc>)> =
            snap.reopened_at.iter().map(|(n, t)| (*n, *t)).collect();
        assert_eq!(reopened, [(2, "2026-05-01T00:00:00Z".parse().unwrap())]);
    }

    #[tokio::test]
    async fn compare_server_error_is_reported() {
        let server = empty_repo().await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(refs_page(
                json!([{ "number": 1, "timelineItems": { "nodes": [
                    commit_ref("c1")
                ] } }]),
                None,
            )))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/compare/abc123...c1"))
            .respond_with(
                ResponseTemplate::new(403).set_body_json(json!({ "message": "rate limited" })),
            )
            .mount(&server)
            .await;

        let err = fetcher(&server)
            .await
            .fetch("o/r", Some("main"), true)
            .await
            .unwrap_err();
        assert!(matches!(err, FetchError::Api(_)));
    }

    #[tokio::test]
    async fn graphql_errors_are_reported() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/branches/main"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "commit": { "sha": "abc123" } })),
            )
            .mount(&server)
            .await;
        for route in ["/repos/o/r/issues", "/repos/o/r/pulls"] {
            Mock::given(method("GET"))
                .and(path(route))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
                .mount(&server)
                .await;
        }
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({ "errors": [{ "message": "Could not resolve to a Repository" }] }),
            ))
            .mount(&server)
            .await;

        let err = fetcher(&server)
            .await
            .fetch("o/r", Some("main"), true)
            .await
            .unwrap_err();
        assert!(matches!(err, FetchError::Api(_)));
    }

    #[tokio::test]
    async fn api_error_is_reported() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r"))
            .respond_with(
                ResponseTemplate::new(404).set_body_json(json!({ "message": "Not Found" })),
            )
            .mount(&server)
            .await;

        let err = fetcher(&server)
            .await
            .fetch("o/r", None, false)
            .await
            .unwrap_err();
        assert!(matches!(err, FetchError::Api(_)));
    }
}
