//! Fetcher: pulls open issues and pull requests for a repository from the GitHub REST API.

use chrono::{DateTime, Utc};
use octocrab::{Octocrab, Page};
use serde::de::DeserializeOwned;
use serde::Deserialize;

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
    pub async fn fetch(&self, repo: &str, branch: Option<&str>) -> Result<Snapshot, FetchError> {
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

        Ok(Snapshot {
            repo: format!("{owner}/{name}"),
            branch,
            head_sha,
            issues,
            pulls,
        })
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
    use serde_json::json;
    use wiremock::matchers::{method, path, query_param};
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

        let snap = fetcher(&server).await.fetch("o/r", None).await.unwrap();

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
            .fetch("o/r", Some("dev"))
            .await
            .unwrap();

        assert_eq!(snap.branch, "dev");
        assert_eq!(snap.head_sha, "def456");
        assert!(snap.issues.is_empty() && snap.pulls.is_empty());
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

        let err = fetcher(&server).await.fetch("o/r", None).await.unwrap_err();
        assert!(matches!(err, FetchError::Api(_)));
    }
}
