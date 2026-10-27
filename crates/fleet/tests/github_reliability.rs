//! GitHub pagination, independent poll failures, and secret-store failures use only local fakes.
#![expect(clippy::unwrap_used, reason = "test fixtures fail the test loudly")]

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{Path, Query};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use chrono::Utc;
use fleet::engine::{Engine, MemorySecrets, SecretStore};
use fleet::github::{GithubClient, GithubError};
use fleet::model::{AppConfig, OrgAuth, OrgConfig, RepoScope};
use serde_json::{Value, json};

mod common;
use common::serve;

fn scope() -> RepoScope {
    RepoScope::Selected {
        repos: vec!["web".into()],
    }
}

fn job(id: u64, run: u64, status: &str) -> Value {
    json!({"id": id, "run_id": run, "name": "build", "status": status, "labels": ["self-hosted"]})
}

#[tokio::test]
async fn total_counts_paginate_runs_and_jobs_before_filtering_completed_jobs() {
    let app = Router::new()
        .route("/repos/{org}/{repo}/actions/runs", get(|Query(q): Query<HashMap<String, String>>| async move {
            let page: u64 = q["page"].parse().unwrap();
            if q["status"] == "queued" {
                Json(json!({"total_count": 0, "workflow_runs": []}))
            } else {
                assert!(page <= 2);
                Json(json!({"total_count": 2, "workflow_runs": [{"id": page}]}))
            }
        }))
        .route("/repos/{org}/{repo}/actions/runs/{run}/jobs", get(
            |Path((_o, _r, run)): Path<(String, String, u64)>, Query(q): Query<HashMap<String, String>>| async move {
                let page: u64 = q["page"].parse().unwrap();
                assert!(page <= 2);
                Json(json!({"total_count": 2, "jobs": [job(run * 10 + page, run, if page == 1 {"completed"} else {"queued"})]}))
            },
        ));
    let base = serve(app).await;
    let scan = GithubClient::new(&base, "fake")
        .scan_jobs("acme", &scope())
        .await
        .unwrap();
    assert_eq!(scan.jobs.iter().map(|job| job.id).collect::<Vec<_>>(), vec![12, 22]);
    assert_eq!(scan.repos_scanned, vec!["web"]);
    assert!(!scan.truncated);
}

#[tokio::test]
async fn link_pagination_survives_conditional_cached_responses() {
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = hits.clone();
    let app = Router::new()
        .route(
            "/repos/{org}/{repo}/actions/runs",
            get(move |headers: HeaderMap, Query(q): Query<HashMap<String, String>>| {
                let hits = seen.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    if headers.contains_key("if-none-match") {
                        return StatusCode::NOT_MODIFIED.into_response();
                    }
                    let page: u64 = q["page"].parse().unwrap();
                    let mut headers = HeaderMap::new();
                    headers.insert("etag", "\"cached\"".parse().unwrap());
                    let runs = if q["status"] == "in_progress" {
                        if page == 1 {
                            // Only the continuation signal is used; authorization never follows this URL.
                            headers.insert(
                                "link",
                                "<https://untrusted.invalid/page2>; rel=\"next\"".parse().unwrap(),
                            );
                        }
                        assert!(page <= 2);
                        json!([{"id": page}])
                    } else {
                        json!([])
                    };
                    (headers, Json(json!({"workflow_runs": runs}))).into_response()
                }
            }),
        )
        .route(
            "/repos/{org}/{repo}/actions/runs/{run}/jobs",
            get(|Path((_o, _r, run)): Path<(String, String, u64)>| async move {
                Json(json!({"total_count": 1, "jobs": [job(run, run, "queued")]}))
            }),
        );
    let base = serve(app).await;
    let client = GithubClient::new(&base, "fake");
    let first = client.scan_jobs("acme", &scope()).await.unwrap();
    let second = client.scan_jobs("acme", &scope()).await.unwrap();
    assert_eq!(first.jobs.iter().map(|job| job.id).collect::<Vec<_>>(), vec![1, 2]);
    assert!(!first.truncated);
    assert_eq!(first, second);
    assert_eq!(hits.load(Ordering::SeqCst), 6);
}

#[tokio::test]
async fn repository_scope_limits_remain_intentional_and_hard_cap_is_visible() {
    let app = Router::new()
        .route(
            "/orgs/{org}/repos",
            get(|Query(q): Query<HashMap<String, String>>| async move {
                assert_eq!(q["per_page"], "2");
                Json(json!([{"name": "web"}, {"name": "api"}]))
            }),
        )
        .route(
            "/repos/{org}/{repo}/actions/runs",
            get(|| async { Json(json!({"total_count": 0, "workflow_runs": []})) }),
        );
    let base = serve(app).await;
    let client = GithubClient::new(&base, "fake");
    let recent = client
        .scan_jobs("acme", &RepoScope::Recent { max_repos: 2 })
        .await
        .unwrap();
    assert_eq!(recent.repos_scanned, vec!["web", "api"]);
    assert!(!recent.truncated, "configured recent scope is not an incomplete scan");
    let repos = (0..=fleet::github::MAX_REPOS).map(|id| format!("repo{id}")).collect();
    let capped = client.scan_jobs("acme", &RepoScope::Selected { repos }).await.unwrap();
    assert!(capped.truncated);
    assert_eq!(capped.repos_scanned.len(), fleet::github::MAX_REPOS);
    assert!(!capped.repos_scanned.contains(&"repo50".to_string()));
    // A disabled or zero repository scope should not make any HTTP requests.
    let offline = GithubClient::new("http://127.0.0.1:1", "fake");
    assert_eq!(
        offline.scan_jobs("acme", &RepoScope::None).await.unwrap(),
        Default::default()
    );
    assert_eq!(
        offline
            .scan_jobs("acme", &RepoScope::Recent { max_repos: 0 })
            .await
            .unwrap(),
        Default::default()
    );
}

async fn capped_scan(cap_runs: bool, cap_jobs: bool) -> (fleet::model::JobScan, usize, usize) {
    let run_hits = Arc::new(AtomicUsize::new(0));
    let job_hits = Arc::new(AtomicUsize::new(0));
    let runs = run_hits.clone();
    let jobs = job_hits.clone();
    let app = Router::new()
        .route(
            "/repos/{org}/{repo}/actions/runs",
            get(move |Query(q): Query<HashMap<String, String>>| {
                let hits = runs.clone();
                async move {
                    if q["status"] == "queued" {
                        return Json(json!({"total_count": 0, "workflow_runs": []}));
                    }
                    hits.fetch_add(1, Ordering::SeqCst);
                    Json(json!({"total_count": if cap_runs {10000} else {1}, "workflow_runs": [{"id": 1}]}))
                }
            }),
        )
        .route(
            "/repos/{org}/{repo}/actions/runs/{run}/jobs",
            get(move |Query(q): Query<HashMap<String, String>>| {
                let hits = jobs.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    let page: u64 = q["page"].parse().unwrap();
                    Json(json!({"total_count": if cap_jobs {10000} else {1}, "jobs": [job(page, 1, "queued")]}))
                }
            }),
        );
    let base = serve(app).await;
    let scan = GithubClient::new(&base, "fake")
        .scan_jobs("acme", &scope())
        .await
        .unwrap();
    (scan, run_hits.load(Ordering::SeqCst), job_hits.load(Ordering::SeqCst))
}

#[tokio::test]
async fn run_and_job_bounds_each_mark_scan_incomplete() {
    let (runs, run_hits, job_hits) = capped_scan(true, false).await;
    assert!(runs.truncated);
    assert_eq!(runs.jobs.len(), 1);
    assert_eq!((run_hits, job_hits), (4, 1));
    let (jobs, run_hits, job_hits) = capped_scan(false, true).await;
    assert!(jobs.truncated);
    assert_eq!(jobs.jobs.len(), 10);
    assert_eq!((run_hits, job_hits), (1, 10));
}

#[tokio::test]
async fn runner_bound_reports_error_instead_of_partial_success() {
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = hits.clone();
    let app = Router::new().route("/orgs/{org}/actions/runners", get(move || {
        let hits = seen.clone();
        async move {
            hits.fetch_add(1, Ordering::SeqCst);
            Json(json!({"total_count": 10000, "runners": [{"id": 1, "name": "fake", "status": "online", "busy": false}]}))
        }
    }));
    let base = serve(app).await;
    let error = GithubClient::new(&base, "fake").org_runners("acme").await.unwrap_err();
    assert!(matches!(error, GithubError::Incomplete(_)));
    assert_eq!(hits.load(Ordering::SeqCst), 20);
}

fn configured_engine(dir: &std::path::Path, base: String, secrets: Arc<dyn SecretStore>) -> Engine {
    let config = AppConfig {
        orgs: vec![OrgConfig {
            login: "acme".into(),
            api_base: base,
            repo_scope: scope(),
            added_at: Utc::now(),
            auth: OrgAuth::Token,
        }],
        ..Default::default()
    };
    let path = dir.join("config.json");
    std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    Engine::load(path, secrets).unwrap()
}

#[tokio::test]
async fn runner_and_job_errors_recover_independently_and_keep_last_good_data() {
    let runner_failed = Arc::new(AtomicBool::new(false));
    let job_failed = Arc::new(AtomicBool::new(false));
    let runners = runner_failed.clone();
    let jobs = job_failed.clone();
    let app = Router::new()
        .route("/orgs/{org}/actions/runners", get(move || {
            let failed = runners.clone();
            async move {
                if failed.load(Ordering::SeqCst) {
                    (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"message": "runner failure"})))
                } else {
                    (StatusCode::OK, Json(json!({"total_count": 1, "runners": [{"id": 7, "name": "fake", "status": "online", "busy": false}]})))
                }
            }
        }))
        .route("/repos/{org}/{repo}/actions/runs", get(move |Query(q): Query<HashMap<String, String>>| {
            let failed = jobs.clone();
            async move {
                if failed.load(Ordering::SeqCst) {
                    (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"message": "job failure"})))
                } else {
                    let ids = if q["status"] == "queued" { json!([{"id": 1}]) } else { json!([]) };
                    (StatusCode::OK, Json(json!({"total_count": ids.as_array().unwrap().len(), "workflow_runs": ids})))
                }
            }
        }))
        .route("/repos/{org}/{repo}/actions/runs/{run}/jobs", get(|| async {
            Json(json!({"total_count": 1, "jobs": [job(1, 1, "queued")]}))
        }));
    let base = serve(app).await;
    let dir = tempfile::tempdir().unwrap();
    let secrets = Arc::new(MemorySecrets::default());
    secrets.set("acme", "fake").unwrap();
    let engine = configured_engine(dir.path(), base, secrets);
    engine.poll_github_runners().await;
    engine.poll_github_jobs().await;
    let good = engine.view();
    assert!(good.queue.complete);
    let runner_at = good.orgs[0].runners_as_of;
    let job_at = good.orgs[0].jobs_as_of;

    runner_failed.store(true, Ordering::SeqCst);
    job_failed.store(true, Ordering::SeqCst);
    engine.poll_github_jobs().await;
    engine.poll_github_runners().await;
    let failed = engine.view();
    let error = failed.orgs[0].error.as_ref().unwrap();
    assert!(
        error.contains("runner failure") && error.contains("job failure"),
        "{error}"
    );
    assert_eq!(failed.orgs[0].runners_as_of, runner_at);
    assert_eq!(failed.orgs[0].jobs_as_of, job_at);
    assert_eq!(failed.queue.jobs.len(), 1);
    assert!(!failed.queue.complete);

    runner_failed.store(false, Ordering::SeqCst);
    engine.poll_github_runners().await;
    assert!(engine.view().orgs[0].error.as_ref().unwrap().contains("job failure"));
    job_failed.store(false, Ordering::SeqCst);
    engine.poll_github_jobs().await;
    assert!(engine.view().orgs[0].error.is_none());
    assert!(engine.view().queue.complete);

    runner_failed.store(true, Ordering::SeqCst);
    engine.poll_github_runners().await;
    engine.poll_github_jobs().await;
    assert!(engine.view().orgs[0].error.as_ref().unwrap().contains("runner failure"));
    runner_failed.store(false, Ordering::SeqCst);
    engine.poll_github_runners().await;
    assert!(engine.view().orgs[0].error.is_none());
}

struct BrokenSecrets {
    thread: Mutex<Option<std::thread::ThreadId>>,
}

impl SecretStore for BrokenSecrets {
    fn get(&self, _account: &str) -> Result<Option<String>, String> {
        *self.thread.lock().unwrap() = Some(std::thread::current().id());
        Err("Keychain is locked".into())
    }
    fn set(&self, _account: &str, _value: &str) -> Result<(), String> {
        unreachable!()
    }
    fn delete(&self, _account: &str) -> Result<(), String> {
        unreachable!()
    }
}

#[tokio::test(flavor = "current_thread")]
async fn secret_read_failures_preserve_cause_and_run_off_async_thread() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = Arc::new(BrokenSecrets {
        thread: Mutex::new(None),
    });
    let engine = configured_engine(dir.path(), "http://127.0.0.1:1".into(), secrets.clone());
    engine.poll_github_runners().await;
    let runner = engine.view();
    assert!(runner.orgs[0].has_token);
    assert!(runner.orgs[0].error.as_ref().unwrap().contains("Keychain is locked"));
    assert_ne!(secrets.thread.lock().unwrap().unwrap(), std::thread::current().id());
    engine.poll_github_jobs().await;
    let jobs = engine.view();
    assert!(jobs.orgs[0].has_token);
    assert!(jobs.orgs[0].error.as_ref().unwrap().contains("Jobs:"));
    assert_ne!(secrets.thread.lock().unwrap().unwrap(), std::thread::current().id());
}

#[tokio::test]
async fn absent_secret_is_reported_from_either_poll() {
    for jobs_first in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let engine = configured_engine(
            dir.path(),
            "http://127.0.0.1:1".into(),
            Arc::new(MemorySecrets::default()),
        );
        if jobs_first {
            engine.poll_github_jobs().await;
        } else {
            engine.poll_github_runners().await;
        }
        let view = engine.view();
        assert!(!view.orgs[0].has_token);
        assert_eq!(
            view.orgs[0].error.as_deref(),
            Some("No token in the Keychain for this organization.")
        );
    }
}

/// One org runner and run 5, in progress until `done`; then its jobs are completed, or the run
/// is gone (404) when `gone`. Counts reads of the run's jobs.
async fn fake_run(done: Arc<AtomicBool>, reads: Arc<AtomicUsize>, gone: bool) -> String {
    let runs_done = done.clone();
    let app = Router::new()
        .route("/orgs/{org}/actions/runners", get(|| async {
            Json(json!({"total_count": 1, "runners": [{"id": 7, "name": "fake", "status": "online", "busy": true, "labels": [{"name": "self-hosted"}]}]}))
        }))
        .route("/repos/{org}/{repo}/actions/runs", get(move |Query(q): Query<HashMap<String, String>>| {
            let done = runs_done.load(Ordering::SeqCst);
            async move {
                let runs = if q["status"] == "in_progress" && !done { json!([{"id": 5}]) } else { json!([]) };
                Json(json!({"total_count": runs.as_array().unwrap().len(), "workflow_runs": runs}))
            }
        }))
        .route("/repos/{org}/{repo}/actions/runs/{run}/jobs", get(move |Query(q): Query<HashMap<String, String>>| {
            reads.fetch_add(1, Ordering::SeqCst);
            let done = done.load(Ordering::SeqCst);
            async move {
                if !done {
                    assert_eq!(q["filter"], "latest", "the scan reads the latest attempt");
                } else {
                    assert_eq!(q["filter"], "all", "history reads every attempt");
                    if gone {
                        return (StatusCode::NOT_FOUND, Json(json!({"message": "Not Found"})));
                    }
                }
                let status = if done { "completed" } else { "in_progress" };
                let ago = |minutes| (Utc::now() - chrono::Duration::minutes(minutes)).to_rfc3339();
                let times = json!({"created_at": ago(6), "started_at": ago(5), "completed_at": ago(1)});
                let mut own = json!({"id": 51, "run_id": 5, "name": "build", "status": status, "conclusion": "success",
                    "labels": ["self-hosted"], "runner_id": 7, "runner_name": "fake"});
                let mut hosted = json!({"id": 52, "run_id": 5, "name": "lint", "status": "completed", "conclusion": "success",
                    "labels": ["ubuntu-latest"], "runner_id": 99, "runner_name": "GitHub Actions 1"});
                for job in [&mut own, &mut hosted] {
                    job.as_object_mut().unwrap().extend(times.as_object().unwrap().clone());
                }
                (StatusCode::OK, Json(json!({"total_count": 2, "jobs": [own, hosted]})))
            }
        }));
    serve(app).await
}

fn fake_secrets() -> Arc<MemorySecrets> {
    let secrets = Arc::new(MemorySecrets::default());
    secrets.set("acme", "fake").unwrap();
    secrets
}

fn recent_ids(engine: &Engine) -> Vec<u64> {
    engine.view().activity.recent.iter().map(|j| j.id).collect()
}

#[tokio::test]
async fn finished_runs_are_read_once_and_their_self_hosted_jobs_kept() {
    let (done, reads) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicUsize::new(0)));
    let base = fake_run(done.clone(), reads.clone(), false).await;
    let dir = tempfile::tempdir().unwrap();
    let engine = configured_engine(dir.path(), base.clone(), fake_secrets());
    engine.poll_github_runners().await;
    engine.poll_github_jobs().await;
    assert_eq!(reads.load(Ordering::SeqCst), 1, "the scan reads the in-flight run");

    done.store(true, Ordering::SeqCst);
    engine.poll_github_jobs().await;
    engine.poll_github_jobs().await;
    assert_eq!(reads.load(Ordering::SeqCst), 2, "a finished run is read once");
    assert_eq!(recent_ids(&engine), vec![51]);
    assert_eq!(
        recent_ids(&configured_engine(dir.path(), base, fake_secrets())),
        vec![51]
    );
}

#[tokio::test]
async fn a_deleted_run_is_dropped() {
    let (done, reads) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicUsize::new(0)));
    let base = fake_run(done.clone(), reads.clone(), true).await;
    let dir = tempfile::tempdir().unwrap();
    let engine = configured_engine(dir.path(), base, fake_secrets());
    engine.poll_github_runners().await;
    engine.poll_github_jobs().await;
    done.store(true, Ordering::SeqCst);
    engine.poll_github_jobs().await;
    engine.poll_github_jobs().await;
    assert_eq!(reads.load(Ordering::SeqCst), 2, "a 404 isn't retried");
    assert!(recent_ids(&engine).is_empty());
}

#[tokio::test]
async fn demo_mode_never_creates_job_history() {
    let (done, reads) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicUsize::new(0)));
    let base = fake_run(done.clone(), reads, false).await;
    let dir = tempfile::tempdir().unwrap();
    let engine = configured_engine(dir.path(), base, fake_secrets());
    engine.set_demo_mode(true).unwrap();
    engine.poll_github_runners().await;
    engine.poll_github_jobs().await;
    done.store(true, Ordering::SeqCst);
    engine.poll_github_jobs().await;
    assert!(!engine.view().activity.recent.is_empty(), "demo history is shown");
    assert!(!dir.path().join("job-history.json").exists());
}
