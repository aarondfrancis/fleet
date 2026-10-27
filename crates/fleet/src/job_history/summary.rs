//! Pure summaries of the journal for the Activity page.

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};

use super::store::Span;
use super::{
    FleetActivity, FleetActivityWindow, FleetFinishedJob, FleetHostedCost, FleetJobConclusion, FleetJobOs,
    FleetMachineBusy, FleetRepoTime,
};
use crate::model::{FleetView, count_u32};

const WINDOW_DAYS: [u32; 3] = [1, 7, 30];
const RECENT_JOBS: usize = 20;
const TOP_REPOS: usize = 5;
/// When [`HOSTED_RATES`] were checked against GitHub's "Actions runner pricing" page.
const RATES_AS_OF: &str = "2026-10-27";
/// GitHub's standard hosted-runner list prices in USD per minute (Linux and Windows 2-core
/// x64, macOS 3/4-core). Jobs without a known OS are priced as Linux.
const HOSTED_RATES: [(FleetJobOs, f64); 3] = [
    (FleetJobOs::Linux, 0.006),
    (FleetJobOs::Windows, 0.010),
    (FleetJobOs::Macos, 0.062),
];

fn secs_u32(secs: i64) -> u32 {
    u32::try_from(secs.max(0)).unwrap_or(u32::MAX)
}

/// Seconds of `spans` within `[from, to)`. `spans` must be sorted and non-overlapping.
pub(super) fn recorded_overlap(spans: &[Span], from: DateTime<Utc>, to: DateTime<Utc>) -> i64 {
    let first = spans.partition_point(|s| s.to <= from);
    spans[first..]
        .iter()
        .take_while(|s| s.from < to)
        .map(|s| (s.to.min(to) - s.from.max(from)).num_seconds().max(0))
        .sum()
}

/// The nearest-rank `p`th percentile of ascending `sorted`.
pub(super) fn percentile(sorted: &[u32], p: usize) -> Option<u32> {
    let rank = (sorted.len() * p).div_ceil(100).max(1);
    sorted.get(rank - 1).copied()
}

/// Windows end now; time the app wasn't recording only lowers their recorded time.
pub(super) fn activity(
    jobs: &[FleetFinishedJob],
    spans: &[Span],
    view: &FleetView,
    now: DateTime<Utc>,
) -> FleetActivity {
    FleetActivity {
        recording_since: spans.first().map(|s| s.from),
        storage_error: None,
        windows: WINDOW_DAYS
            .iter()
            .map(|&days| window(jobs, spans, view, now, days))
            .collect(),
        recent: jobs.iter().rev().take(RECENT_JOBS).cloned().collect(),
        rates_as_of: RATES_AS_OF.into(),
    }
}

fn window(
    jobs: &[FleetFinishedJob],
    spans: &[Span],
    view: &FleetView,
    end: DateTime<Utc>,
    days: u32,
) -> FleetActivityWindow {
    let start = end - Duration::days(i64::from(days));
    let jobs: Vec<&FleetFinishedJob> = jobs
        .iter()
        .filter(|j| (start..=end).contains(&j.completed_at))
        .collect();
    let recorded = recorded_overlap(spans, start, end);

    // Runners GitHub lists now, by (org, name), and how many each machine has.
    let mut runner_machine: HashMap<(String, String), usize> = HashMap::new();
    let mut machines: Vec<FleetMachineBusy> = Vec::new();
    for machine in &view.machines {
        let listed: Vec<_> = machine.runners.iter().filter(|r| r.github.is_some()).collect();
        if listed.is_empty() {
            continue;
        }
        for runner in &listed {
            let org = runner.org.as_deref().unwrap_or_default().to_lowercase();
            runner_machine.insert((org, runner.name.to_lowercase()), machines.len());
        }
        machines.push(FleetMachineBusy {
            machine_id: machine.id.clone(),
            name: machine.display_name.clone(),
            runners: count_u32(listed.len()),
            busy_secs: 0,
            busy_percent: None,
        });
    }

    let mut busy = vec![0i64; machines.len()];
    let mut repos: HashMap<&str, (u32, i64)> = HashMap::new();
    let mut minutes: HashMap<FleetJobOs, u32> = HashMap::new();
    let mut waits = Vec::new();
    let mut runner_secs = 0;
    for job in &jobs {
        let run = job.run_secs();
        runner_secs += run;
        let repo = repos.entry(&job.repo).or_default();
        repo.0 += 1;
        repo.1 += run;
        let Some(started) = job.started_at else { continue };
        *minutes.entry(job.os.unwrap_or(FleetJobOs::Linux)).or_default() += secs_u32((run + 59) / 60);
        if let Some(created) = job.created_at {
            waits.push(secs_u32((started - created).num_seconds()));
        }
        if let Some(name) = &job.runner_name
            && let Some(&i) = runner_machine.get(&(job.org.clone(), name.to_lowercase()))
        {
            busy[i] += recorded_overlap(spans, started.max(start), job.completed_at.min(end));
        }
    }
    for (machine, secs) in machines.iter_mut().zip(busy) {
        machine.busy_secs = secs_u32(secs);
        let capacity = i64::from(machine.runners) * recorded;
        machine.busy_percent = (capacity > 0).then(|| (secs as f64 / capacity as f64 * 100.0).min(100.0));
    }
    waits.sort_unstable();
    let mut repos: Vec<FleetRepoTime> = repos
        .into_iter()
        .map(|(repo, (jobs, secs))| FleetRepoTime {
            repo: repo.into(),
            jobs,
            runner_secs: secs_u32(secs),
        })
        .collect();
    repos.sort_by(|a, b| b.runner_secs.cmp(&a.runner_secs).then_with(|| a.repo.cmp(&b.repo)));
    repos.truncate(TOP_REPOS);

    FleetActivityWindow {
        days,
        recorded_secs: secs_u32(recorded),
        jobs: count_u32(jobs.len()),
        succeeded: count_u32(
            jobs.iter()
                .filter(|j| j.conclusion == FleetJobConclusion::Success)
                .count(),
        ),
        failed: count_u32(
            jobs.iter()
                .filter(|j| j.conclusion == FleetJobConclusion::Failure)
                .count(),
        ),
        runner_secs: secs_u32(runner_secs),
        queue_median_secs: percentile(&waits, 50),
        queue_p95_secs: percentile(&waits, 95),
        machines,
        repos,
        hosted: HOSTED_RATES
            .iter()
            .map(|&(os, rate)| {
                let minutes = minutes.get(&os).copied().unwrap_or(0);
                FleetHostedCost {
                    os,
                    minutes,
                    usd_per_minute: rate,
                    usd: f64::from(minutes) * rate,
                }
            })
            .collect(),
    }
}
