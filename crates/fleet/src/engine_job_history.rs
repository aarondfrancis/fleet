//! Following self-hosted runs that left the job scan, and recording their finished jobs.

use chrono::Utc;

use super::Engine;
use crate::github::GithubError;
use crate::model::RepoScope;
use crate::sync::{MutexExt, RwLockExt};

/// Run reads per organization per round, bounding API use after a busy period.
const RUNS_PER_ROUND: usize = 20;

impl Engine {
    /// Runs after each job poll. Uses only the clients that poll created; never in demo mode.
    pub(super) async fn update_job_history(&self) {
        let config = self.config();
        if config.demo_mode {
            return;
        }
        // A concurrent check already updates the same runs.
        let Ok(_round) = self.job_history_round.try_lock() else {
            return;
        };
        let now = Utc::now();
        let scanned: Vec<String> = config
            .orgs
            .iter()
            .filter(|o| o.repo_scope != RepoScope::None)
            .map(|o| o.login.to_lowercase())
            .collect();
        let rounds: Vec<_> = {
            let obs = self.obs.read_locked();
            let clients = self.github.locked();
            scanned
                .iter()
                .filter_map(|org| {
                    let observed = obs.github.get(org).filter(|o| o.jobs_error.is_none())?;
                    let scan = observed.jobs.as_ref()?.value.clone();
                    let runners = observed.runners.as_ref().map(|r| r.value.clone()).unwrap_or_default();
                    Some((org.clone(), clients.get(org)?.clone(), scan, runners))
                })
                .collect()
        };
        let due: Vec<_> = {
            let mut history = self.job_history.locked();
            if !rounds.is_empty() && rounds.len() == scanned.len() {
                history.cover(now);
            }
            rounds
                .iter()
                .map(|(org, _, scan, runners)| {
                    history.track(org, scan, runners, now);
                    history.due(org, scan, now, RUNS_PER_ROUND)
                })
                .collect()
        };
        let mut reads = Vec::new();
        for ((_, client, _, runners), runs) in rounds.iter().zip(due) {
            for (run_id, repo) in runs {
                let Some((owner, name)) = repo.split_once('/') else {
                    continue;
                };
                // Every attempt, so a quick re-run can't hide the first attempt's jobs.
                let result = client.run_jobs(owner, name, run_id, "all").await;
                let stop = matches!(&result, Err(e) if !matches!(e, GithubError::NotFound(_)));
                reads.push((run_id, runners.clone(), result));
                if stop {
                    // Leave this organization's budget to the regular polls until next round.
                    break;
                }
            }
        }
        let history = self.job_history.clone();
        let saved = tokio::task::spawn_blocking(move || {
            let save = {
                let mut history = history.locked();
                for (run_id, runners, result) in reads {
                    match result {
                        Ok(jobs) => history.read_run(run_id, &jobs.items, jobs.truncated, &runners, now),
                        Err(GithubError::NotFound(_)) => history.forget_run(run_id, now),
                        Err(e) => {
                            tracing::debug!(run = run_id, "job history read failed: {e}");
                            history.checked(run_id, now);
                        }
                    }
                }
                history.save_due(now)
            };
            // Written without the lock `view()` takes; this round's lock keeps writes in order.
            if let Some((path, bytes)) = save {
                let result = crate::atomic_write::atomic_write(&path, &bytes);
                history.locked().saved(result, now);
            }
        })
        .await;
        if let Err(e) = saved {
            tracing::warn!("saving job history stopped unexpectedly: {e}");
        }
    }
}
