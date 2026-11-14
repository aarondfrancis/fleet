//! Read-only GitHub REST client for org runners and in-flight jobs.
//!
//! - Runners: `GET /orgs/{org}/actions/runners` (fine-grained PAT: org
//!   "Self-hosted runners: read"; classic: `admin:org`). A personal account has
//!   no organization runners, so its runners are listed per repository with
//!   `GET /repos/{o}/{r}/actions/runners` (repo "Administration: read"; classic: `repo`).
//! - Jobs: there is no org-wide jobs endpoint, so we list
//!   `GET /repos/{o}/{r}/actions/runs?status=queued|in_progress` per repo in scope,
//!   then `GET .../runs/{id}/jobs` (repo "Actions: read").
//! - Every GET is conditional (ETag); a 304 doesn't count against the rate limit.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::stream::{self, StreamExt};
use reqwest::StatusCode;
use reqwest::header::{ACCEPT, ETAG, HeaderMap, IF_NONE_MATCH, LINK, USER_AGENT};
use serde::Deserialize;
use serde_json::Value;

use crate::model::{AccountKind, GhJob, GhRunner, JobScan, RateLimit, RepoScope};
use crate::sync::MutexExt;

pub const API_VERSION: &str = "2026-03-10";
/// Bounds a repository list read, at 100 repositories a page.
const MAX_REPO_LIST_PAGES: usize = 50;
const CONCURRENCY: usize = 4;
// Bound each collection so a busy repository cannot monopolize a poll cycle.
const MAX_RUN_PAGES: usize = 4;
const MAX_JOB_PAGES: usize = 10;
const MAX_RUNNER_PAGES: usize = 20;
/// Repository runner lists are short; each repository is one or two pages.
const MAX_REPO_RUNNER_PAGES: usize = 5;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// Conditional responses kept per client. Job pages are keyed by run, so old runs must age out.
const MAX_CACHED_PAGES: usize = 512;

#[derive(Debug, Clone)]
struct ApiPage {
    body: Value,
    has_next: bool,
}

/// Recently fetched pages by URL, for conditional requests; the least recently used page goes first.
#[derive(Debug, Default)]
struct EtagCache {
    pages: HashMap<String, (String, ApiPage)>,
    order: VecDeque<String>,
}

impl EtagCache {
    fn get(&mut self, url: &str) -> Option<(String, ApiPage)> {
        let page = self.pages.get(url).cloned()?;
        self.touch(url);
        Some(page)
    }

    fn insert(&mut self, url: String, etag: String, page: ApiPage) {
        if self.pages.insert(url.clone(), (etag, page)).is_some() {
            self.touch(&url);
            return;
        }
        self.order.push_back(url);
        while self.order.len() > MAX_CACHED_PAGES {
            if let Some(oldest) = self.order.pop_front() {
                self.pages.remove(&oldest);
            }
        }
    }

    fn touch(&mut self, url: &str) {
        if let Some(index) = self.order.iter().position(|u| u == url)
            && let Some(url) = self.order.remove(index)
        {
            self.order.push_back(url);
        }
    }
}

struct Collection<T> {
    items: Vec<T>,
    truncated: bool,
}

#[derive(Debug, thiserror::Error, Clone, PartialEq)]
pub enum GithubError {
    #[error("GitHub rejected the token (401). It may be expired or revoked. Replace it in Settings ▸ GitHub.")]
    Unauthorized,
    #[error("GitHub refused access (403): {0}")]
    Forbidden(String),
    #[error("Not found (404): {0}")]
    NotFound(String),
    #[error("GitHub rate limit reached; resets {0}")]
    RateLimited(String),
    #[error("GitHub returned {0}: {1}")]
    Http(u16, String),
    #[error("Network error: {0}")]
    Network(String),
    #[error("Unexpected response: {0}")]
    Decode(String),
    #[error("GitHub runner listing is incomplete: {0}")]
    Incomplete(String),
}

pub struct GithubClient {
    http: reqwest::Client,
    api_base: String,
    token: String,
    cache: Mutex<EtagCache>,
    rate: Mutex<Option<RateLimit>>,
}

/// The token is never shown.
impl std::fmt::Debug for GithubClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GithubClient")
            .field("api_base", &self.api_base)
            .finish_non_exhaustive()
    }
}

impl GithubClient {
    pub fn new(api_base: &str, token: &str) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .expect("http client"),
            api_base: api_base.trim_end_matches('/').to_string(),
            token: token.to_string(),
            cache: Mutex::new(EtagCache::default()),
            rate: Mutex::new(None),
        }
    }

    pub fn rate_limit(&self) -> Option<RateLimit> {
        self.rate.locked().clone()
    }

    async fn get(&self, path_and_query: &str) -> Result<ApiPage, GithubError> {
        let url = format!("{}{}", self.api_base, path_and_query);
        let cached = self.cache.locked().get(&url);
        let mut req = self
            .http
            .get(&url)
            .header(ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION)
            .header(USER_AGENT, "runner-dashboard")
            .bearer_auth(&self.token);
        if let Some((etag, _)) = &cached {
            req = req.header(IF_NONE_MATCH, etag);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| GithubError::Network(e.without_url().to_string()))?;
        self.record_rate(resp.headers());
        let status = resp.status();

        if status == StatusCode::NOT_MODIFIED
            && let Some((_, body)) = cached
        {
            return Ok(body);
        }
        if !status.is_success() {
            let remaining_zero = resp
                .headers()
                .get("x-ratelimit-remaining")
                .and_then(|v| v.to_str().ok())
                == Some("0");
            let body = resp.text().await.unwrap_or_default();
            let msg = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| v["message"].as_str().map(str::to_string))
                .unwrap_or(body);
            return Err(match status.as_u16() {
                401 => GithubError::Unauthorized,
                403 | 429 if remaining_zero => GithubError::RateLimited(
                    self.rate_limit()
                        .and_then(|r| r.resets_at)
                        .map(|t| t.format("%H:%M UTC").to_string())
                        .unwrap_or_else(|| "soon".into()),
                ),
                403 => GithubError::Forbidden(msg),
                404 => GithubError::NotFound(msg),
                code => GithubError::Http(code, msg),
            });
        }
        let etag = resp
            .headers()
            .get(ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let has_next = resp.headers().get_all(LINK).iter().any(|header| {
            header.to_str().is_ok_and(|value| {
                value.split(',').any(|link| {
                    link.split(';').skip(1).any(|parameter| {
                        parameter
                            .trim()
                            .strip_prefix("rel=")
                            .is_some_and(|rel| rel.trim_matches('"').split_whitespace().any(|name| name == "next"))
                    })
                })
            })
        });
        let body: Value = resp.json().await.map_err(|e| GithubError::Decode(e.to_string()))?;
        let body = ApiPage { body, has_next };
        if let Some(etag) = etag {
            self.cache.locked().insert(url, etag, body.clone());
        }
        Ok(body)
    }

    fn record_rate(&self, headers: &HeaderMap) {
        let num = |name: &str| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<i64>().ok())
        };
        let count = |name: &str| num(name).and_then(|v| u32::try_from(v).ok());
        if let (Some(limit), Some(remaining)) = (count("x-ratelimit-limit"), count("x-ratelimit-remaining")) {
            *self.rate.locked() = Some(RateLimit {
                limit,
                remaining,
                resets_at: num("x-ratelimit-reset").and_then(|t| DateTime::from_timestamp(t, 0)),
            });
        }
    }

    async fn collection<T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        field: &str,
        per_page: usize,
        max_pages: usize,
    ) -> Result<Collection<T>, GithubError> {
        let mut items = Vec::new();
        let separator = if path.contains('?') { '&' } else { '?' };
        for page in 1..=max_pages {
            let response = self
                .get(&format!("{path}{separator}per_page={per_page}&page={page}"))
                .await?;
            let total = response.body["total_count"].as_u64();
            let batch: Vec<T> = decode(response.body[field].clone())?;
            let count = batch.len();
            items.extend(batch);
            let more = response.has_next || total.map_or(count == per_page, |total| (items.len() as u64) < total);
            if !more {
                return Ok(Collection {
                    items,
                    truncated: false,
                });
            }
            // An empty page with an advertised continuation is incomplete too.
            if count == 0 {
                return Ok(Collection { items, truncated: true });
            }
        }
        Ok(Collection { items, truncated: true })
    }

    pub async fn org_runners(&self, org: &str) -> Result<Vec<GhRunner>, GithubError> {
        let runners = self
            .collection::<RawRunner>(
                &format!("/orgs/{org}/actions/runners"),
                "runners",
                100,
                MAX_RUNNER_PAGES,
            )
            .await
            .map_err(|e| hint_runner_permission(e, org))?;
        if runners.truncated {
            return Err(GithubError::Incomplete(format!(
                "Couldn't read all of {org}'s runners within {MAX_RUNNER_PAGES} pages."
            )));
        }
        Ok(runners.items.into_iter().map(RawRunner::into_model).collect())
    }

    /// An organization's repositories, most recently pushed first.
    pub async fn recent_repos(&self, org: &str, max: usize) -> Result<Vec<String>, GithubError> {
        self.repo_names(&format!("/orgs/{org}/repos?sort=pushed&direction=desc"), None, max)
            .await
    }

    /// The personal account's own repositories, most recently pushed first.
    /// `/users/{login}/repos` lists only public ones, so this reads the signed-in
    /// user's repositories and keeps those `owner` owns.
    pub async fn user_repos(&self, owner: &str, max: usize) -> Result<Vec<String>, GithubError> {
        self.repo_names(
            "/user/repos?affiliation=owner&sort=pushed&direction=desc",
            Some(owner),
            max,
        )
        .await
    }

    /// Up to `max` unarchived repository names from a repository list, page by page.
    async fn repo_names(&self, path: &str, owner: Option<&str>, max: usize) -> Result<Vec<String>, GithubError> {
        #[derive(Deserialize)]
        struct Owner {
            login: String,
        }
        #[derive(Deserialize)]
        struct Repo {
            name: String,
            owner: Option<Owner>,
            #[serde(default)]
            archived: bool,
        }
        let per_page = max.clamp(1, 100);
        let mut names = Vec::new();
        for page in 1..=MAX_REPO_LIST_PAGES {
            if names.len() >= max {
                break;
            }
            let v = self.get(&format!("{path}&per_page={per_page}&page={page}")).await?;
            let repos: Vec<Repo> = decode(v.body)?;
            let count = repos.len();
            names.extend(
                repos
                    .into_iter()
                    .filter(|r| !r.archived)
                    .filter(|r| {
                        owner.is_none_or(|owner| r.owner.as_ref().is_some_and(|o| o.login.eq_ignore_ascii_case(owner)))
                    })
                    .map(|r| r.name),
            );
            if !v.has_next && count < per_page {
                break;
            }
        }
        names.truncate(max);
        Ok(names)
    }

    /// Recently pushed repositories of an organization or a personal account.
    async fn account_repos(&self, owner: &str, kind: AccountKind, max: usize) -> Result<Vec<String>, GithubError> {
        match kind {
            AccountKind::Organization => self.recent_repos(owner, max).await,
            AccountKind::User => self.user_repos(owner, max).await,
        }
    }

    /// Whether `login` is an organization or a personal account. Anything but a
    /// clear `User` answer keeps organization mode.
    pub async fn account_kind(&self, login: &str) -> AccountKind {
        match self.get(&format!("/users/{login}")).await {
            Ok(page) if page.body["type"].as_str() == Some("User") => AccountKind::User,
            _ => AccountKind::Organization,
        }
    }

    /// Runners for an organization, or for each repository of a personal account in scope.
    pub async fn runners(
        &self,
        owner: &str,
        kind: AccountKind,
        scope: &RepoScope,
    ) -> Result<Vec<GhRunner>, GithubError> {
        match kind {
            AccountKind::Organization => self.org_runners(owner).await,
            AccountKind::User => self.user_runners(owner, scope).await,
        }
    }

    async fn repo_runners(&self, owner: &str, repo: &str) -> Result<Vec<GhRunner>, GithubError> {
        let runners = self
            .collection::<RawRunner>(
                &format!("/repos/{owner}/{repo}/actions/runners"),
                "runners",
                100,
                MAX_REPO_RUNNER_PAGES,
            )
            .await?;
        if runners.truncated {
            return Err(GithubError::Incomplete(format!(
                "Couldn't read all of {owner}/{repo}'s runners within {MAX_REPO_RUNNER_PAGES} pages."
            )));
        }
        let repository = format!("{owner}/{repo}");
        Ok(runners
            .items
            .into_iter()
            .map(|raw| GhRunner {
                repository: Some(repository.clone()),
                ..raw.into_model()
            })
            .collect())
    }

    /// A personal account's runners, read per repository. The scope's list is used
    /// when it names repositories; otherwise the account's own repositories are read.
    pub async fn user_runners(&self, owner: &str, scope: &RepoScope) -> Result<Vec<GhRunner>, GithubError> {
        let repos = match scope {
            RepoScope::Selected { repos } => repos.clone(),
            RepoScope::Recent { max_repos } => self.user_repos(owner, *max_repos as usize).await?,
            RepoScope::None => self.user_repos(owner, usize::MAX).await?,
        };
        self.listed_repo_runners(owner, &repos).await
    }

    /// The runners registered to each of `owner`'s `repos`, read one repository at a
    /// time. This works for an organization too, and never calls an `/orgs` endpoint.
    /// A repository that can't be read fails the whole refresh, so a partial list
    /// never looks complete. The error names each repository that failed. A
    /// repository listed twice (names are case-insensitive) is read once.
    pub async fn listed_repo_runners(&self, owner: &str, repos: &[String]) -> Result<Vec<GhRunner>, GithubError> {
        let repos = unique_repos(repos);
        let results: Vec<(String, Result<Vec<GhRunner>, GithubError>)> = stream::iter(repos.iter().cloned())
            .map(|repo| async move {
                let result = self.repo_runners(owner, &repo).await;
                (repo, result)
            })
            .buffer_unordered(CONCURRENCY)
            .collect()
            .await;
        let mut runners: Vec<GhRunner> = Vec::new();
        let mut failed: Vec<(String, GithubError)> = Vec::new();
        for (repo, result) in results {
            match result {
                Ok(batch) => {
                    runners.extend(batch);
                }
                Err(e @ (GithubError::NotFound(_) | GithubError::Forbidden(_))) => failed.push((repo, e)),
                Err(e) => return Err(e),
            }
        }
        if !failed.is_empty() {
            failed.sort_by(|a, b| a.0.cmp(&b.0));
            if failed.len() == repos.len() {
                let (_, e) = failed.swap_remove(0);
                return Err(hint_repo_runner_permission(e, owner));
            }
            let reasons: Vec<String> = failed.iter().map(|(repo, e)| format!("{owner}/{repo}: {e}")).collect();
            return Err(GithubError::Incomplete(format!(
                "Read runners in {} of {} repositories. {}. Listing runners needs the repository permission \"Administration: read\" (classic tokens: repo) on each one.",
                repos.len() - failed.len(),
                repos.len(),
                reasons.join("; ")
            )));
        }
        runners.sort_by(|a, b| (a.id, &a.repository).cmp(&(b.id, &b.repository)));
        Ok(runners)
    }

    async fn runs(&self, org: &str, repo: &str, status: &str) -> Result<Collection<u64>, GithubError> {
        #[derive(Deserialize)]
        struct Run {
            id: u64,
        }
        let runs = self
            .collection::<Run>(
                &format!("/repos/{org}/{repo}/actions/runs?status={status}"),
                "workflow_runs",
                50,
                MAX_RUN_PAGES,
            )
            .await?;
        Ok(Collection {
            items: runs.items.into_iter().map(|r| r.id).collect(),
            truncated: runs.truncated,
        })
    }

    async fn run_jobs(&self, org: &str, repo: &str, run_id: u64) -> Result<Collection<GhJob>, GithubError> {
        let jobs = self
            .collection::<RawJob>(
                &format!("/repos/{org}/{repo}/actions/runs/{run_id}/jobs?filter=latest"),
                "jobs",
                100,
                MAX_JOB_PAGES,
            )
            .await?;
        let full = format!("{org}/{repo}");
        Ok(Collection {
            items: jobs.items.into_iter().map(|j| j.into_model(&full)).collect(),
            truncated: jobs.truncated,
        })
    }

    /// Queued and in-progress jobs across an organization's repos in scope.
    pub async fn scan_jobs(&self, org: &str, scope: &RepoScope) -> Result<JobScan, GithubError> {
        self.scan_account_jobs(org, AccountKind::Organization, scope).await
    }

    /// Queued and in-progress jobs across the repos in scope, for either kind of account.
    pub async fn scan_account_jobs(
        &self,
        org: &str,
        kind: AccountKind,
        scope: &RepoScope,
    ) -> Result<JobScan, GithubError> {
        let mut truncated = false;
        let repos = match scope {
            RepoScope::None => return Ok(JobScan::default()),
            RepoScope::Selected { repos } => unique_repos(repos),
            RepoScope::Recent { max_repos } => self.account_repos(org, kind, *max_repos as usize).await?,
        };

        let results: Vec<Result<Collection<GhJob>, GithubError>> = stream::iter(repos.iter().cloned())
            .map(|repo| async move {
                let active = self.runs(org, &repo, "in_progress").await?;
                let queued = self.runs(org, &repo, "queued").await?;
                let mut truncated = active.truncated || queued.truncated;
                let mut run_ids = active.items;
                run_ids.extend(queued.items);
                run_ids.sort_unstable();
                run_ids.dedup();
                let mut jobs = Vec::new();
                for id in run_ids {
                    let batch = self.run_jobs(org, &repo, id).await?;
                    truncated |= batch.truncated;
                    jobs.extend(
                        batch
                            .items
                            .into_iter()
                            .filter(|j| j.status == "queued" || j.status == "in_progress"),
                    );
                }
                Ok(Collection { items: jobs, truncated })
            })
            .buffer_unordered(CONCURRENCY)
            .collect()
            .await;

        let mut jobs = Vec::new();
        let mut first_err = None;
        for r in results {
            match r {
                Ok(j) => {
                    truncated |= j.truncated;
                    jobs.extend(j.items);
                }
                // A repo we can't read (e.g. token not granted to it) shouldn't
                // hide the others; mark the scan incomplete instead.
                Err(GithubError::NotFound(_)) | Err(GithubError::Forbidden(_)) => truncated = true,
                Err(e) => first_err = first_err.or(Some(e)),
            }
        }
        if let Some(e) = first_err {
            return Err(e);
        }
        jobs.sort_by_key(|j| j.id);
        Ok(JobScan {
            repos_scanned: repos,
            truncated,
            jobs,
        })
    }
}

fn hint_runner_permission(e: GithubError, org: &str) -> GithubError {
    match e {
        GithubError::Forbidden(msg) | GithubError::NotFound(msg) => GithubError::Forbidden(format!(
            "{msg}. Listing {org}'s runners needs a fine-grained token with the organization permission \"Self-hosted runners: read\" (classic tokens: admin:org), from an org owner."
        )),
        other => other,
    }
}

/// `repos` with repeats removed. GitHub repository names are case-insensitive,
/// and a saved scope can list one twice, which would read it twice.
fn unique_repos(repos: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    repos
        .iter()
        .filter(|repo| seen.insert(repo.to_lowercase()))
        .cloned()
        .collect()
}

fn hint_repo_runner_permission(e: GithubError, owner: &str) -> GithubError {
    match e {
        GithubError::Forbidden(msg) | GithubError::NotFound(msg) => GithubError::Forbidden(format!(
            "{msg}. Listing runners in {owner}'s repositories needs a fine-grained token with the repository permission \"Administration: read\" (classic tokens: repo)."
        )),
        other => other,
    }
}

fn decode<T: for<'de> Deserialize<'de>>(v: Value) -> Result<T, GithubError> {
    serde_json::from_value(v).map_err(|e| GithubError::Decode(e.to_string()))
}

#[derive(Deserialize)]
struct RawRunner {
    id: u64,
    name: String,
    #[serde(default)]
    os: String,
    status: String,
    busy: bool,
    #[serde(default)]
    labels: Vec<RawLabel>,
    ephemeral: Option<bool>,
    version: Option<String>,
    runner_group_id: Option<u64>,
}

#[derive(Deserialize)]
struct RawLabel {
    name: String,
}

impl RawRunner {
    fn into_model(self) -> GhRunner {
        GhRunner {
            id: self.id,
            name: self.name,
            os: self.os,
            status: self.status,
            busy: self.busy,
            labels: self.labels.into_iter().map(|l| l.name).collect(),
            ephemeral: self.ephemeral,
            version: self.version,
            runner_group_id: self.runner_group_id,
            repository: None,
        }
    }
}

#[derive(Deserialize)]
struct RawJob {
    id: u64,
    run_id: u64,
    name: String,
    workflow_name: Option<String>,
    status: String,
    #[serde(default)]
    labels: Vec<String>,
    runner_id: Option<u64>,
    runner_name: Option<String>,
    created_at: Option<DateTime<Utc>>,
    started_at: Option<DateTime<Utc>>,
    html_url: Option<String>,
    head_branch: Option<String>,
}

impl RawJob {
    fn into_model(self, repo: &str) -> GhJob {
        GhJob {
            id: self.id,
            run_id: self.run_id,
            repo: repo.to_string(),
            name: self.name,
            workflow_name: self.workflow_name,
            status: self.status,
            labels: self.labels,
            runner_id: self.runner_id.filter(|id| *id != 0),
            runner_name: self.runner_name.filter(|n| !n.is_empty()),
            created_at: self.created_at,
            started_at: self.started_at,
            html_url: self.html_url,
            head_branch: self.head_branch,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::{Path, Query};
    use axum::http::{HeaderMap as AxHeaders, StatusCode as AxStatus};
    use axum::routing::get;
    use axum::{Json, Router};
    use serde_json::json;

    use crate::test_server::serve;

    fn runner(id: u64, name: &str, busy: bool) -> Value {
        json!({"id": id, "name": name, "os": "macOS", "status": "online", "busy": busy,
               "labels": [{"id": 1, "name": "self-hosted", "type": "read-only"}, {"id": 2, "name": "macOS", "type": "read-only"}],
               "ephemeral": false, "runner_group_id": 1})
    }

    #[tokio::test]
    async fn paginates_runners_and_sends_version_header() {
        let router = Router::new().route(
            "/orgs/{org}/actions/runners",
            get(
                |headers: AxHeaders, Query(q): Query<HashMap<String, String>>| async move {
                    assert_eq!(headers.get("x-github-api-version").unwrap(), API_VERSION);
                    assert_eq!(headers.get("authorization").unwrap(), "Bearer t0k");
                    let page: u64 = q["page"].parse().unwrap();
                    let runners = if page == 1 {
                        (1..=100)
                            .map(|i| runner(i, &format!("r{i}"), false))
                            .collect::<Vec<_>>()
                    } else {
                        vec![runner(101, "r101", true)]
                    };
                    let mut h = AxHeaders::new();
                    h.insert("x-ratelimit-limit", "5000".parse().unwrap());
                    h.insert("x-ratelimit-remaining", "4990".parse().unwrap());
                    (h, Json(json!({"total_count": 101, "runners": runners})))
                },
            ),
        );
        let base = serve(router).await;
        let client = GithubClient::new(&base, "t0k");
        let runners = client.org_runners("acme").await.unwrap();
        assert_eq!(runners.len(), 101);
        assert!(runners[100].busy);
        assert_eq!(runners[0].labels, vec!["self-hosted", "macOS"]);
        assert_eq!(client.rate_limit().unwrap().remaining, 4990);
    }

    #[test]
    fn etag_cache_evicts_the_least_recently_used_page() {
        let page = || ApiPage {
            body: Value::Null,
            has_next: false,
        };
        let mut cache = EtagCache::default();
        for i in 0..MAX_CACHED_PAGES {
            cache.insert(format!("/page/{i}"), format!("\"{i}\""), page());
        }
        assert!(cache.get("/page/0").is_some());
        cache.insert("/page/new".into(), "\"new\"".into(), page());
        assert_eq!(cache.pages.len(), MAX_CACHED_PAGES);
        assert_eq!(cache.order.len(), MAX_CACHED_PAGES);
        assert!(cache.get("/page/0").is_some(), "recently used pages stay");
        assert!(cache.get("/page/1").is_none(), "the least recently used page goes");
        assert!(cache.get("/page/new").is_some());
    }

    #[tokio::test]
    async fn forbidden_runner_list_explains_permission() {
        let router = Router::new().route(
            "/orgs/{org}/actions/runners",
            get(|| async {
                (
                    AxStatus::FORBIDDEN,
                    Json(json!({"message": "Resource not accessible by personal access token"})),
                )
            }),
        );
        let base = serve(router).await;
        let err = GithubClient::new(&base, "t").org_runners("acme").await.unwrap_err();
        let GithubError::Forbidden(msg) = err else {
            panic!("{err:?}")
        };
        assert!(msg.contains("Self-hosted runners: read"));
    }

    #[tokio::test]
    async fn scans_jobs_across_recent_repos() {
        let router = Router::new()
            .route(
                "/orgs/{org}/repos",
                get(|| async {
                    Json(json!([{"name": "web", "archived": false}, {"name": "old", "archived": true}, {"name": "api"}]))
                }),
            )
            .route(
                "/repos/{org}/{repo}/actions/runs",
                get(|Path((_org, repo)): Path<(String, String)>, Query(q): Query<HashMap<String, String>>| async move {
                    let runs = match (repo.as_str(), q["status"].as_str()) {
                        ("web", "in_progress") => json!([{"id": 11}]),
                        ("web", "queued") => json!([{"id": 11}, {"id": 12}]),
                        _ => json!([]),
                    };
                    Json(json!({"total_count": 0, "workflow_runs": runs}))
                }),
            )
            .route(
                "/repos/{org}/{repo}/actions/runs/{run}/jobs",
                get(|Path((_o, _r, run)): Path<(String, String, u64)>| async move {
                    let jobs = if run == 11 {
                        json!([
                            {"id": 111, "run_id": 11, "name": "build", "workflow_name": "CI", "status": "in_progress",
                             "labels": ["self-hosted", "macOS"], "runner_id": 7, "runner_name": "mini",
                             "created_at": "2026-10-03T10:00:00Z", "started_at": "2026-10-03T10:01:00Z"},
                            {"id": 112, "run_id": 11, "name": "lint", "status": "completed", "labels": ["ubuntu-latest"]}
                        ])
                    } else {
                        json!([{"id": 121, "run_id": 12, "name": "test", "status": "queued",
                                "labels": ["self-hosted", "linux"], "runner_id": null, "runner_name": null,
                                "created_at": "2026-10-03T10:02:00Z"}])
                    };
                    Json(json!({"total_count": 0, "jobs": jobs}))
                }),
            );
        let base = serve(router).await;
        let scan = GithubClient::new(&base, "t")
            .scan_jobs("acme", &RepoScope::Recent { max_repos: 10 })
            .await
            .unwrap();
        assert_eq!(scan.repos_scanned, vec!["web", "api"]);
        assert_eq!(scan.jobs.iter().map(|j| j.id).collect::<Vec<_>>(), vec![111, 121]);
        assert_eq!(scan.jobs[0].runner_id, Some(7));
        assert_eq!(scan.jobs[0].repo, "acme/web");
        assert_eq!(scan.jobs[1].runner_id, None);
        assert!(!scan.truncated);
    }

    #[tokio::test]
    async fn personal_account_reads_runners_and_jobs_per_repository() {
        let router = Router::new()
            .route("/users/{login}", get(|| async { Json(json!({"login": "solo", "type": "User"})) }))
            .route(
                "/user/repos",
                get(|| async {
                    Json(json!([
                        {"name": "app", "owner": {"login": "solo"}},
                        {"name": "site", "owner": {"login": "Solo"}},
                        {"name": "old", "owner": {"login": "solo"}, "archived": true},
                        {"name": "theirs", "owner": {"login": "someone-else"}}
                    ]))
                }),
            )
            .route(
                "/repos/{owner}/{repo}/actions/runners",
                get(|Path((_owner, repo)): Path<(String, String)>| async move {
                    match repo.as_str() {
                        "app" => (AxStatus::OK, Json(json!({"total_count": 2, "runners": [runner(1, "a", false), runner(2, "app-02", true)]}))),
                        "site" => (AxStatus::OK, Json(json!({"total_count": 1, "runners": [runner(2, "site-2", true)]}))),
                        _ => (AxStatus::NOT_FOUND, Json(json!({"message": "Not Found"}))),
                    }
                }),
            )
            .route(
                "/repos/{owner}/{repo}/actions/runs",
                get(|| async { Json(json!({"total_count": 0, "workflow_runs": []})) }),
            )
            .fallback(|| async { (AxStatus::IM_A_TEAPOT, "organization endpoints are not used") });
        let base = serve(router).await;
        let client = GithubClient::new(&base, "t");
        assert_eq!(client.account_kind("solo").await, AccountKind::User);
        let runners = client
            .runners("solo", AccountKind::User, &RepoScope::Recent { max_repos: 10 })
            .await
            .unwrap();
        assert_eq!(
            runners
                .iter()
                .map(|r| (r.id, r.name.as_str(), r.repository.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                (1, "a", Some("solo/app")),
                (2, "app-02", Some("solo/app")),
                (2, "site-2", Some("solo/site")),
            ]
        );
        let scan = client
            .scan_account_jobs("solo", AccountKind::User, &RepoScope::Recent { max_repos: 10 })
            .await
            .unwrap();
        assert_eq!(scan.repos_scanned, vec!["app", "site"]);
    }

    #[tokio::test]
    async fn personal_account_explains_repository_permission() {
        let router = Router::new().route(
            "/repos/{owner}/{repo}/actions/runners",
            get(|| async { (AxStatus::FORBIDDEN, Json(json!({"message": "Must have admin rights"}))) }),
        );
        let base = serve(router).await;
        let scope = RepoScope::Selected {
            repos: vec!["app".into()],
        };
        let err = GithubClient::new(&base, "t")
            .user_runners("solo", &scope)
            .await
            .unwrap_err();
        let GithubError::Forbidden(msg) = err else {
            panic!("{err:?}")
        };
        assert!(msg.contains("Administration: read"));
    }

    #[tokio::test]
    async fn a_repository_listed_twice_is_read_once() {
        let router = Router::new()
            .route(
                "/repos/{owner}/{repo}/actions/runners",
                get(|Path((_owner, repo)): Path<(String, String)>| async move {
                    Json(json!({"total_count": 1, "runners": [runner(7, &format!("{repo}-7"), false)]}))
                }),
            )
            .route(
                "/repos/{owner}/{repo}/actions/runs",
                get(|Query(q): Query<HashMap<String, String>>| async move {
                    let runs = if q["status"] == "in_progress" {
                        json!([{"id": 1}])
                    } else {
                        json!([])
                    };
                    Json(json!({"total_count": 0, "workflow_runs": runs}))
                }),
            )
            .route(
                "/repos/{owner}/{repo}/actions/runs/{run}/jobs",
                get(|| async {
                    Json(
                        json!({"total_count": 1, "jobs": [{"id": 1, "run_id": 1, "name": "build",
                        "status": "in_progress", "labels": ["self-hosted"], "runner_id": 7}]}),
                    )
                }),
            );
        let base = serve(router).await;
        let client = GithubClient::new(&base, "t");
        let scope = RepoScope::Selected {
            repos: vec!["app".into(), "App".into(), "site".into()],
        };
        let runners = client.user_runners("solo", &scope).await.unwrap();
        assert_eq!(
            runners.iter().map(|r| r.repository.as_deref()).collect::<Vec<_>>(),
            vec![Some("solo/app"), Some("solo/site")]
        );
        let scan = client
            .scan_account_jobs("solo", AccountKind::User, &scope)
            .await
            .unwrap();
        assert_eq!(scan.repos_scanned, vec!["app", "site"]);
        assert_eq!(scan.jobs.len(), 2, "one in-progress job per repository, read once");
    }

    #[tokio::test]
    async fn personal_account_partial_runner_read_is_an_error() {
        let router = Router::new().route(
            "/repos/{owner}/{repo}/actions/runners",
            get(|Path((_owner, repo)): Path<(String, String)>| async move {
                match repo.as_str() {
                    "app" => (
                        AxStatus::OK,
                        Json(json!({"total_count": 1, "runners": [runner(1, "a", false)]})),
                    ),
                    "locked" => (AxStatus::FORBIDDEN, Json(json!({"message": "Must have admin rights"}))),
                    _ => (AxStatus::NOT_FOUND, Json(json!({"message": "Not Found"}))),
                }
            }),
        );
        let base = serve(router).await;
        let scope = RepoScope::Selected {
            repos: vec!["app".into(), "locked".into(), "gone".into()],
        };
        let err = GithubClient::new(&base, "t")
            .user_runners("solo", &scope)
            .await
            .unwrap_err();
        let GithubError::Incomplete(msg) = err else {
            panic!("{err:?}")
        };
        assert!(msg.contains("1 of 3 repositories"), "{msg}");
        assert!(msg.contains("solo/locked: GitHub refused access (403)"), "{msg}");
        assert!(msg.contains("solo/gone: Not found (404)"), "{msg}");
    }

    #[tokio::test]
    async fn unknown_account_kind_keeps_organization_mode() {
        let base = serve(Router::new()).await;
        assert_eq!(
            GithubClient::new(&base, "t").account_kind("acme").await,
            AccountKind::Organization
        );
    }
}
