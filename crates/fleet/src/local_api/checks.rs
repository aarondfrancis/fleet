//! Bounded asynchronous read-only checks and exact inventory-runner log access.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, State, rejection::JsonRejection};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::CONCURRENT_LOGS;
use super::common::{ApiError, ApiState, ErrorBody, ErrorCode, Snapshot};
use super::queries::{self, NoQuery, Parameters};
use crate::engine::Engine;
use crate::fleet_plan::DesiredRunnerRole;
use crate::issues::FleetIssue;
use crate::model::{DataSource, MachineConnection, inventory_machine_key};
use crate::sync::MutexExt;

pub(super) const CHECK_RETENTION: Duration = Duration::hours(1);
pub(super) const MAX_RETAINED_CHECKS: usize = 100;
pub(super) const DEFAULT_LOG_LINES: u32 = 200;
pub(super) const MAX_LOG_LINES: u32 = 10_000;
pub(super) const MAX_LOG_BYTES: usize = 1_048_576;

#[derive(Debug, Clone, Copy, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(super) enum CheckKind {
    Refresh,
    Inventory,
    Doctor,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct CheckRequest {
    pub kind: CheckKind,
    /// Exact machine ID, inventory alias, or unambiguous name; omit for all.
    pub machine: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(super) enum CheckStatus {
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(super) struct CheckResult {
    pub source: DataSource,
    pub generated_at: DateTime<Utc>,
    pub revision: String,
    pub issues: Vec<FleetIssue>,
    pub errors: Vec<String>,
    pub skipped: Vec<String>,
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(super) struct CheckJob {
    pub id: String,
    pub source: DataSource,
    pub request: CheckRequest,
    pub status: CheckStatus,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub result: Option<CheckResult>,
    pub error: Option<ErrorBody>,
}

#[derive(Debug, Default)]
pub(super) struct CheckStore(Mutex<VecDeque<CheckJob>>);
impl CheckStore {
    fn begin(&self, request: CheckRequest) -> Result<CheckJob, ApiError> {
        let mut jobs = self.0.locked();
        if jobs.iter().any(|job| job.status == CheckStatus::Running) {
            return Err(ApiError::busy(
                "A check is already running. Poll its Location or try again after it finishes.",
            ));
        }
        jobs.retain(|job| job.started_at > Utc::now() - CHECK_RETENTION);
        while jobs.len() >= MAX_RETAINED_CHECKS {
            jobs.pop_front();
        }
        let started_at = Utc::now();
        let job = CheckJob {
            id: format!("{}-{}", std::process::id(), started_at.format("%Y%m%dT%H%M%S%.9f")),
            source: DataSource::Live,
            request,
            status: CheckStatus::Running,
            started_at,
            finished_at: None,
            result: None,
            error: None,
        };
        jobs.push_back(job.clone());
        Ok(job)
    }
    fn finish(&self, id: &str, result: Result<CheckResult, ApiError>) {
        let mut jobs = self.0.locked();
        if let Some(job) = jobs.iter_mut().find(|job| job.id == id) {
            job.finished_at = Some(Utc::now());
            match result {
                Ok(result) => {
                    job.result = Some(result);
                    job.status = CheckStatus::Completed;
                }
                Err(error) => {
                    job.error = Some(error.body);
                    job.status = CheckStatus::Failed;
                }
            }
        }
    }
    fn find(&self, id: &str) -> Result<CheckJob, ApiError> {
        self.0
            .locked()
            .iter()
            .find(|job| {
                job.id == id && (job.status == CheckStatus::Running || job.started_at > Utc::now() - CHECK_RETENTION)
            })
            .cloned()
            .ok_or_else(|| {
                ApiError::not_found(format!(
                    "No retained check has this ID. Checks expire after {} minutes or {MAX_RETAINED_CHECKS} newer checks, and on restart.",
                    CHECK_RETENTION.num_minutes()
                ))
            })
    }
}

fn require_live(source: DataSource) -> Result<(), ApiError> {
    if source == DataSource::Demo {
        Err(ApiError::new(
            StatusCode::CONFLICT,
            ErrorCode::DemoMode,
            "Turn off demo data before requesting live checks or logs.",
        ))
    } else {
        Ok(())
    }
}

fn unsupported(message: &str) -> ApiError {
    ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, ErrorCode::UnsupportedTarget, message)
}
fn failed(error: impl std::fmt::Display) -> ApiError {
    ApiError::new(StatusCode::BAD_GATEWAY, ErrorCode::CheckFailed, error.to_string())
}

pub(super) async fn start(
    State(state): State<ApiState>,
    Extension(snapshot): Extension<Snapshot>,
    Parameters(_): Parameters<NoQuery>,
    body: Result<Json<CheckRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    require_live(snapshot.view.source)?;
    let Json(mut request) = body.map_err(|e| ApiError::new(e.status(), ErrorCode::InvalidRequest, e.body_text()))?;
    let machine = request
        .machine
        .as_ref()
        .map(|id| queries::resolve_machine(&snapshot.view, id))
        .transpose()?;
    let host = machine.and_then(|m| m.desired.as_ref()).map(|d| d.host.id.clone());
    match request.kind {
        CheckKind::Inventory | CheckKind::Doctor if machine.is_some() && host.is_none() => {
            return Err(unsupported("Inventory and doctor checks require an inventory machine."));
        }
        CheckKind::Refresh if machine.is_some_and(|m| matches!(m.connection, MachineConnection::Disabled)) => {
            return Err(unsupported("Automatic SSH observations are disabled for this machine."));
        }
        CheckKind::Refresh | CheckKind::Inventory | CheckKind::Doctor => {}
    }
    request.machine = machine.map(crate::model::MachineView::preferences_key);
    let job = state.checks.begin(request.clone())?;
    let id = job.id.clone();
    let engine = state.engine;
    let checks = state.checks;
    // The task belongs to the API, not the requesting connection: a disconnected
    // client can reconnect and poll. Each SSH command, GitHub request and local
    // CLI call the check makes has its own time limit.
    tokio::spawn(async move {
        let result = tokio::spawn(run(engine, request, host))
            .await
            .map_err(|e| failed(format!("The check stopped unexpectedly: {e}")))
            .and_then(|result| result);
        checks.finish(&id, result);
    });
    Ok((
        StatusCode::ACCEPTED,
        [("location", format!("/v1/checks/{}", job.id))],
        Json(json!({ "check": job })),
    )
        .into_response())
}

async fn run(engine: Arc<Engine>, request: CheckRequest, host: Option<String>) -> Result<CheckResult, ApiError> {
    require_live(if engine.config().demo_mode {
        DataSource::Demo
    } else {
        DataSource::Live
    })?;
    match request.kind {
        CheckKind::Refresh => {
            engine.poll_tailscale().await;
            if request.machine.is_some() {
                engine.poll_machines_wait(request.machine.as_deref()).await;
            } else {
                tokio::join!(
                    engine.poll_machines_wait(None),
                    engine.poll_github_runners(),
                    engine.poll_github_jobs()
                );
            }
        }
        CheckKind::Inventory => engine.desired_refresh_wait(host).await.map_err(failed)?,
        CheckKind::Doctor => engine.desired_doctor_wait(host).await.map_err(failed)?,
    }
    let snapshot = Snapshot::read(engine).await?;
    require_live(snapshot.view.source)?;
    let view = &snapshot.view;
    let selected = request
        .machine
        .as_ref()
        .map(|id| queries::resolve_machine(view, id))
        .transpose()?;
    let issues = view
        .issues
        .iter()
        .filter(|i| selected.is_none_or(|m| i.machine_id.as_ref() == Some(&m.id)))
        .cloned()
        .collect();
    let mut errors = Vec::new();
    let mut skipped = Vec::new();
    match request.kind {
        CheckKind::Refresh => {
            errors.extend(view.tailnet.error.iter().cloned());
            if request.machine.is_none() {
                errors.extend(view.orgs.iter().filter_map(|org| org.error.clone()));
            }
            for machine in view
                .machines
                .iter()
                .chain(&view.other_machines)
                .filter(|m| selected.is_none_or(|s| s.id == m.id))
            {
                match &machine.connection {
                    MachineConnection::Error { detail } => errors.push(format!("{}: {detail}", machine.id)),
                    MachineConnection::Stale { reason, .. } => errors.push(format!("{}: {reason}", machine.id)),
                    MachineConnection::Offline { .. } | MachineConnection::Disabled | MachineConnection::Pending => {
                        skipped.push(machine.id.clone())
                    }
                    MachineConnection::Connected { .. } => {}
                }
            }
        }
        CheckKind::Inventory => {
            errors.extend(view.plan.github_error.iter().cloned());
            errors.extend(
                view.machines
                    .iter()
                    .filter(|m| selected.is_none_or(|s| s.id == m.id))
                    .filter_map(|m| m.desired.as_ref().and_then(|d| d.error.clone())),
            );
        }
        CheckKind::Doctor => {
            if let Some(error) = &view.plan.checks_error {
                return Err(failed(error));
            }
            errors.extend(
                view.plan
                    .control_checks
                    .iter()
                    .chain(
                        view.machines
                            .iter()
                            .filter(|m| selected.is_none_or(|s| s.id == m.id))
                            .filter_map(|m| m.desired.as_ref())
                            .flat_map(|d| &d.checks),
                    )
                    .filter(|c| c.status == crate::desired::model::CheckStatus::Fail)
                    .map(|c| format!("{}: {}: {}", c.host, c.check, c.detail)),
            );
        }
    }
    Ok(CheckResult {
        source: view.source,
        generated_at: view.generated_at,
        revision: snapshot.revision,
        issues,
        errors,
        skipped,
    })
}

pub(super) async fn status(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Parameters(_): Parameters<NoQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(json!({ "check": state.checks.find(&id)? })))
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct LogQuery {
    pub runner: String,
    pub org: Option<String>,
    pub lines: Option<u32>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(super) struct LogRead {
    pub runner_key: String,
    pub machine_key: String,
    pub requested_lines: u32,
    pub read_at: DateTime<Utc>,
    pub text: String,
    pub truncated: bool,
}

pub(super) async fn logs(
    State(state): State<ApiState>,
    Extension(snapshot): Extension<Snapshot>,
    Parameters(query): Parameters<LogQuery>,
) -> Result<Json<LogRead>, ApiError> {
    require_live(snapshot.view.source)?;
    let lines = query.lines.unwrap_or(DEFAULT_LOG_LINES);
    if !(1..=MAX_LOG_LINES).contains(&lines) {
        return Err(ApiError::invalid(format!(
            "lines must be between 1 and {MAX_LOG_LINES}."
        )));
    }
    let runner = queries::resolve_runner(&snapshot.view, &query.runner, query.org.as_deref())?.runner;
    let desired = runner
        .desired
        .as_ref()
        .filter(|d| !matches!(d.role, DesiredRunnerRole::Undeclared))
        .ok_or_else(|| {
            unsupported("Logs require an exact runner declared in the inventory, or its retained legacy runner.")
        })?;
    let _permit = state.logs.try_acquire().map_err(|_| {
        ApiError::busy(format!(
            "{CONCURRENT_LOGS} log reads are already running. Try again shortly."
        ))
    })?;
    let mut text = state.engine.desired_logs(runner.name, lines).await.map_err(failed)?;
    let truncated = text.len() > MAX_LOG_BYTES;
    if truncated {
        let mut offset = text.len() - MAX_LOG_BYTES;
        while !text.is_char_boundary(offset) {
            offset += 1;
        }
        text.drain(..offset);
    }
    Ok(Json(LogRead {
        runner_key: runner.key,
        machine_key: inventory_machine_key(&desired.host_id),
        requested_lines: lines,
        read_at: Utc::now(),
        text,
        truncated,
    }))
}
