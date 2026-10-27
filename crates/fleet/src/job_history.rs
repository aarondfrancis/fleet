//! Finished self-hosted jobs recorded on this Mac, and the numbers the Activity page shows.
//!
//! The job scan only sees queued and running jobs. Runs with a self-hosted job in flight are
//! remembered; when a run leaves the scan its jobs are read once, and the completed
//! self-hosted ones are kept in `job-history.json` (see `store`). Only GitHub reads are made.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::labels;
use crate::model::{GhJob, GhRunner};

pub(crate) mod demo;
mod store;
mod summary;
pub(crate) use store::JobHistory;

/// How a finished job ended. Timed-out and startup failures count as failures.
/// Skipped jobs never get a runner, so they're never recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum FleetJobConclusion {
    Success,
    Failure,
    Cancelled,
    Other,
}

impl FleetJobConclusion {
    fn from_github(value: Option<&str>) -> Self {
        match value {
            Some("success") => Self::Success,
            Some("failure" | "timed_out" | "startup_failure") => Self::Failure,
            Some("cancelled") => Self::Cancelled,
            _ => Self::Other,
        }
    }
}

/// The operating system a job ran on, for the GitHub-hosted cost estimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum FleetJobOs {
    Linux,
    Windows,
    Macos,
}

impl FleetJobOs {
    fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "linux" => Some(Self::Linux),
            "windows" => Some(Self::Windows),
            "macos" => Some(Self::Macos),
            _ => None,
        }
    }
}

/// A completed self-hosted job that a runner picked up.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct FleetFinishedJob {
    #[ts(type = "number")]
    pub id: u64,
    /// Organization login, lowercase.
    pub org: String,
    /// `owner/name`
    pub repo: String,
    pub name: String,
    pub workflow_name: Option<String>,
    pub labels: Vec<String>,
    pub conclusion: FleetJobConclusion,
    pub runner_name: Option<String>,
    pub os: Option<FleetJobOs>,
    pub created_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: DateTime<Utc>,
    pub html_url: Option<String>,
}

impl FleetFinishedJob {
    /// Seconds a runner spent on the job; zero if it never started.
    pub(crate) fn run_secs(&self) -> i64 {
        self.started_at
            .map_or(0, |at| (self.completed_at - at).num_seconds().max(0))
    }

    /// The record for a completed job, or `None` if it never got a runner (skipped, or
    /// cancelled while queued) or GitHub gave no completion time.
    fn from_github(org: &str, job: &GhJob, runners: &[GhRunner]) -> Option<Self> {
        if job.runner_id.is_none() && job.runner_name.is_none() {
            return None;
        }
        let runner = runners
            .iter()
            .find(|r| Some(r.id) == job.runner_id || job.runner_name.as_ref() == Some(&r.name));
        Some(Self {
            id: job.id,
            org: org.to_lowercase(),
            repo: job.repo.clone(),
            name: job.name.clone(),
            workflow_name: job.workflow_name.clone(),
            labels: job.labels.clone(),
            conclusion: FleetJobConclusion::from_github(job.conclusion.as_deref()),
            runner_name: job.runner_name.clone(),
            os: job
                .labels
                .iter()
                .find_map(|l| FleetJobOs::parse(l))
                .or_else(|| runner.and_then(|r| FleetJobOs::parse(&r.os))),
            created_at: job.created_at,
            started_at: job.started_at,
            completed_at: job.completed_at?,
            html_url: job.html_url.clone(),
        })
    }
}

/// A job is self-hosted if it asks for `self-hosted`, ran on one of the organization's
/// runners, or its labels are compatible with one of them.
pub(crate) fn is_self_hosted(job: &GhJob, runners: &[GhRunner]) -> bool {
    labels::targets_self_hosted(&job.labels)
        || job.runner_id.is_some_and(|id| runners.iter().any(|r| r.id == id))
        || runners.iter().any(|r| labels::is_compatible(&job.labels, &r.labels))
}

/// The Activity page: one summary per window, the latest jobs, and storage health.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct FleetActivity {
    /// When recording began (within the 30-day retention).
    pub recording_since: Option<DateTime<Utc>>,
    pub storage_error: Option<String>,
    /// The 24 hours, 7 days and 30 days up to now.
    pub windows: Vec<FleetActivityWindow>,
    /// The most recently finished jobs, newest first.
    pub recent: Vec<FleetFinishedJob>,
    /// The date the GitHub-hosted rates were checked, `YYYY-MM-DD`.
    pub rates_as_of: String,
}

/// Totals for jobs that finished in the last `days` days.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct FleetActivityWindow {
    pub days: u32,
    /// Time in the window the app was running with complete job scans.
    pub recorded_secs: u32,
    pub jobs: u32,
    pub succeeded: u32,
    pub failed: u32,
    pub runner_secs: u32,
    /// Nearest-rank percentiles of time from creation to start.
    pub queue_median_secs: Option<u32>,
    pub queue_p95_secs: Option<u32>,
    pub machines: Vec<FleetMachineBusy>,
    /// Repositories by runner time, at most five.
    pub repos: Vec<FleetRepoTime>,
    /// What the same minutes would cost on GitHub-hosted runners, per OS.
    pub hosted: Vec<FleetHostedCost>,
}

/// Share of a machine's runner capacity spent on jobs, over recorded time only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct FleetMachineBusy {
    pub machine_id: String,
    pub name: String,
    /// Runners GitHub lists on this machine now.
    pub runners: u32,
    pub busy_secs: u32,
    /// `busy_secs / (runners × recorded_secs)`; null without recorded time.
    pub busy_percent: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct FleetRepoTime {
    pub repo: String,
    pub jobs: u32,
    pub runner_secs: u32,
}

/// Job minutes (each job rounded up, as GitHub bills) at a fixed list rate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct FleetHostedCost {
    pub os: FleetJobOs,
    pub minutes: u32,
    pub usd_per_minute: f64,
    pub usd: f64,
}

#[cfg(test)]
mod tests;
