//! Snapshot read routes. Filters never start network work.

use axum::extract::{Path, State};
use axum::{Extension, Json};
use serde_json::{Value, json};

use super::checks::{CHECK_RETENTION, MAX_LOG_BYTES, MAX_LOG_LINES, MAX_RETAINED_CHECKS};
use super::common::{ApiError, ApiState, Snapshot};
use super::queries::{self, NoQuery, Parameters};
use super::{CONCURRENT_LOGS, DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE};
use crate::model::DataSource;
use crate::monitoring::{EVENT_PAGE_SIZE, EVENT_RETENTION, MAX_EVENTS};

type Reply = Result<Json<Value>, ApiError>;

pub(super) async fn index(Extension(s): Extension<Snapshot>, Parameters(_): Parameters<NoQuery>) -> Json<Value> {
    Json(json!({
        "name": "Runner Dashboard local API", "version": env!("CARGO_PKG_VERSION"), "readOnly": true,
        "capabilities": { "schema": "/v1/openapi.json", "conditionalGet": true, "events": true, "issues": true,
            "checks": ["refresh", "inventory", "doctor"], "logs": true, "runnerMutations": false },
        "limits": { "defaultPageSize": DEFAULT_PAGE_SIZE, "maxPageSize": MAX_PAGE_SIZE, "defaultEventPageSize": EVENT_PAGE_SIZE,
            "eventRetentionDays": EVENT_RETENTION.num_days(), "maxEvents": MAX_EVENTS, "maxLogLines": MAX_LOG_LINES,
            "maxLogBytes": MAX_LOG_BYTES, "concurrentChecks": 1, "retainedChecks": MAX_RETAINED_CHECKS,
            "checkRetentionMinutes": CHECK_RETENTION.num_minutes(), "concurrentLogs": CONCURRENT_LOGS },
        "endpoints": super::schema::endpoint_descriptions(),
        "semantics": {
            "source": "demo means fictional data; on-demand checks and logs require live mode.",
            "stale": "Last known observations, not current facts. Missing data never means zero or recovered.",
            "activity": "busy | idle | offline | unknown; busy and idle require fresh GitHub data.",
            "currentJob": "Only populated when GitHub links the job to the runner; repo filters on runners inspect this field.",
            "machines": "Default scope=all includes fleet and other discovered machines. Ignored peers remain in /v1/discovered. Summary machine counts refer to fleet machines.",
            "identifiers": "Exact IDs take precedence, followed by inventory aliases, then names. Ambiguous names return 409 with candidate IDs.",
            "pagination": "Lists are sorted by stable ID with exclusive after and nextCursor. Events are newest first with exclusive before. Restart list pagination if revision changes.",
            "revision": "Opaque fingerprint of the fleet snapshot excluding generatedAt. ETags describe each response. Poll /v1/changes?since=revision or use If-None-Match on GET.",
            "checks": "POST starts a bounded read-only job. Poll its Location until completed or failed. completed means reads finished, not that the fleet is healthy; inspect result.issues and result.errors.",
            "attention": "summary.attention equals issues.length, including unresolved stale or muted alerts.",
            "history": format!(
                "Observed machine transitions only; no completed-job archive. Retention is at most {} days and {MAX_EVENTS} events.",
                EVENT_RETENTION.num_days()
            ),
            "scope": s.view.source,
        }
    }))
}

pub(super) async fn summary(Extension(s): Extension<Snapshot>, Parameters(_): Parameters<NoQuery>) -> Json<Value> {
    Json(json!({ "summary": s.view.summary, "orgs": s.view.orgs, "tailnet": s.view.tailnet }))
}
pub(super) async fn fleet(Extension(s): Extension<Snapshot>, Parameters(_): Parameters<NoQuery>) -> Reply {
    serde_json::to_value(&*s.view)
        .map(Json)
        .map_err(|e| ApiError::internal(e.to_string()))
}
pub(super) async fn monitoring(
    Extension(s): Extension<Snapshot>,
    Parameters(q): Parameters<queries::MachineFilter>,
) -> Reply {
    if let Some(machine) = q.machine {
        Ok(Json(
            json!({ "monitoring": queries::resolve_machine(&s.view, &machine)?.monitoring }),
        ))
    } else {
        Ok(Json(json!({ "monitoring": s.view.monitoring })))
    }
}

pub(super) async fn machines(
    Extension(s): Extension<Snapshot>,
    Parameters(q): Parameters<queries::MachineQuery>,
) -> Reply {
    let limit = queries::limit(q.limit, DEFAULT_PAGE_SIZE)?;
    let machines = match q.scope {
        queries::MachineScope::All => s.view.machines.iter().chain(&s.view.other_machines).collect::<Vec<_>>(),
        queries::MachineScope::Fleet => s.view.machines.iter().collect(),
        queries::MachineScope::Discovered => s.view.other_machines.iter().collect(),
    };
    let machines = machines
        .into_iter()
        .filter(|m| {
            q.os.is_none_or(|os| m.os == Some(os))
                && q.connection.is_none_or(|c| c.matches(&m.connection))
                && q.q.as_ref().is_none_or(|query| {
                    [m.id.as_str(), &m.display_name, &m.host]
                        .iter()
                        .any(|v| v.to_lowercase().contains(&query.to_lowercase()))
                })
        })
        .collect();
    let (machines, total, cursor) = queries::page(machines, limit, q.after.as_deref(), |m| m.id.clone());
    Ok(Json(
        json!({ "scope": q.scope, "machines": machines, "total": total, "nextCursor": cursor }),
    ))
}

pub(super) async fn machine(
    Extension(s): Extension<Snapshot>,
    Path(id): Path<String>,
    Parameters(_): Parameters<NoQuery>,
) -> Reply {
    serde_json::to_value(queries::resolve_machine(&s.view, &id)?)
        .map(Json)
        .map_err(|e| ApiError::internal(e.to_string()))
}

pub(super) async fn runners(
    Extension(s): Extension<Snapshot>,
    Parameters(q): Parameters<queries::RunnerQuery>,
) -> Reply {
    let limit = queries::limit(q.limit, DEFAULT_PAGE_SIZE)?;
    let machine = q
        .machine
        .as_ref()
        .map(|id| queries::resolve_machine(&s.view, id))
        .transpose()?;
    let runners = queries::flat_runners(&s.view)
        .into_iter()
        .filter(|flat| {
            let r = &flat.runner;
            machine.is_none_or(|m| r.machine_id.as_ref() == Some(&m.id))
                && queries::matches(r.org.as_deref(), q.org.as_deref())
                && q.activity.is_none_or(|a| a.matches(&r.activity))
                && q.label.as_ref().is_none_or(|label| {
                    r.github
                        .as_ref()
                        .is_some_and(|g| g.labels.iter().any(|l| l.eq_ignore_ascii_case(label)))
                })
                && queries::matches(r.current_job.as_ref().map(|j| j.repo.as_str()), q.repo.as_deref())
        })
        .collect();
    let (runners, total, cursor) = queries::page(runners, limit, q.after.as_deref(), |r| r.runner.key.clone());
    Ok(Json(
        json!({ "runners": runners, "total": total, "nextCursor": cursor }),
    ))
}

pub(super) async fn runner(
    Extension(s): Extension<Snapshot>,
    Path(key): Path<String>,
    Parameters(q): Parameters<queries::OrgQuery>,
) -> Reply {
    serde_json::to_value(queries::resolve_runner(&s.view, &key, q.org.as_deref())?)
        .map(Json)
        .map_err(|e| ApiError::internal(e.to_string()))
}

pub(super) async fn queue(Extension(s): Extension<Snapshot>, Parameters(q): Parameters<queries::QueueQuery>) -> Reply {
    let limit = queries::limit(q.limit, DEFAULT_PAGE_SIZE)?;
    let jobs = s
        .view
        .queue
        .jobs
        .iter()
        .filter(|job| {
            queries::matches(Some(&job.org), q.org.as_deref())
                && queries::matches(Some(&job.job.repo), q.repo.as_deref())
                && q.label
                    .as_ref()
                    .is_none_or(|label| job.job.labels.iter().any(|l| l.eq_ignore_ascii_case(label)))
        })
        .cloned()
        .collect();
    let (jobs, total, cursor) = queries::page(jobs, limit, q.after.as_deref(), |j| {
        format!("{}:{:020}", j.org, j.job.id)
    });
    let mut queue = s.view.queue.clone();
    queue.jobs = jobs;
    Ok(Json(json!({ "queue": queue, "total": total, "nextCursor": cursor })))
}

pub(super) async fn discovered(Extension(s): Extension<Snapshot>, Parameters(_): Parameters<NoQuery>) -> Json<Value> {
    Json(json!({ "otherMachines": s.view.other_machines, "ignored": s.view.ignored }))
}

pub(super) async fn desired(
    Extension(s): Extension<Snapshot>,
    Parameters(q): Parameters<queries::MachineFilter>,
) -> Reply {
    let selected = q
        .machine
        .as_ref()
        .map(|id| queries::resolve_machine(&s.view, id))
        .transpose()?;
    let hosts: Vec<_> = s.view.machines.iter().chain(&s.view.other_machines)
        .filter(|m| selected.is_none_or(|s| s.id == m.id))
        .filter_map(|m| m.desired.as_ref().map(|d| json!({ "machineId": m.id, "machineKey": m.preferences_key(),
            "machineName": m.display_name, "desired": d,
            "runners": m.runners.iter().filter_map(|r| r.desired.as_ref().map(|d| json!({"key": r.key, "name": r.name, "desired": d}))).collect::<Vec<_>>()
        }))).collect();
    Ok(Json(json!({ "plan": s.view.plan, "hosts": hosts })))
}

pub(super) async fn events(
    State(state): State<ApiState>,
    Extension(s): Extension<Snapshot>,
    Parameters(q): Parameters<queries::EventQuery>,
) -> Reply {
    let limit = queries::limit(q.limit, EVENT_PAGE_SIZE)?;
    if q.machine.is_some() && q.machine_key.is_some() {
        return Err(ApiError::invalid("Use either machine or machineKey, not both."));
    }
    if q.before
        .as_ref()
        .is_some_and(|b| b.len() != 20 || !b.bytes().all(|c| c.is_ascii_digit()))
    {
        return Err(ApiError::invalid(
            "before must be a 20-digit event ID returned by this API.",
        ));
    }
    let key = q
        .machine
        .as_ref()
        .map(|id| queries::resolve_machine(&s.view, id).map(crate::model::MachineView::preferences_key))
        .transpose()?
        .or(q.machine_key);
    let mut events = if s.view.source == DataSource::Demo {
        s.view
            .monitoring
            .events
            .iter()
            .filter(|e| key.as_ref().is_none_or(|k| e.machine_key == *k) && q.before.as_ref().is_none_or(|b| e.id < *b))
            .take(limit + 1)
            .cloned()
            .collect()
    } else {
        state
            .engine
            .monitoring_event_page(key.as_deref(), q.before.as_deref(), limit + 1)
    };
    let more = events.len() > limit;
    events.truncate(limit);
    let cursor = more.then(|| events.last().map(|e| e.id.clone())).flatten();
    Ok(Json(
        json!({ "events": events, "hasMore": more, "nextCursor": cursor, "retentionDays": EVENT_RETENTION.num_days(), "maxEvents": MAX_EVENTS }),
    ))
}

pub(super) async fn issues(Extension(s): Extension<Snapshot>, Parameters(q): Parameters<queries::IssueQuery>) -> Reply {
    let limit = queries::limit(q.limit, DEFAULT_PAGE_SIZE)?;
    let machine = q
        .machine
        .as_ref()
        .map(|id| queries::resolve_machine(&s.view, id))
        .transpose()?;
    let issues = s
        .view
        .issues
        .iter()
        .filter(|issue| {
            machine.is_none_or(|m| issue.machine_id.as_ref() == Some(&m.id))
                && q.category.is_none_or(|c| issue.category == c)
                && q.stale.is_none_or(|v| issue.stale == v)
                && q.muted.is_none_or(|v| issue.muted == v)
        })
        .collect();
    let (issues, total, cursor) = queries::page(issues, limit, q.after.as_deref(), |i| i.id.clone());
    Ok(Json(json!({ "issues": issues, "total": total, "nextCursor": cursor })))
}

pub(super) async fn changes(
    Extension(s): Extension<Snapshot>,
    Parameters(q): Parameters<queries::ChangesQuery>,
) -> Json<Value> {
    Json(json!({ "changed": q.since.as_ref() != Some(&s.revision) }))
}
