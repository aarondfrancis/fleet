//! Fictional job history for previews. Never reads or writes the live journal.

use chrono::{DateTime, Duration, Utc};

use super::store::Span;
use super::{FleetFinishedJob, FleetJobConclusion, FleetJobOs};
use crate::model::FleetView;

const REPOS: [&str; 4] = ["acme-demo/web", "acme-demo/ios-app", "acme-demo/api", "acme-demo/infra"];
const WORKFLOWS: [&str; 3] = ["CI", "Release", "Nightly"];

/// 30 days of 14-hour recorded days, with the app closed for two of them.
pub(crate) fn attach(view: &mut FleetView, now: DateTime<Utc>) {
    let spans: Vec<Span> = (0..30i64)
        .rev()
        .filter(|day| !matches!(day, 9 | 10))
        .map(|day| {
            let to = now - Duration::days(day);
            Span {
                from: to - Duration::hours(14),
                to,
            }
        })
        .collect();
    let runners: Vec<_> = view
        .machines
        .iter()
        .flat_map(|m| &m.runners)
        .filter_map(|r| Some((r.org.clone()?, r.name.clone(), r.github.as_ref()?)))
        .collect();
    let mut jobs = Vec::new();
    for (day, span) in (0u64..).zip(&spans) {
        for (index, (org, runner, github)) in (0u64..).zip(&runners) {
            let count = 4 + (index * 3 + day) % 9;
            for k in 0..count {
                let seed = day * 97 + index * 31 + k * 7;
                let wait = 10 + seed * 13 % 120 + if seed % 9 == 0 { 600 } else { 0 };
                let created = span.from + Duration::minutes(i64::try_from(k * 840 / count + seed % 30).unwrap_or(0));
                let started = created + Duration::seconds(i64::try_from(wait).unwrap_or(0));
                // Some runners take longer jobs; the cap (after up to 41 minutes of jitter and wait) avoids overlaps.
                let minutes = ((5 + seed % 25) * (1 + index % 3)).min(840 / count - 45);
                let completed = started + Duration::minutes(i64::try_from(minutes).unwrap_or(0));
                if completed > span.to {
                    continue;
                }
                let id = 50_000 + u64::try_from(jobs.len()).unwrap_or(0);
                let repo = REPOS[usize::try_from(seed % 4).unwrap_or(0)];
                jobs.push(FleetFinishedJob {
                    id,
                    org: org.clone(),
                    repo: repo.into(),
                    name: if k % 2 == 0 { "build" } else { "test" }.into(),
                    workflow_name: Some(WORKFLOWS[usize::try_from(seed % 3).unwrap_or(0)].into()),
                    labels: github.labels.iter().take(2).cloned().collect(),
                    conclusion: match seed {
                        s if s % 13 == 0 => FleetJobConclusion::Failure,
                        s if s % 29 == 0 => FleetJobConclusion::Cancelled,
                        _ => FleetJobConclusion::Success,
                    },
                    runner_name: Some(runner.clone()),
                    os: FleetJobOs::parse(&github.os),
                    created_at: Some(created),
                    started_at: Some(started),
                    completed_at: completed,
                    html_url: Some(format!("https://github.com/{repo}/actions/runs/{}/job/{id}", id * 10)),
                });
            }
        }
    }
    jobs.sort_by_key(|j| (j.completed_at, j.id));
    view.activity = super::summary::activity(&jobs, &spans, view, now);
}
