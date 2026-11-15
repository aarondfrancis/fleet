//! Read-only GitHub REST client for org runners and in-flight jobs.
//!
//! - Runners: `GET /orgs/{org}/actions/runners` (fine-grained PAT: org
//!   "Self-hosted runners: read"; classic: `admin:org`).
//! - Jobs: there is no org-wide jobs endpoint, so we list
//!   `GET /repos/{o}/{r}/actions/runs?status=queued|in_progress` per repo in scope,
//!   then `GET .../runs/{id}/jobs` (repo "Actions: read").
//! - Every GET is conditional (ETag); a 304 doesn't count against the rate limit.
//! - After a primary or secondary rate limit, requests fail fast until the reset or retry time.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use futures::stream::{self, StreamExt};
use reqwest::StatusCode;
use reqwest::header::{ACCEPT, ETAG, HeaderMap, IF_NONE_MATCH, LINK, USER_AGENT};
use serde::Deserialize;
use serde_json::Value;

use crate::model::{GhJob, GhRunner, JobScan, RateLimit, RepoScope};
use crate::sync::MutexExt;

pub const API_VERSION: &str = "2026-03-10";
/// Hard cap on repos scanned per org per cycle, whatever the scope says.
pub const MAX_REPOS: usize = 50;
const CONCURRENCY: usize = 4;
// Bound each collection so a busy repository cannot monopolize a poll cycle.
const MAX_RUN_PAGES: usize = 4;
const MAX_JOB_PAGES: usize = 10;
const MAX_RUNNER_PAGES: usize = 20;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// Conditional responses kept per client. Job pages are keyed by run, so old runs must age out.
const MAX_CACHED_PAGES: usize = 512;
/// Rate-limit pauses last at least a minute (GitHub's advice without `retry-after`, and a guard
/// against a clock ahead of GitHub's) and at most an hour (a guard against garbled headers).
const MIN_PAUSE: TimeDelta = TimeDelta::minutes(1);
const MAX_PAUSE: TimeDelta = TimeDelta::hours(1);

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
    token: Mutex<String>,
    /// Token re-reads requested, and the latest request a stored token satisfies.
    token_requested: AtomicU64,
    token_read: AtomicU64,
    cache: Mutex<EtagCache>,
    rate: Mutex<Option<RateLimit>>,
    paused_until: Mutex<Option<DateTime<Utc>>>,
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
            token: Mutex::new(token.to_string()),
            token_requested: AtomicU64::new(0),
            token_read: AtomicU64::new(0),
            cache: Mutex::new(EtagCache::default()),
            rate: Mutex::new(None),
            paused_until: Mutex::new(None),
        }
    }

    /// Ask the owner to read the token again before the next poll; the ETag cache is kept.
    pub fn expire_token(&self) {
        self.token_requested.fetch_add(1, Ordering::SeqCst);
    }

    /// The pending token re-read request, to pass to [`Self::set_token`] once the token is read.
    pub fn token_request(&self) -> Option<u64> {
        let requested = self.token_requested.load(Ordering::SeqCst);
        (requested > self.token_read.load(Ordering::SeqCst)).then_some(requested)
    }

    /// Swap in a token read for `request`, keeping the ETag cache and any rate-limit pause.
    /// A request made while it was being read stays pending.
    pub fn set_token(&self, token: &str, request: u64) {
        *self.token.locked() = token.to_string();
        self.token_read.fetch_max(request, Ordering::SeqCst);
    }

    /// Extend the pause to `until`, or the shortest pause, within [`MIN_PAUSE`]..[`MAX_PAUSE`]
    /// from now. It only ever grows and ends by time.
    fn pause(&self, until: Option<DateTime<Utc>>) {
        let now = Utc::now();
        let until = until.unwrap_or(now).clamp(now + MIN_PAUSE, now + MAX_PAUSE);
        let mut paused = self.paused_until.locked();
        *paused = Some(paused.map_or(until, |p| p.max(until)));
    }

    /// The error to return while a pause lasts.
    fn paused(&self) -> Option<GithubError> {
        let until = self.paused_until.locked().filter(|until| *until > Utc::now())?;
        Some(GithubError::RateLimited(until.format("%H:%M UTC").to_string()))
    }

    pub fn rate_limit(&self) -> Option<RateLimit> {
        self.rate.locked().clone()
    }

    async fn get(&self, path_and_query: &str) -> Result<ApiPage, GithubError> {
        if let Some(error) = self.paused() {
            return Err(error);
        }
        let url = format!("{}{}", self.api_base, path_and_query);
        let token = self.token.locked().clone();
        let cached = self.cache.locked().get(&url);
        let mut req = self
            .http
            .get(&url)
            .header(ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION)
            .header(USER_AGENT, "runner-dashboard")
            .bearer_auth(token);
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
            let retry_after = header_number(resp.headers(), "retry-after");
            let body = resp.text().await.unwrap_or_default();
            let msg = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| v["message"].as_str().map(str::to_string))
                .unwrap_or(body);
            // A primary limit exhausts the quota (record_rate paused already); secondary ones
            // send retry-after or only say so.
            let text = msg.to_lowercase();
            let limited = status == StatusCode::TOO_MANY_REQUESTS
                || (status == StatusCode::FORBIDDEN
                    && (retry_after.is_some()
                        || self.rate_limit().is_some_and(|r| r.remaining == 0)
                        || text.contains("rate limit")
                        || text.contains("abuse detection")));
            if limited {
                self.pause(
                    retry_after.map(|secs| Utc::now() + TimeDelta::seconds(secs.clamp(0, MAX_PAUSE.num_seconds()))),
                );
                if let Some(error) = self.paused() {
                    return Err(error);
                }
            }
            return Err(match status.as_u16() {
                401 => GithubError::Unauthorized,
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

    /// Keep the latest primary rate limit; an exhausted one pauses requests until it resets.
    fn record_rate(&self, headers: &HeaderMap) {
        let count = |name: &str| header_number(headers, name).and_then(|v| u32::try_from(v).ok());
        if let (Some(limit), Some(remaining)) = (count("x-ratelimit-limit"), count("x-ratelimit-remaining")) {
            let resets_at = header_number(headers, "x-ratelimit-reset").and_then(|t| DateTime::from_timestamp(t, 0));
            if remaining == 0 {
                self.pause(resets_at);
            }
            *self.rate.locked() = Some(RateLimit {
                limit,
                remaining,
                resets_at,
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

    pub async fn recent_repos(&self, org: &str, max: usize) -> Result<Vec<String>, GithubError> {
        #[derive(Deserialize)]
        struct Repo {
            name: String,
            #[serde(default)]
            archived: bool,
        }
        if max == 0 {
            return Ok(Vec::new());
        }
        let per_page = max.clamp(1, 100);
        let v = self
            .get(&format!(
                "/orgs/{org}/repos?sort=pushed&direction=desc&per_page={per_page}"
            ))
            .await?;
        let repos: Vec<Repo> = decode(v.body)?;
        Ok(repos
            .into_iter()
            .filter(|r| !r.archived)
            .map(|r| r.name)
            .take(max)
            .collect())
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

    /// Queued and in-progress jobs across the repos in scope.
    pub async fn scan_jobs(&self, org: &str, scope: &RepoScope) -> Result<JobScan, GithubError> {
        let (repos, mut truncated) = match scope {
            RepoScope::None => return Ok(JobScan::default()),
            RepoScope::Selected { repos } => (
                repos.iter().take(MAX_REPOS).cloned().collect::<Vec<_>>(),
                repos.len() > MAX_REPOS,
            ),
            RepoScope::Recent { max_repos } => {
                let max = (*max_repos as usize).min(MAX_REPOS);
                (self.recent_repos(org, max).await?, (*max_repos as usize) > MAX_REPOS)
            }
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

fn header_number(headers: &HeaderMap, name: &str) -> Option<i64> {
    headers.get(name)?.to_str().ok()?.trim().parse().ok()
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
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

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

    /// A runner listing that always answers `status` with `headers`, counting requests.
    async fn answering(
        status: AxStatus,
        headers: Vec<(&'static str, String)>,
        message: &'static str,
    ) -> (GithubClient, Arc<AtomicUsize>) {
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        let router = Router::new().route(
            "/orgs/{org}/actions/runners",
            get(move || {
                counter.fetch_add(1, Ordering::SeqCst);
                let mut h = AxHeaders::new();
                for (name, value) in &headers {
                    h.insert(*name, value.parse().unwrap());
                }
                async move {
                    (
                        status,
                        h,
                        Json(json!({"message": message, "total_count": 0, "runners": []})),
                    )
                }
            }),
        );
        (GithubClient::new(&serve(router).await, "t"), hits)
    }

    fn exhausted(reset: DateTime<Utc>) -> Vec<(&'static str, String)> {
        vec![
            ("x-ratelimit-limit", "5000".into()),
            ("x-ratelimit-remaining", "0".into()),
            ("x-ratelimit-reset", reset.timestamp().to_string()),
        ]
    }

    /// Asserts the client is paused for `min..=max` from now and sends nothing meanwhile.
    async fn assert_paused(client: &GithubClient, hits: &AtomicUsize, min: TimeDelta, max: TimeDelta) {
        let sent = hits.load(Ordering::SeqCst);
        let err = client.org_runners("acme").await.unwrap_err();
        assert!(matches!(err, GithubError::RateLimited(_)), "{err:?}");
        assert_eq!(hits.load(Ordering::SeqCst), sent, "no requests while paused");
        let until = client.paused_until.locked().unwrap() - Utc::now();
        assert!(until > min - TimeDelta::seconds(5) && until <= max, "{until}");
    }

    #[tokio::test]
    async fn exhausted_primary_limit_pauses_until_reset() {
        let reset = Utc::now() + TimeDelta::minutes(10);
        let (client, hits) = answering(AxStatus::OK, exhausted(reset), "").await;
        assert!(
            client.org_runners("acme").await.is_ok(),
            "the last request still succeeds"
        );
        assert_paused(&client, &hits, TimeDelta::minutes(10), TimeDelta::minutes(10)).await;
        client.pause(None);
        assert_paused(&client, &hits, TimeDelta::minutes(10), TimeDelta::minutes(10)).await;
    }

    #[tokio::test]
    async fn forbidden_with_no_quota_left_is_a_rate_limit_even_after_the_local_reset() {
        let (client, hits) = answering(AxStatus::FORBIDDEN, exhausted(Utc::now() + MAX_PAUSE), "Forbidden").await;
        assert!(matches!(
            client.org_runners("acme").await,
            Err(GithubError::RateLimited(_))
        ));
        assert_paused(&client, &hits, MAX_PAUSE, MAX_PAUSE).await;
        // This Mac's clock is ahead of GitHub's: the reset has passed here but not there.
        let past = Utc::now() - TimeDelta::minutes(10);
        let (client, hits) = answering(AxStatus::FORBIDDEN, exhausted(past), "Forbidden").await;
        assert!(matches!(
            client.org_runners("acme").await,
            Err(GithubError::RateLimited(_))
        ));
        assert_paused(&client, &hits, MIN_PAUSE, MIN_PAUSE).await;
    }

    #[tokio::test]
    async fn secondary_limits_pause_within_bounds() {
        let retry = |secs: &str| vec![("retry-after", secs.to_string())];
        let cases = [
            (
                AxStatus::FORBIDDEN,
                retry("120"),
                "You have exceeded a secondary rate limit.",
                TimeDelta::minutes(2),
            ),
            (
                AxStatus::FORBIDDEN,
                vec![],
                "You have triggered an abuse detection mechanism.",
                MIN_PAUSE,
            ),
            (AxStatus::TOO_MANY_REQUESTS, vec![], "", MIN_PAUSE),
            (AxStatus::TOO_MANY_REQUESTS, retry(&i64::MAX.to_string()), "", MAX_PAUSE),
        ];
        for (status, headers, message, pause) in cases {
            let (client, hits) = answering(status, headers, message).await;
            let err = client.org_runners("acme").await.unwrap_err();
            assert!(matches!(err, GithubError::RateLimited(_)), "{message}: {err:?}");
            assert_paused(&client, &hits, pause, pause).await;
        }
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
}
